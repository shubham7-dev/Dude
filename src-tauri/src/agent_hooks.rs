use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

extern "system" {
    fn GetShortPathNameW(
        lpszlongpath: *const u16,
        lpszshortpath: *mut u16,
        cchbuffer: u32,
    ) -> u32;
    fn GetLocalTime(lpsystemtime: *mut SYSTEMTIME);
}

#[repr(C)]
#[allow(non_snake_case)]
struct SYSTEMTIME {
    wYear: u16,
    wMonth: u16,
    wDayOfWeek: u16,
    wDay: u16,
    wHour: u16,
    wMinute: u16,
    wSecond: u16,
    wMilliseconds: u16,
}

pub const AGENT_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PreInvocation",
    "PostInvocation",
    "Stop",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentsStatusInfo {
    pub installed: bool,
    pub enabled: bool,
    pub status: String, // "not installed", "installed and enabled", "installed but disabled", "needs repair"
    pub hooks_path: String,
    pub cursor_hooks_path: String,
    pub events: Vec<String>,
    pub backup_path: String,
    pub hook_exe_path: String,
    pub error: Option<String>,
}

pub fn get_antigravity_hooks_path() -> Result<PathBuf, String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "Could not determine user profile directory (%USERPROFILE% / $HOME)".to_string())?;

    Ok(PathBuf::from(home).join(".gemini").join("config").join("hooks.json"))
}

pub fn get_cursor_hooks_path() -> Result<PathBuf, String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "Could not determine user profile directory (%USERPROFILE% / $HOME)".to_string())?;

    Ok(PathBuf::from(home).join(".cursor").join("hooks.json"))
}

fn to_short_path(path: &Path) -> Result<PathBuf, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::ffi::OsStringExt;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 1024];
    let len = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 {
        return Err(format!("GetShortPathNameW failed for {}", path.display()));
    }
    buf.truncate(len as usize);
    let os_str = OsString::from_wide(&buf);
    Ok(PathBuf::from(os_str))
}

pub fn generate_backup_path(hooks_path: &Path) -> PathBuf {
    let mut st = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { GetLocalTime(&mut st) };
    let timestamp = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    );
    let file_name = format!("hooks.json.dude-backup-{}", timestamp);
    hooks_path.with_file_name(file_name)
}

fn find_source_dude_hook_exe() -> Result<PathBuf, String> {
    // 1. Check directory of current executable (direct or in resources/ subfolder)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let candidate = parent.join("dude-hook.exe");
            if candidate.exists() {
                return Ok(candidate);
            }
            let candidate_res = parent.join("resources").join("dude-hook.exe");
            if candidate_res.exists() {
                return Ok(candidate_res);
            }
            let candidate_res2 = parent.join("_up_").join("resources").join("dude-hook.exe");
            if candidate_res2.exists() {
                return Ok(candidate_res2);
            }
        }
    }

    // 2. Check candidate build and resource paths
    let candidates = [
        "src-tauri/resources/dude-hook.exe",
        "resources/dude-hook.exe",
        "src-tauri/target/release/dude-hook.exe",
        "target/release/dude-hook.exe",
        "../src-tauri/target/release/dude-hook.exe",
        "../target/release/dude-hook.exe",
        "src-tauri/target/debug/dude-hook.exe",
        "target/debug/dude-hook.exe",
        "../src-tauri/target/debug/dude-hook.exe",
        "../target/debug/dude-hook.exe",
    ];

    for rel in &candidates {
        let p = Path::new(rel);
        if p.exists() {
            if let Ok(abs) = fs::canonicalize(p) {
                return Ok(abs);
            }
            return Ok(p.to_path_buf());
        }
    }

    Err("Could not locate source dude-hook.exe binary to install. Build it with cargo build --bin dude-hook first.".to_string())
}

