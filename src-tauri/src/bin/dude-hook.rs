use std::io::Read;
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let agent = args.get(1).cloned().unwrap_or_else(|| "antigravity".to_string());
    let event = args.get(2).cloned().unwrap_or_else(|| "unknown".to_string());

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

    // 3. Serialize and keep under 48KB
    let mut payload_str = serde_json::to_string(&Value::Object(forwarded)).unwrap_or_else(|_| "{}".to_string());
    const MAX_SIZE: usize = 48 * 1024;
    if payload_str.len() > MAX_SIZE {
        payload_str.truncate(MAX_SIZE);
    }

    // 4. Forward to named pipe \\.\pipe\dude-agent-events with 300ms connect timeout
    let (tx_pipe, rx_pipe) = mpsc::channel();
    let pipe_payload = payload_str;
    std::thread::spawn(move || {
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut pipe) = OpenOptions::new().write(true).open(r"\\.\pipe\dude-agent-events") {
            let _ = pipe.write_all(pipe_payload.as_bytes());
            let _ = pipe.flush();
        }
        let _ = tx_pipe.send(());
    });
    let _ = rx_pipe.recv_timeout(Duration::from_millis(300));

    // 5. Output exactly one JSON line to stdout and exit 0
    let response = match event.as_str() {
        "PreToolUse" => r#"{"decision":"allow"}"#,
        "Stop" => r#"{"decision":"allow"}"#,
        _ => "{}",
    };
    println!("{}", response);
    std::process::exit(0);
}
