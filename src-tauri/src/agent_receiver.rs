use std::path::Path;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncReadExt;
use tokio::net::windows::named_pipe::ServerOptions;

pub const PIPE_NAME: &str = r"\\.\pipe\dude-agent-events";
const MAX_PAYLOAD_SIZE: usize = 65536; // 64 KB

static LAST_AGENT_EVENT: Mutex<Option<(String, Instant)>> = Mutex::new(None);

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

    // Generate label
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

pub async fn start_agent_pipe_server(app: AppHandle) {
    let mut server = match ServerOptions::new()
        .first_pipe_instance(true)
        .create(PIPE_NAME)
    {
        Ok(s) => s,
        Err(_) => match ServerOptions::new().create(PIPE_NAME) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to create agent pipe server: {}", e);
                return;
            }
        },
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
            match ServerOptions::new().create(PIPE_NAME) {
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
                        // Invalid JSON, reject
                        return;
                    }
                };

                // Antigravity hook event handling
                if let Some(agent_name) = val.get("agent").and_then(|a| a.as_str()) {
                    if agent_name == "antigravity" {
                        let event_name = val
                            .get("event")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown")
                            .to_string();

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