pub fn deploy_hook_exe() -> Result<PathBuf, String> {
    let source_exe = find_source_dude_hook_exe()?;

    // 1. Prefer C:\ProgramData\Dude\hooks\dude-hook.exe (create folder)
    let prog_data = PathBuf::from(r"C:\ProgramData\Dude\hooks");
    let mut chosen_dest = None;

    if fs::create_dir_all(&prog_data).is_ok() {
        let dest = prog_data.join("dude-hook.exe");
        let dest_str = dest.to_string_lossy().to_string();
        if !dest_str.contains(' ') {
            chosen_dest = Some(dest);
        }
    }

    // 2. If ProgramData fails or contains spaces, fallback to 8.3 short path of %LOCALAPPDATA%\Dude\hooks
    let dest_path = match chosen_dest {
        Some(p) => p,
        None => {
            let local_app_data = std::env::var("LOCALAPPDATA")
                .map_err(|_| "Could not find %LOCALAPPDATA%".to_string())?;
            let local_dir = PathBuf::from(local_app_data).join("Dude").join("hooks");
            fs::create_dir_all(&local_dir)
                .map_err(|e| format!("Failed to create local hooks dir: {}", e))?;
            let raw_dest = local_dir.join("dude-hook.exe");

            let short_dest = if raw_dest.to_string_lossy().contains(' ') {
                to_short_path(&raw_dest).unwrap_or(raw_dest)
            } else {
                raw_dest
            };

            if short_dest.to_string_lossy().contains(' ') {
                return Err("Hook path contains spaces and 8.3 conversion failed. Aborted.".to_string());
            }
            short_dest
        }
    };

    // Copy to destination
    let clean_dest = PathBuf::from(dest_path.to_string_lossy().replace(r"\\?\", ""));
    fs::copy(&source_exe, &clean_dest)
        .map_err(|e| format!("Failed to copy dude-hook.exe to {}: {}", clean_dest.display(), e))?;

    Ok(clean_dest)
}

pub fn get_target_hook_exe_path() -> PathBuf {
    let prog_data = PathBuf::from(r"C:\ProgramData\Dude\hooks\dude-hook.exe");
    if prog_data.exists() {
        return prog_data;
    }

    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let raw = PathBuf::from(local_app_data).join("Dude").join("hooks").join("dude-hook.exe");
        if raw.exists() {
            return to_short_path(&raw).unwrap_or(raw);
        }
    }

    prog_data
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut serialized = serde_json::to_string_pretty(value)
        .map_err(|e| format!("Failed to serialize hooks JSON: {}", e))?;
    serialized.push('\n');

    let parent = path.parent().ok_or_else(|| "Hooks file has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;

    let temp_name = format!("hooks.json.tmp-{}", std::process::id());
    let temp_path = path.with_file_name(temp_name);

    fs::write(&temp_path, serialized.as_bytes())
        .map_err(|e| format!("Failed to write temporary hooks file: {}", e))?;

    if let Err(e) = fs::rename(&temp_path, path) {
        if let Err(copy_err) = fs::copy(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            return Err(format!("Failed to write hooks file atomically: {} (copy error: {})", e, copy_err));
        }
        let _ = fs::remove_file(&temp_path);
    }

    Ok(())
}

fn build_dude_hook_object(hook_exe_path: &Path, enabled: bool) -> Value {
    let clean_path = hook_exe_path.to_string_lossy().replace(r"\\?\", "");
    let cmd = |event: &str| -> String {
        format!("{} antigravity {}", clean_path, event)
    };

    json!({
        "enabled": enabled,
        "SessionStart": [
            {
                "type": "command",
                "command": cmd("SessionStart"),
                "timeout": 5
            }
        ],
        "PreToolUse": [
            {
                "matcher": "*",
                "hooks": [
                    {
                        "type": "command",
                        "command": cmd("PreToolUse"),
                        "timeout": 60
                    }
                ]
            }
        ],
        "PostToolUse": [
            {
                "matcher": "*",
                "hooks": [
                    {
                        "type": "command",
                        "command": cmd("PostToolUse"),
                        "timeout": 5
                    }
                ]
            }
        ],
        "PreInvocation": [
            {
                "type": "command",
                "command": cmd("PreInvocation"),
                "timeout": 5
            }
        ],
        "PostInvocation": [
            {
                "type": "command",
                "command": cmd("PostInvocation"),
                "timeout": 5
            }
        ],
        "Stop": [
            {
                "type": "command",
                "command": cmd("Stop"),
                "timeout": 5
            }
        ]
    })
}

#[derive(Debug)]
pub enum SelfTestOutcome {
    Success(String),
    WarningSlowPowerShell(String),
    Fatal(String),
}

#[derive(Debug, Clone)]
struct SingleRunOutput {
    success: bool,
    elapsed_ms: u128,
    timed_out: bool,
    timeout_limit_ms: u64,
}

fn execute_command_with_stdin(
    prog: &str,
    args: &[&str],
    timeout_ms: u64,
) -> SingleRunOutput {
    let sample_payload = r#"{"event":"PreToolUse","toolCall":{"name":"test"}}"#;
    let start = std::time::Instant::now();

    let mut command = std::process::Command::new(prog);
    command.args(args);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(_) => {
            return SingleRunOutput {
                success: false,
                elapsed_ms: start.elapsed().as_millis(),
                timed_out: false,
                timeout_limit_ms: timeout_ms,
            };
        }
    };

    let pid = child.id();

    // Write sample payload to stdin and CLOSE stdin right away (drop handle so hook never waits for EOF)
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(sample_payload.as_bytes());
        let _ = stdin.flush();
        drop(stdin);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let res = child.wait_with_output();
        let _ = tx.send(res);
    });

    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(Ok(output)) => {
            let elapsed_ms = start.elapsed().as_millis();
            let mut success = output.status.success();
            if success {
                let out_str = String::from_utf8_lossy(&output.stdout);
                let clean_out = out_str.trim().trim_start_matches('\u{feff}').trim();
                let has_allow = if let Ok(json_val) = serde_json::from_str::<Value>(clean_out) {
                    json_val.get("decision").and_then(|v| v.as_str()) == Some("allow")
                } else {
                    clean_out.lines().any(|line| {
                        let l = line.trim().trim_start_matches('\u{feff}').trim();
                        serde_json::from_str::<Value>(l)
                            .map(|jv| jv.get("decision").and_then(|v| v.as_str()) == Some("allow"))
                            .unwrap_or(false)
                    })
                };
                if !has_allow {
                    success = false;
                }
            }
            SingleRunOutput {
                success,
                elapsed_ms,
                timed_out: false,
                timeout_limit_ms: timeout_ms,
            }
        }
        Ok(Err(_)) => SingleRunOutput {
            success: false,
            elapsed_ms: start.elapsed().as_millis(),
            timed_out: false,
            timeout_limit_ms: timeout_ms,
        },
        Err(_) => {
            let mut tk = std::process::Command::new("taskkill");
            tk.args(["/F", "/T", "/PID", &pid.to_string()]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                tk.creation_flags(0x08000000);
            }
            let _ = tk.output();
            SingleRunOutput {
                success: false,
                elapsed_ms: timeout_ms as u128,
                timed_out: true,
                timeout_limit_ms: timeout_ms,
            }
        }
    }
}

