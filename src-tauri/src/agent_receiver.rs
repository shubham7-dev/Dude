use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncReadExt;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::sync::oneshot;

pub const PIPE_NAME: &str = r"\\.\pipe\dude-agent-events";
const MAX_PAYLOAD_SIZE: usize = 65536; // 64 KB

static LAST_AGENT_EVENT: Mutex<Option<(String, Instant)>> = Mutex::new(None);

use std::io::Write;

#[cfg(debug_assertions)]
fn log_receiver(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\ProgramData\Dude\hooks\dude-receiver.log")
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = writeln!(f, "[{}][PID:{}] {}", now, std::process::id(), msg);
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn log_receiver(_msg: &str) {}

pub type ApprovalSender = oneshot::Sender<String>;
static PENDING_APPROVALS: Mutex<Option<HashMap<String, (String, ApprovalSender)>>> = Mutex::new(None);

pub fn register_pending_approval(id: String, conv_id: String, tx: ApprovalSender) {
    let mut lock = PENDING_APPROVALS.lock().unwrap();
    if lock.is_none() {
        *lock = Some(HashMap::new());
    }
    if let Some(ref mut map) = *lock {
        map.insert(id, (conv_id, tx));
    }
}

pub fn resolve_pending_approval(id: &str, choice: String) -> bool {
    log_receiver(&format!("RESOLVE_CALLED id={} choice={}", id, choice));
    let mut lock = PENDING_APPROVALS.lock().unwrap();
    if let Some(ref mut map) = *lock {
        if let Some((_, tx)) = map.remove(id) {
            let _ = tx.send(choice);
            return true;
        }
    }
    false
}

pub fn dismiss_pending_approvals_for_conversation(conv_id: &str) {
    let mut lock = PENDING_APPROVALS.lock().unwrap();
    if let Some(ref mut map) = *lock {
        let matching_ids: Vec<String> = map
            .iter()
            .filter(|(_, (c, _))| c == conv_id)
            .map(|(k, _)| k.clone())
            .collect();

        for id in matching_ids {
            if let Some((_, tx)) = map.remove(&id) {
                let _ = tx.send("deny".to_string());
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AgentLastEventInfo {
    pub event: String,
    pub seconds_ago: u64,
}

pub fn get_last_agent_event() -> Option<AgentLastEventInfo> {
    let lock = LAST_AGENT_EVENT.lock().unwrap();
    lock.as_ref().map(|(ev, inst)| AgentLastEventInfo {
        event: ev.clone(),
        seconds_ago: inst.elapsed().as_secs(),
    })
}

#[derive(serde::Deserialize, Debug)]
pub struct RawClaudeHookEvent {
    pub hook_event_name: Option<String>,
    #[serde(alias = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub prompt: Option<String>,
    pub message: Option<String>,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct AgentEvent {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub agent: String,
    pub project: String,
    pub kind: String,
    pub tool: Option<String>,
    pub label: String,
    pub ts: u64,
}

fn extract_project(cwd: Option<&str>) -> String {
    match cwd {
        Some(c) => {
            let trimmed = c.trim().trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                "unknown".to_string()
            } else {
                Path::new(trimmed)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(trimmed)
                    .to_string()
            }
        }
        None => "unknown".to_string(),
    }
}

fn extract_file_or_pattern(tool_input: &serde_json::Value) -> Option<String> {
    if let Some(s) = tool_input.as_str() {
        return Some(s.to_string());
    }
    if let Some(obj) = tool_input.as_object() {
        for key in &["file_path", "path", "pattern", "glob", "query", "file", "filename"] {
            if let Some(val) = obj.get(*key).and_then(|v| v.as_str()) {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn extract_bash_command(tool_input: &serde_json::Value) -> Option<String> {
    if let Some(s) = tool_input.as_str() {
        return Some(s.to_string());
    }
    if let Some(obj) = tool_input.as_object() {
        for key in &["command", "cmd"] {
            if let Some(val) = obj.get(*key).and_then(|v| v.as_str()) {
                return Some(val.to_string());
            }
        }
    }
    None
}

pub fn normalize_payload(raw: RawClaudeHookEvent) -> AgentEvent {
    let session_id = raw
        .session_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());

    let project = extract_project(raw.cwd.as_deref());

    let hook_name = raw.hook_event_name.as_deref().unwrap_or("unknown");
    let kind = match hook_name {
        "PreToolUse" => "tool_start",
        "PostToolUse" => "tool_end",
        "PostToolUseFailure" => "tool_failed",
        "Notification" => "needs_you",
        "Stop" => "done",
        "UserPromptSubmit" => "prompt",
        "SessionStart" => "session_start",
        "SessionEnd" => "session_end",
        other => other,
    }
    .to_string();

    let tool = raw.tool_name.clone();

    let label = if let Some(ref t) = raw.tool_name {
        let t_lower = t.to_ascii_lowercase();
        if t_lower == "read" || t_lower == "grep" || t_lower == "glob" {
            let target = raw
                .tool_input
                .as_ref()
                .and_then(extract_file_or_pattern)
                .unwrap_or_default();
            if target.is_empty() {
                "Read".to_string()
            } else {
                format!("Read {}", target)
            }
        } else if t_lower == "edit" || t_lower == "write" || t_lower == "multiedit" {
            let target = raw
                .tool_input
                .as_ref()
                .and_then(extract_file_or_pattern)
                .unwrap_or_default();
            if target.is_empty() {
                "Edit".to_string()
            } else {
                format!("Edit {}", target)
            }
        } else if t_lower == "bash" {
            let cmd_full = raw
                .tool_input
                .as_ref()
                .and_then(extract_bash_command)
                .unwrap_or_default();
            let truncated: String = cmd_full.chars().take(60).collect();
            format!("Bash: {}", truncated)
        } else {
            t.clone()
        }
    } else {
        match kind.as_str() {
            "prompt" => "Prompt".to_string(),
            "done" => "Done".to_string(),
            "needs_you" => raw.message.unwrap_or_else(|| "Needs attention".to_string()),
            "session_start" => "Session started".to_string(),
            "session_end" => "Session ended".to_string(),
            _ => hook_name.to_string(),
        }
    };

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    AgentEvent {
        session_id,
        agent: "claude-code".to_string(),
        project,
        kind,
        tool,
        label,
        ts,
    }
}

struct CurrentUserSecurityAttributes {
    sa: windows::Win32::Security::SECURITY_ATTRIBUTES,
    sd_ptr: usize,
}

unsafe impl Send for CurrentUserSecurityAttributes {}
unsafe impl Sync for CurrentUserSecurityAttributes {}

impl Drop for CurrentUserSecurityAttributes {
    fn drop(&mut self) {
        if self.sd_ptr != 0 {
            unsafe {
                let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(self.sd_ptr as *mut _));
            }
        }
    }
}

fn create_current_user_security_attributes() -> Option<CurrentUserSecurityAttributes> {
    use windows::Win32::Foundation::*;
    use windows::Win32::Security::Authorization::*;
    use windows::Win32::Security::*;
    use windows::Win32::System::Threading::*;
    use windows::core::HSTRING;

    unsafe {
        let mut token_handle = HANDLE(std::ptr::null_mut());
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle).is_err() {
            return None;
        }

        let mut returned_len = 0u32;
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut returned_len);
        if returned_len == 0 {
            let _ = CloseHandle(token_handle);
            return None;
        }

        let mut buf = vec![0u8; returned_len as usize];
        if GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut _),
            returned_len,
            &mut returned_len,
        )
        .is_err()
        {
            let _ = CloseHandle(token_handle);
            return None;
        }
        let _ = CloseHandle(token_handle);

        let token_user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut string_sid: windows::core::PWSTR = windows::core::PWSTR::null();
        if ConvertSidToStringSidW(token_user.User.Sid, &mut string_sid).is_err() {
            return None;
        }

        let sid_str = string_sid.to_string().unwrap_or_default();
        let _ = LocalFree(HLOCAL(string_sid.0 as _));

        if sid_str.is_empty() {
            return None;
        }

        // SDDL: DACL protected (P), allow Generic All (GA) to current user SID
        let sddl = format!("D:P(A;;GA;;;{})", sid_str);
        let sddl_hstring = HSTRING::from(sddl);

        let mut p_sd = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            windows::core::PCWSTR(sddl_hstring.as_ptr()),
            SDDL_REVISION_1,
            &mut p_sd,
            None,
        )
        .is_err()
            || p_sd.0.is_null()
        {
            return None;
        }

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: p_sd.0,
            bInheritHandle: BOOL(0),
        };

        Some(CurrentUserSecurityAttributes {
            sa,
            sd_ptr: p_sd.0 as usize,
        })
    }
}

pub async fn start_agent_pipe_server(app: AppHandle) {
    let sa_guard = create_current_user_security_attributes();

    let create_server_instance = |is_first: bool| {
        let raw_sa = sa_guard.as_ref().map(|g| &g.sa as *const _ as *mut std::ffi::c_void).unwrap_or(std::ptr::null_mut());
        if is_first {
            if !raw_sa.is_null() {
                if let Ok(s) = unsafe { ServerOptions::new().first_pipe_instance(true).create_with_security_attributes_raw(PIPE_NAME, raw_sa) } {
                    return Ok(s);
                }
            }
            if let Ok(s) = ServerOptions::new().first_pipe_instance(true).create(PIPE_NAME) {
                return Ok(s);
            }
            ServerOptions::new().create(PIPE_NAME)
        } else {
            if !raw_sa.is_null() {
                if let Ok(s) = unsafe { ServerOptions::new().create_with_security_attributes_raw(PIPE_NAME, raw_sa) } {
                    return Ok(s);
                }
            }
            ServerOptions::new().create(PIPE_NAME)
        }
    };

    let mut server = match create_server_instance(true) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to create agent pipe server: {}", e);
            return;
        }
    };

    loop {
        if crate::is_quitting() {
            break;
        }
        if server.connect().await.is_ok() {
            if crate::is_quitting() {
                break;
            }
            let mut connected_server = server;
            // Create next server instance to accept future clients
            match create_server_instance(false) {
                Ok(next_server) => {
                    server = next_server;
                }
                Err(e) => {
                    log::error!("Failed to recreate agent pipe instance: {}", e);
                    break;
                }
            }

            let app_clone = app.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match connected_server.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.len() > MAX_PAYLOAD_SIZE {
                                // Payload over 64 KB, reject connection
                                return;
                            }
                            if buf.contains(&b'\n') {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }

                if buf.is_empty() {
                    return;
                }

                let val: serde_json::Value = match serde_json::from_slice(&buf) {
                    Ok(v) => v,
                    Err(_) => {
                        // Malformed JSON, reject
                        return;
                    }
                };

                // Check for approval request
                if val.get("type").and_then(|v| v.as_str()) == Some("approval") {
                    let req_id = val.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let conv_id = val.get("conversationId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let cmd = val.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();

                    if req_id.is_empty() || cmd.is_empty() {
                        use tokio::io::AsyncWriteExt;
                        let _ = connected_server.write_all(b"deny\n").await;
                        let _ = connected_server.flush().await;
                        return;
                    }

                    log_receiver(&format!("APPROVAL_REQ_RECEIVED id={}", req_id));
                    let (tx, rx) = oneshot::channel::<String>();
                    register_pending_approval(req_id.clone(), conv_id, tx);

                    if let Some(win) = app_clone.get_webview_window("main") {
                        let _ = win.unminimize();
                        let _ = win.show();
                        let _ = win.set_focus();
                        let _ = win.emit("agent-approval-request", &val);
                        log_receiver(&format!("EMITTED_TO_WINDOW id={}", req_id));
                    } else {
                        let _ = app_clone.emit("agent-approval-request", val);
                        log_receiver(&format!("EMITTED_TO_APP id={}", req_id));
                    }

                    let reply = match tokio::time::timeout(tokio::time::Duration::from_secs(45), rx).await {
                        Ok(Ok(choice)) => {
                            log_receiver(&format!("APPROVAL_RESOLVED id={} choice={}", req_id, choice));
                            choice
                        }
                        _ => {
                            log_receiver(&format!("APPROVAL_TIMED_OUT id={}", req_id));
                            "deny".to_string()
                        }
                    };

                    {
                        let mut lock = PENDING_APPROVALS.lock().unwrap();
                        if let Some(ref mut map) = *lock {
                            map.remove(&req_id);
                        }
                    }

                    use tokio::io::AsyncWriteExt;
                    let reply_line = format!("{}\n", reply);
                    let _ = connected_server.write_all(reply_line.as_bytes()).await;
                    let _ = connected_server.flush().await;
                    return;
                }

                // Antigravity hook event handling
                if let Some(agent_name) = val.get("agent").and_then(|a| a.as_str()) {
                    if agent_name == "antigravity" {
                        let event_name = val
                            .get("event")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        if event_name == "Stop" {
                            if let Some(conv_id) = val.get("conversationId").and_then(|c| c.as_str()) {
                                dismiss_pending_approvals_for_conversation(conv_id);
                            }
                        }

                        // Keep last event in memory
                        {
                            let mut lock = LAST_AGENT_EVENT.lock().unwrap();
                            *lock = Some((event_name.clone(), Instant::now()));
                        }

                        let mut obj = val.as_object().cloned().unwrap_or_default();
                        let ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        obj.insert("receivedAt".to_string(), serde_json::json!(ts));

                        let _ = app_clone.emit("agent-event", serde_json::Value::Object(obj));
                        return;
                    }
                }

                // Legacy Claude Code hook mapping (kept unused)
                if let Ok(raw) = serde_json::from_value::<RawClaudeHookEvent>(val) {
                    let event = normalize_payload(raw);
                    let _ = app_clone.emit("agent-event", event);
                }
            });
        } else {
            // Short backoff on error before retrying
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
