use std::io::{Read, Write};
#[cfg(feature = "approvals")]
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;
use serde_json::{json, Map, Value};

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        s.chars().take(max_len).collect()
    } else {
        s.to_string()
    }
}

#[cfg(all(debug_assertions, feature = "approvals"))]
fn log_hook(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\ProgramData\Dude\hooks\dude-hook.log")
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = writeln!(f, "[{}][PID:{}] {}", now, std::process::id(), msg);
    }
}

#[cfg(not(all(debug_assertions, feature = "approvals")))]
#[inline(always)]
#[allow(dead_code)]
fn log_hook(_msg: &str) {}

#[allow(dead_code)]
fn approvals_flag_exists() -> bool {
    std::path::Path::new(r"C:\ProgramData\Dude\hooks\approvals.on").exists()
}

#[cfg(feature = "approvals")]
#[allow(dead_code)]
fn should_include_approval_overrides() -> bool {
    let mut check_paths = Vec::new();
    if let Ok(app_data) = std::env::var("APPDATA") {
        check_paths.push(
            std::path::PathBuf::from(app_data)
                .join("com.shubham7dev.dude")
                .join("dude_store.json"),
        );
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        check_paths.push(
            std::path::PathBuf::from(local_app_data)
                .join("com.shubham7dev.dude")
                .join("dude_store.json"),
        );
    }
    check_paths.push(std::path::PathBuf::from(
        r"C:\ProgramData\Dude\hooks\approval_overrides.on",
    ));

    for path in check_paths {
        if path.file_name() == Some(std::ffi::OsStr::new("approval_overrides.on")) {
            if path.exists() {
                return true;
            }
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(val) = serde_json::from_str::<Value>(&content) {
                if let Some(b) = val.get("agents.approvalOverrides").and_then(|v| v.as_bool()) {
                    return b;
                }
                if let Some(b) = val
                    .get("agents")
                    .and_then(|a| a.get("approvalOverrides"))
                    .and_then(|v| v.as_bool())
                {
                    return b;
                }
            }
        }
    }
    false
}