pub fn run_self_test(hook_exe_path: &Path) -> SelfTestOutcome {
    let clean_hook_str = hook_exe_path.to_string_lossy().replace(r"\\?\", "");

    // a. Direct: spawn dude-hook.exe with "antigravity PreToolUse", close stdin right away.
    // Limits: 3s first run, 1s second run. Judged by second run.
    let _d1 = execute_command_with_stdin(&clean_hook_str, &["antigravity", "PreToolUse"], 3000);
    let d2 = execute_command_with_stdin(&clean_hook_str, &["antigravity", "PreToolUse"], 1000);
    let direct_pass = d2.success;
    let direct_str = if d2.timed_out {
        format!("direct timed out at {}ms", d2.timeout_limit_ms)
    } else if !d2.success {
        format!("direct failed ({}ms)", d2.elapsed_ms)
    } else {
        format!("direct {}ms", d2.elapsed_ms)
    };

    // b. cmd: cmd /c <exact command string>.
    // Limits: 5s, then 2s. Fails if both runs fail.
    let cmd_str = format!("{} antigravity PreToolUse", clean_hook_str);
    let c1 = execute_command_with_stdin("cmd", &["/c", &cmd_str], 5000);
    let c2 = execute_command_with_stdin("cmd", &["/c", &cmd_str], 2000);
    let cmd_pass = c2.success || c1.success;
    let cmd_str_rep = if c2.success {
        format!("cmd {}ms", c2.elapsed_ms)
    } else if c1.success {
        format!("cmd {}ms", c1.elapsed_ms)
    } else if c2.timed_out {
        format!("cmd timed out at {}ms", c2.timeout_limit_ms)
    } else if c1.timed_out {
        format!("cmd timed out at {}ms", c1.timeout_limit_ms)
    } else {
        format!("cmd failed ({}ms)", c2.elapsed_ms)
    };

    // c. PowerShell: powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "<exact command string>".
    // Limits: 15s, then 8s.
    let ps_cmd = format!("{} antigravity PreToolUse", clean_hook_str);
    let p1 = execute_command_with_stdin(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &ps_cmd],
        15000,
    );
    let (ps_pass, ps_str_rep) = if p1.timed_out {
        (false, "powershell timed out at 15000ms".to_string())
    } else {
        let p2 = execute_command_with_stdin(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &ps_cmd],
            8000,
        );
        let pass = p2.success;
        let rep = if p2.success {
            format!("powershell {}ms", p2.elapsed_ms)
        } else if p2.timed_out {
            format!("powershell timed out at {}ms", p2.timeout_limit_ms)
        } else {
            format!("powershell failed ({}ms)", p2.elapsed_ms)
        };
        (pass, rep)
    };

    let timing = format!("{}, {}, {}", direct_str, cmd_str_rep, ps_str_rep);

    // Abort and roll back ONLY if check (a) fails, or check (b) fails on both runs.
    if !direct_pass || !cmd_pass {
        return SelfTestOutcome::Fatal(timing);
    }

    // If only check (c) fails or times out, do NOT abort
    if !ps_pass {
        return SelfTestOutcome::WarningSlowPowerShell(timing);
    }

    SelfTestOutcome::Success(timing)
}

fn extract_exe_path_from_command(cmd: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if let Some(first) = parts.first() {
        let clean = first.trim_matches('"');
        return Some(PathBuf::from(clean));
    }
    None
}

// --------------------------------------------------------------------------
// Public Functions called by Tauri Commands
// --------------------------------------------------------------------------

pub fn agents_status_internal() -> Result<AgentsStatusInfo, String> {
    let hooks_path = get_antigravity_hooks_path()?;
    let cursor_hooks_path = get_cursor_hooks_path()?.to_string_lossy().to_string();
    let events = AGENT_EVENTS.iter().map(|s| s.to_string()).collect();
    let backup_path = generate_backup_path(&hooks_path).to_string_lossy().to_string();
    let target_hook_exe = get_target_hook_exe_path();
    let hook_exe_path = target_hook_exe.to_string_lossy().to_string();

    if !hooks_path.exists() {
        return Ok(AgentsStatusInfo {
            installed: false,
            enabled: false,
            status: "not installed".to_string(),
            hooks_path: hooks_path.to_string_lossy().to_string(),
            cursor_hooks_path,
            events,
            backup_path,
            hook_exe_path,
            error: None,
        });
    }

    let content = fs::read_to_string(&hooks_path)
        .map_err(|e| format!("Failed to read hooks file at {}: {}", hooks_path.display(), e))?;

    let root: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!(
                "The hooks file at {} contains invalid JSON (error: {}). Aborted without making changes.",
                hooks_path.display(),
                e
            );
            return Ok(AgentsStatusInfo {
                installed: false,
                enabled: false,
                status: "error".to_string(),
                hooks_path: hooks_path.to_string_lossy().to_string(),
                cursor_hooks_path,
                events,
                backup_path,
                hook_exe_path,
                error: Some(msg),
            });
        }
    };

    let dude_obj = match root.get("dude").and_then(|d| d.as_object()) {
        Some(d) => d,
        None => {
            return Ok(AgentsStatusInfo {
                installed: false,
                enabled: false,
                status: "not installed".to_string(),
                hooks_path: hooks_path.to_string_lossy().to_string(),
                cursor_hooks_path,
                events,
                backup_path,
                hook_exe_path,
                error: None,
            });
        }
    };

    let enabled = dude_obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

    // Verify exe path and timeout
    let mut exe_exists = false;
    let mut timeout_ok = false;

    if let Some(ptu_hook) = dude_obj
        .get("PreToolUse")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|g| g.get("hooks"))
        .and_then(|h| h.as_array())
        .and_then(|a| a.first())
    {
        if let Some(cmd_val) = ptu_hook.get("command").and_then(|c| c.as_str()) {
            if let Some(exe_p) = extract_exe_path_from_command(cmd_val) {
                if exe_p.exists() {
                    exe_exists = true;
                }
            }
        }
        if let Some(t) = ptu_hook.get("timeout").and_then(|t| t.as_i64()) {
            if t == 60 {
                timeout_ok = true;
            }
        }
    }

    let (status, error) = if !exe_exists {
        ("needs repair".to_string(), Some("hook binary missing".to_string()))
    } else if !timeout_ok {
        ("needs repair".to_string(), Some("approval timeout".to_string()))
    } else if enabled {
        ("installed and enabled".to_string(), None)
    } else {
        ("installed but disabled".to_string(), None)
    };

    Ok(AgentsStatusInfo {
        installed: true,
        enabled,
        status,
        hooks_path: hooks_path.to_string_lossy().to_string(),
        cursor_hooks_path,
        events,
        backup_path,
        hook_exe_path,
        error,
    })
}