fn open_pipe_with_retry(
    read: bool,
    write: bool,
    max_retries: usize,
    delay_ms: u64,
) -> Result<std::fs::File, std::io::Error> {
    for attempt in 0..max_retries {
        match std::fs::OpenOptions::new()
            .read(read)
            .write(write)
            .open(r"\\.\pipe\dude-agent-events")
        {
            Ok(file) => return Ok(file),
            Err(e) => {
                let os_err = e.raw_os_error();
                // 2 = ERROR_FILE_NOT_FOUND, 231 = ERROR_PIPE_BUSY, 232 = ERROR_NO_DATA, 536 = ERROR_PIPE_LISTENING
                let is_transient = os_err == Some(2)
                    || os_err == Some(231)
                    || os_err == Some(232)
                    || os_err == Some(536);
                if attempt + 1 < max_retries && is_transient {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                } else if !is_transient {
                    return Err(e);
                }
            }
        }
    }
    std::fs::OpenOptions::new()
        .read(read)
        .write(write)
        .open(r"\\.\pipe\dude-agent-events")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let agent = args.get(1).cloned().unwrap_or_else(|| "antigravity".to_string());
    let event = args.get(2).cloned().unwrap_or_else(|| "unknown".to_string());

    #[cfg(all(debug_assertions, feature = "approvals"))]
    log_hook(&format!("INVOKED agent={} event={}", agent, event));

    // 1. Read stdin with a 500ms limit
    let (tx_in, rx_in) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut buf);
        let _ = tx_in.send(buf);
    });

    let stdin_bytes = rx_in.recv_timeout(Duration::from_millis(500)).unwrap_or_default();

    // 2. Filter payload fields
    let mut forwarded = Map::new();
    forwarded.insert("agent".to_string(), json!(truncate_str(&agent, 100)));
    forwarded.insert("event".to_string(), json!(truncate_str(&event, 100)));

    if !stdin_bytes.is_empty() {
        if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&stdin_bytes) {
            if let Some(v) = map.get("conversationId").and_then(|s| s.as_str()) {
                forwarded.insert("conversationId".to_string(), json!(truncate_str(v, 4000)));
            }

            if let Some(first_ws) = map
                .get("workspacePaths")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
            {
                let clean_ws = truncate_str(first_ws, 4000);
                forwarded.insert("workspacePaths".to_string(), json!([clean_ws.clone()]));
                forwarded.insert("workspacePath".to_string(), json!(clean_ws));
            } else if let Some(v) = map.get("workspacePath").and_then(|s| s.as_str()) {
                let clean_ws = truncate_str(v, 4000);
                forwarded.insert("workspacePaths".to_string(), json!([clean_ws.clone()]));
                forwarded.insert("workspacePath".to_string(), json!(clean_ws));
            }

            if let Some(v) = map.get("modelName").and_then(|s| s.as_str()) {
                forwarded.insert("modelName".to_string(), json!(truncate_str(v, 4000)));
            }

            if let Some(tc) = map.get("toolCall").and_then(|v| v.as_object()) {
                let mut tc_map = Map::new();
                if let Some(name) = tc.get("name").and_then(|s| s.as_str()) {
                    tc_map.insert("name".to_string(), json!(truncate_str(name, 4000)));
                }
                if let Some(args_val) = tc.get("args") {
                    if let Some(s) = args_val.as_str() {
                        tc_map.insert("args".to_string(), json!(truncate_str(s, 4000)));
                    } else if let Ok(s) = serde_json::to_string(args_val) {
                        tc_map.insert("args".to_string(), json!(truncate_str(&s, 4000)));
                    }
                }
                forwarded.insert("toolCall".to_string(), Value::Object(tc_map));
            }

            if let Some(v) = map.get("stepIdx").and_then(|n| n.as_i64()) {
                forwarded.insert("stepIdx".to_string(), json!(v));
            }

            if let Some(v) = map.get("error").and_then(|s| s.as_str()) {
                forwarded.insert("error".to_string(), json!(truncate_str(v, 300)));
            }

            if let Some(v) = map.get("invocationNum").and_then(|n| n.as_i64()) {
                forwarded.insert("invocationNum".to_string(), json!(v));
            }

            if let Some(v) = map.get("terminationReason").and_then(|s| s.as_str()) {
                forwarded.insert("terminationReason".to_string(), json!(truncate_str(v, 4000)));
            }

            if let Some(v) = map.get("fullyIdle").and_then(|b| b.as_bool()) {
                forwarded.insert("fullyIdle".to_string(), json!(v));
            }
        }
    }

    // 3. Extract run_command info for PreToolUse approval gate
    #[cfg(feature = "approvals")]
    let mut is_run_command = false;
    #[cfg(feature = "approvals")]
    let mut command_line = String::new();
    #[cfg(feature = "approvals")]
    let mut cwd_str = String::new();

    #[cfg(feature = "approvals")]
    if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&stdin_bytes) {
        if let Some(tc) = map.get("toolCall").and_then(|v| v.as_object()) {
            if let Some(name) = tc.get("name").and_then(|s| s.as_str()) {
                if name == "run_command" {
                    is_run_command = true;
                }
            }
            if let Some(args_val) = tc.get("args") {
                if let Some(args_obj) = args_val.as_object() {
                    if let Some(c) = args_obj
                        .get("CommandLine")
                        .or_else(|| args_obj.get("commandLine"))
                        .or_else(|| args_obj.get("command"))
                        .and_then(|v| v.as_str())
                    {
                        command_line = c.to_string();
                    }
                    if let Some(w) = args_obj
                        .get("Cwd")
                        .or_else(|| args_obj.get("cwd"))
                        .and_then(|v| v.as_str())
                    {
                        cwd_str = w.to_string();
                    }
                } else if let Some(s) = args_val.as_str() {
                    if let Ok(Value::Object(sub_map)) = serde_json::from_str::<Value>(s) {
                        if let Some(c) = sub_map
                            .get("CommandLine")
                            .or_else(|| sub_map.get("commandLine"))
                            .or_else(|| sub_map.get("command"))
                            .and_then(|v| v.as_str())
                        {
                            command_line = c.to_string();
                        }
                        if let Some(w) = sub_map
                            .get("Cwd")
                            .or_else(|| sub_map.get("cwd"))
                            .and_then(|v| v.as_str())
                        {
                            cwd_str = w.to_string();
                        }
                    } else {
                        command_line = s.to_string();
                    }
                }
            }
        }
    }

    // 4. Approval Flow for PreToolUse with run_command (feature-gated behind "approvals")
    #[cfg(feature = "approvals")]
    if event == "PreToolUse" && is_run_command && approvals_flag_exists() {
        let req_id = format!(
            "req-{:x}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            std::process::id()
        );

        let conv_id = forwarded
            .get("conversationId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let ws_path = forwarded
            .get("workspacePath")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let step_idx = forwarded.get("stepIdx").and_then(|v| v.as_i64());

        log_hook(&format!(
            "APPROVAL_START id={} step={:?}",
            req_id, step_idx
        ));

        let approval_payload = json!({
            "type": "approval",
            "id": req_id,
            "conversationId": conv_id,
            "stepIdx": step_idx,
            "workspacePath": ws_path,
            "command": command_line,
            "cwd": cwd_str
        });

        let mut approval_str =
            serde_json::to_string(&approval_payload).unwrap_or_else(|_| "{}".to_string());
        if approval_str.len() > 64 * 1024 {
            approval_str.truncate(64 * 1024);
        }

        // Open named pipe with read and write
        let mut pipe = match open_pipe_with_retry(true, true, 5, 20) {
            Ok(p) => p,
            Err(_) => {
                log_hook(&format!("PIPE_CONNECT_FAILED id={}", req_id));
                println!("{}", json!({ "decision": "force_ask" }));
                std::process::exit(0);
            }
        };

        // Send approval message with newline
        if pipe.write_all(format!("{}\n", approval_str).as_bytes()).is_err() || pipe.flush().is_err() {
            log_hook(&format!("PIPE_WRITE_FAILED id={}", req_id));
            println!("{}", json!({ "decision": "force_ask" }));
            std::process::exit(0);
        }

        log_hook(&format!("PIPE_WRITTEN id={}", req_id));

        // WAIT for one reply line on the same connection, internal deadline 47s
        let (tx_reply, rx_reply) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut line = String::new();
            let res = reader.read_line(&mut line).map(|_| line);
            let _ = tx_reply.send(res);
        });

        match rx_reply.recv_timeout(Duration::from_secs(47)) {
            Ok(Ok(line)) => {
                let reply = line.trim();
                log_hook(&format!("REPLY_RECEIVED id={} reply={}", req_id, reply));

                let trimmed = command_line.trim();

                let out = match reply {
                    // User clicked Allow in Dude -> allow
                    "once" | "conversation" | "always" | "allow" | "variant:1" | "once:1"
                    | "variant:2" | "once:2" | "variant:3" | "once:3" | "variant:4" | "once:4" => {
                        let mut val = json!({
                            "decision": "allow",
                            "reason": "Approved from Dude"
                        });
                        if !trimmed.is_empty() {
                            val["permissionOverrides"] = json!([format!("command({})", trimmed)]);
                        }
                        log_hook(&format!("OUTPUT_DECISION id={} decision=allow", req_id));
                        val
                    }
                    "deny" => {
                        log_hook(&format!("OUTPUT_DECISION id={} decision=deny", req_id));
                        json!({
                            "decision": "deny",
                            "reason": "Denied from Dude by the user"
                        })
                    }
                    // Every other case while approvals.on exists -> force_ask
                    _ => {
                        log_hook(&format!("OUTPUT_DECISION id={} decision=force_ask", req_id));
                        json!({ "decision": "force_ask" })
                    }
                };

                println!("{}", out);
            }
            Ok(Err(_e)) => {
                log_hook(&format!("PIPE_READ_ERROR id={}", req_id));
                println!("{}", json!({ "decision": "force_ask" }));
            }
            Err(_e) => {
                log_hook(&format!("TIMEOUT id={}", req_id));
                println!("{}", json!({ "decision": "force_ask" }));
            }
        }
        std::process::exit(0);
    }

    // 5. Default Non-approval Forwarding (for all other events, tools, or when approvals feature is OFF)
    let response = match event.as_str() {
        "PreToolUse" => r#"{"decision":"allow"}"#,
        "Stop" => r#"{"decision":"allow"}"#,
        _ => "{}",
    };
    println!("{}", response);
    let _ = std::io::stdout().flush();

    let mut payload_str =
        serde_json::to_string(&Value::Object(forwarded)).unwrap_or_else(|_| "{}".to_string());
    const MAX_SIZE: usize = 48 * 1024;
    if payload_str.len() > MAX_SIZE {
        payload_str.truncate(MAX_SIZE);
    }

    // Forward to named pipe \\.\pipe\dude-agent-events with 300ms connect timeout
    let (tx_pipe, rx_pipe) = mpsc::channel();
    let pipe_payload = payload_str;
    std::thread::spawn(move || {
        if let Ok(mut pipe) = open_pipe_with_retry(false, true, 3, 20) {
            let _ = pipe.write_all(pipe_payload.as_bytes());
            let _ = pipe.flush();
        }
        let _ = tx_pipe.send(());
    });
    let _ = rx_pipe.recv_timeout(Duration::from_millis(300));

    std::process::exit(0);
}