pub fn agents_install_internal() -> Result<String, String> {
    let hooks_path = get_antigravity_hooks_path()?;

    // Read and validate JSON if file already exists
    let (mut root, backup_path_opt) = if hooks_path.exists() {
        let content = fs::read_to_string(&hooks_path)
            .map_err(|e| format!("Failed to read hooks file: {}", e))?;

        let val: Value = serde_json::from_str(&content).map_err(|e| {
            format!(
                "The hooks file at {} contains invalid JSON ({}). Aborted without making any changes.",
                hooks_path.display(),
                e
            )
        })?;

        // Back it up first next to it as hooks.json.dude-backup-YYYYMMDD-HHMMSS
        let backup_path = generate_backup_path(&hooks_path);
        fs::copy(&hooks_path, &backup_path)
            .map_err(|e| format!("Failed to create backup at {}: {}", backup_path.display(), e))?;

        (val, Some(backup_path))
    } else {
        // Create parent directories if missing
        if let Some(parent) = hooks_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        (json!({}), None)
    };

    if !root.is_object() {
        root = json!({});
    }

    // Deploy dude-hook.exe to C:\ProgramData\Dude\hooks\dude-hook.exe
    let deployed_exe = deploy_hook_exe()?;

    // Replace ONLY the "dude" key; never touch, reorder or delete other keys.
    let root_obj = root.as_object_mut().unwrap();
    let dude_config = build_dude_hook_object(&deployed_exe, true);
    root_obj.insert("dude".to_string(), dude_config);

    // Write atomically, 2-space indentation
    atomic_write_json(&hooks_path, &root)?;

    // Self-test
    match run_self_test(&deployed_exe) {
        SelfTestOutcome::Fatal(timing) => {
            // Rollback to backup
            if let Some(ref bp) = backup_path_opt {
                let _ = fs::copy(bp, &hooks_path);
            } else {
                let _ = fs::remove_file(&hooks_path);
            }
            return Err(format!("Self-test failed ({}). Restored previous hooks configuration.", timing));
        }
        SelfTestOutcome::WarningSlowPowerShell(timing) => {
            return Ok(format!("Installed, but the PowerShell check was slow ({}). If no events arrive, press Reinstall.", timing));
        }
        SelfTestOutcome::Success(_) => {
            return Ok("Successfully installed Antigravity hooks for Dude".to_string());
        }
    }
}

pub fn agents_set_enabled_internal(enabled: bool) -> Result<String, String> {
    let hooks_path = get_antigravity_hooks_path()?;
    if !hooks_path.exists() {
        return Err("Hooks file does not exist".to_string());
    }

    let content = fs::read_to_string(&hooks_path)
        .map_err(|e| format!("Failed to read hooks file: {}", e))?;

    let mut root: Value = serde_json::from_str(&content).map_err(|e| {
        format!(
            "The hooks file at {} contains invalid JSON ({}). Aborted without making any changes.",
            hooks_path.display(),
            e
        )
    })?;

    let root_obj = root.as_object_mut().ok_or_else(|| "Hooks root is not a JSON object".to_string())?;

    let dude_obj = root_obj
        .get_mut("dude")
        .and_then(|d| d.as_object_mut())
        .ok_or_else(|| "Dude hooks are not installed".to_string())?;

    // The toggle sets "enabled" true or false on the "dude" key without deleting it.
    dude_obj.insert("enabled".to_string(), json!(enabled));

    atomic_write_json(&hooks_path, &root)?;

    Ok(format!("Antigravity hooks {}", if enabled { "enabled" } else { "disabled" }))
}

pub fn agents_reinstall_internal() -> Result<String, String> {
    let hooks_path = get_antigravity_hooks_path()?;

    let (mut root, current_enabled, backup_path_opt) = if hooks_path.exists() {
        let content = fs::read_to_string(&hooks_path)
            .map_err(|e| format!("Failed to read hooks file: {}", e))?;

        let val: Value = serde_json::from_str(&content).map_err(|e| {
            format!(
                "The hooks file at {} contains invalid JSON ({}). Aborted without making any changes.",
                hooks_path.display(),
                e
            )
        })?;

        let backup_path = generate_backup_path(&hooks_path);
        fs::copy(&hooks_path, &backup_path)
            .map_err(|e| format!("Failed to create backup at {}: {}", backup_path.display(), e))?;

        let enabled = val
            .get("dude")
            .and_then(|d| d.get("enabled"))
            .and_then(|b| b.as_bool())
            .unwrap_or(true);

        (val, enabled, Some(backup_path))
    } else {
        if let Some(parent) = hooks_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        (json!({}), true, None)
    };

    if !root.is_object() {
        root = json!({});
    }

    let deployed_exe = deploy_hook_exe()?;

    // Reinstall rewrites the "dude" key (repair)
    let root_obj = root.as_object_mut().unwrap();
    let dude_config = build_dude_hook_object(&deployed_exe, current_enabled);
    root_obj.insert("dude".to_string(), dude_config);

    atomic_write_json(&hooks_path, &root)?;

    match run_self_test(&deployed_exe) {
        SelfTestOutcome::Fatal(timing) => {
            if let Some(ref bp) = backup_path_opt {
                let _ = fs::copy(bp, &hooks_path);
            } else {
                let _ = fs::remove_file(&hooks_path);
            }
            return Err(format!("Self-test failed ({}). Restored previous hooks configuration.", timing));
        }
        SelfTestOutcome::WarningSlowPowerShell(timing) => {
            return Ok(format!("Installed, but the PowerShell check was slow ({}). If no events arrive, press Reinstall.", timing));
        }
        SelfTestOutcome::Success(_) => {
            return Ok("Successfully reinstalled Antigravity hooks for Dude".to_string());
        }
    }
}

pub fn agents_uninstall_internal() -> Result<String, String> {
    let hooks_path = get_antigravity_hooks_path()?;
    if !hooks_path.exists() {
        return Ok("Hooks file does not exist; nothing to uninstall".to_string());
    }

    let content = fs::read_to_string(&hooks_path)
        .map_err(|e| format!("Failed to read hooks file: {}", e))?;

    let mut root: Value = serde_json::from_str(&content).map_err(|e| {
        format!(
            "The hooks file at {} contains invalid JSON ({}). Aborted without making any changes.",
            hooks_path.display(),
            e
        )
    })?;

    // Backs up first and removes only the "dude" key
    let backup_path = generate_backup_path(&hooks_path);
    fs::copy(&hooks_path, &backup_path)
        .map_err(|e| format!("Failed to create backup before uninstall: {}", e))?;

    if let Some(root_obj) = root.as_object_mut() {
        root_obj.remove("dude");
    }

    atomic_write_json(&hooks_path, &root)?;

    Ok("Successfully uninstalled Dude hooks from Antigravity".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dude_hook_schema() {
        let fake_exe = PathBuf::from(r"C:\ProgramData\Dude\hooks\dude-hook.exe");
        let obj = build_dude_hook_object(&fake_exe, true);

        assert_eq!(obj["enabled"], true);

        // SessionStart, PreInvocation, PostInvocation, Stop take handlers directly (no matcher)
        assert_eq!(obj["SessionStart"][0]["type"], "command");
        assert_eq!(
            obj["SessionStart"][0]["command"],
            r"C:\ProgramData\Dude\hooks\dude-hook.exe antigravity SessionStart"
        );
        assert_eq!(obj["SessionStart"][0]["timeout"], 5);

        assert_eq!(obj["PreInvocation"][0]["type"], "command");
        assert_eq!(
            obj["PreInvocation"][0]["command"],
            r"C:\ProgramData\Dude\hooks\dude-hook.exe antigravity PreInvocation"
        );

        assert_eq!(obj["PostInvocation"][0]["type"], "command");
        assert_eq!(
            obj["PostInvocation"][0]["command"],
            r"C:\ProgramData\Dude\hooks\dude-hook.exe antigravity PostInvocation"
        );

        assert_eq!(obj["Stop"][0]["type"], "command");
        assert_eq!(
            obj["Stop"][0]["command"],
            r"C:\ProgramData\Dude\hooks\dude-hook.exe antigravity Stop"
        );

        // PreToolUse and PostToolUse use matcher groups
        assert_eq!(obj["PreToolUse"][0]["matcher"], "*");
        assert_eq!(obj["PreToolUse"][0]["hooks"][0]["type"], "command");
        assert_eq!(
            obj["PreToolUse"][0]["hooks"][0]["command"],
            r"C:\ProgramData\Dude\hooks\dude-hook.exe antigravity PreToolUse"
        );
        assert_eq!(obj["PreToolUse"][0]["hooks"][0]["timeout"], 60);

        assert_eq!(obj["PostToolUse"][0]["matcher"], "*");
        assert_eq!(obj["PostToolUse"][0]["hooks"][0]["type"], "command");
        assert_eq!(
            obj["PostToolUse"][0]["hooks"][0]["command"],
            r"C:\ProgramData\Dude\hooks\dude-hook.exe antigravity PostToolUse"
        );
    }

    #[test]
    fn test_preserve_other_keys_and_manipulate_dude_key() {
        let temp_dir = std::env::temp_dir().join(format!("dude_test_hooks_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let hooks_file = temp_dir.join("hooks.json");

        // Existing file has "coucou" key
        let initial_json = json!({
            "coucou": {
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "coucou.exe", "timeout": 10 }] }]
            }
        });
        atomic_write_json(&hooks_file, &initial_json).unwrap();

        // 1. Install dude hook
        let fake_exe = PathBuf::from(r"C:\ProgramData\Dude\hooks\dude-hook.exe");
        let mut root: Value = serde_json::from_str(&fs::read_to_string(&hooks_file).unwrap()).unwrap();
        let root_obj = root.as_object_mut().unwrap();
        root_obj.insert("dude".to_string(), build_dude_hook_object(&fake_exe, true));
        atomic_write_json(&hooks_file, &root).unwrap();

        // Check coucou untouched and dude present
        let after_str = fs::read_to_string(&hooks_file).unwrap();
        let after_val: Value = serde_json::from_str(&after_str).unwrap();
        assert!(after_val.get("coucou").is_some());
        assert_eq!(after_val["coucou"]["PreToolUse"][0]["hooks"][0]["command"], "coucou.exe");
        assert!(after_val.get("dude").is_some());
        assert_eq!(after_val["dude"]["enabled"], true);

        // Check 2-space indentation
        assert!(after_str.contains("  \"coucou\": {"));
        assert!(after_str.contains("  \"dude\": {"));

        // 2. Toggle enabled to false
        let mut toggle_val: Value = serde_json::from_str(&fs::read_to_string(&hooks_file).unwrap()).unwrap();
        toggle_val.as_object_mut().unwrap().get_mut("dude").unwrap().as_object_mut().unwrap().insert("enabled".to_string(), json!(false));
        atomic_write_json(&hooks_file, &toggle_val).unwrap();

        let after_toggle: Value = serde_json::from_str(&fs::read_to_string(&hooks_file).unwrap()).unwrap();
        assert_eq!(after_toggle["dude"]["enabled"], false);
        assert!(after_toggle.get("coucou").is_some());

        // 3. Uninstall: removes only "dude"
        let mut un_val: Value = serde_json::from_str(&fs::read_to_string(&hooks_file).unwrap()).unwrap();
        un_val.as_object_mut().unwrap().remove("dude");
        atomic_write_json(&hooks_file, &un_val).unwrap();

        let after_un: Value = serde_json::from_str(&fs::read_to_string(&hooks_file).unwrap()).unwrap();
        assert!(after_un.get("dude").is_none());
        assert!(after_un.get("coucou").is_some());
        assert_eq!(after_un["coucou"]["PreToolUse"][0]["hooks"][0]["command"], "coucou.exe");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_self_test_timing_format() {
        let hook_exe = PathBuf::from(r"C:\ProgramData\Dude\hooks\dude-hook.exe");
        if hook_exe.exists() {
            let outcome = run_self_test(&hook_exe);
            println!("MEASURED TIMINGS: {:?}", outcome);
            match outcome {
                SelfTestOutcome::Success(t) => {
                    assert!(t.contains("direct"));
                    assert!(t.contains("cmd"));
                    assert!(t.contains("powershell"));
                }
                SelfTestOutcome::WarningSlowPowerShell(t) => {
                    assert!(t.contains("direct"));
                    assert!(t.contains("cmd"));
                    assert!(t.contains("powershell"));
                }
                SelfTestOutcome::Fatal(t) => {
                    panic!("Self-test unexpectedly failed: {}", t);
                }
            }
        }
    }
}
