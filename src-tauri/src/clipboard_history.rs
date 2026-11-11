// Dude - Clipboard History Module
// Bounded in-memory list: latest + up to 10 older (11 in total)
// Event-driven Win32 format listener thread

use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;
use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, HWND_MESSAGE,
    MSG, WINDOW_EX_STYLE, WNDCLASSW, WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WM_TIMER,
    WS_OVERLAPPED,
};

const MAX_ITEMS: usize = 11;
const MAX_ITEM_TEXT_LEN: usize = 5000;
const MAX_MONITOR_TEXT_LEN: usize = 20000;
const PREVIEW_MAX_CHARS: usize = 300;
const CF_UNICODETEXT: u32 = 13;
const TIMER_DEBOUNCE_ID: usize = 101;
const DEBOUNCE_MS: u32 = 150;
const STORE_FILENAME: &str = "dude_store.json";

#[derive(Clone, Serialize, Deserialize)]
pub struct ClipboardItem {
    pub id: String,
    pub text: String,
    #[serde(rename = "copiedAt")]
    pub copied_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClipboardItemPreview {
    pub id: String,
    pub preview: String,
    #[serde(rename = "copiedAt")]
    pub copied_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClipboardState {
    pub enabled: bool,
    pub items: Vec<ClipboardItemPreview>,
    #[serde(rename = "currentCopiedId")]
    pub current_copied_id: Option<String>,
}

#[derive(Clone, Serialize)]
struct ClipboardUpdatedPayload {
    count: usize,
}

struct ManagerState {
    enabled: bool,
    items: Vec<ClipboardItem>,
    current_copied_id: Option<String>,
    next_id: u64,
}

static MANAGER: OnceLock<Mutex<ManagerState>> = OnceLock::new();
static LISTENER_HWND: AtomicIsize = AtomicIsize::new(0);
static LISTENER_RUNNING: AtomicBool = AtomicBool::new(false);
static DUDE_LAST_COPIED_HASH: AtomicU64 = AtomicU64::new(0);
static SKIP_NEXT_HASH: AtomicU64 = AtomicU64::new(0);
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
static DEBUG_PREVIEW_ACTIVE: AtomicBool = AtomicBool::new(false);

fn get_manager() -> &'static Mutex<ManagerState> {
    MANAGER.get_or_init(|| {
        Mutex::new(ManagerState {
            enabled: false,
            items: Vec::new(),
            current_copied_id: None,
            next_id: 1,
        })
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn simple_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// Check for secrets and sensitive tokens
fn looks_like_secret(text: &str) -> bool {
    let t = text.trim();
    if t.contains("AIza") || t.contains("BEGIN PRIVATE KEY") {
        return true;
    }
    if t.contains("sk-") || t.contains("ghp_") || t.contains("AQ") {
        return true;
    }
    if let Some(pos) = t.find("Bearer ") {
        let rest = t[pos + 7..].trim_start();
        let token = rest.split_whitespace().next().unwrap_or("");
        if token.len() > 20 {
            return true;
        }
    }

    // Single token without spaces of >= 40 chars that looks random
    for token in t.split_whitespace() {
        if token.len() >= 40 {
            let mut has_upper = false;
            let mut has_lower = false;
            let mut has_digit = false;
            let mut has_symbol = false;
            let mut char_set = std::collections::HashSet::new();

            for c in token.chars() {
                if c.is_ascii_uppercase() {
                    has_upper = true;
                } else if c.is_ascii_lowercase() {
                    has_lower = true;
                } else if c.is_ascii_digit() {
                    has_digit = true;
                } else if c.is_ascii_punctuation() {
                    has_symbol = true;
                }
                char_set.insert(c);
            }

            let classes = (has_upper as u8) + (has_lower as u8) + (has_digit as u8) + (has_symbol as u8);
            if classes >= 3 && char_set.len() >= 16 {
                return true;
            }
            if (has_digit && (has_lower || has_upper)) && char_set.len() >= 14 && token.chars().all(|c| c.is_ascii_hexdigit()) {
                return true;
            }
        }
    }

    false
}

fn emit_update_event(count: usize) {
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit("clipboard-updated", ClipboardUpdatedPayload { count });
    }
}

fn mark_clipboard_unmatched() {
    if let Ok(mut mgr) = get_manager().lock() {
        if mgr.current_copied_id.is_some() {
            mgr.current_copied_id = None;
            let count = mgr.items.len();
            drop(mgr);
            emit_update_event(count);
        }
    }
}

pub fn move_to_top_or_add(text: &str, is_dude_self_write: bool) {
    let mut mgr = match get_manager().lock() {
        Ok(m) => m,
        Err(_) => return,
    };

    if !mgr.enabled && !is_dude_self_write {
        return;
    }

    let timestamp = now_ms();
    let truncated_text: String = if text.len() > MAX_ITEM_TEXT_LEN {
        text.chars().take(MAX_ITEM_TEXT_LEN).collect()
    } else {
        text.to_string()
    };

    // Dedupe: if text equals existing item, move to top and update time
    let item_id = if let Some(pos) = mgr.items.iter().position(|item| item.text == truncated_text) {
        let mut item = mgr.items.remove(pos);
        item.copied_at = timestamp;
        let id = item.id.clone();
        mgr.items.insert(0, item);
        id
    } else {
        let id = format!("clip_{}_{}", timestamp, mgr.next_id);
        mgr.next_id += 1;
        mgr.items.insert(0, ClipboardItem {
            id: id.clone(),
            text: truncated_text,
            copied_at: timestamp,
        });
        id
    };

    mgr.current_copied_id = Some(item_id);

    if mgr.items.len() > MAX_ITEMS {
        mgr.items.truncate(MAX_ITEMS);
    }

    let count = mgr.items.len();
    drop(mgr);

    emit_update_event(count);
}

// Window proc for hidden message-only window
unsafe extern "system" fn clipboard_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            // Debounce with Win32 timer
            let _ = SetTimer(hwnd, TIMER_DEBOUNCE_ID, DEBOUNCE_MS, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_DEBOUNCE_ID {
                let _ = KillTimer(hwnd, TIMER_DEBOUNCE_ID);
                process_clipboard_update(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = RemoveClipboardFormatListener(hwnd);
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            LISTENER_HWND.store(0, Ordering::SeqCst);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// Read clipboard safely with retries and exclusions
fn process_clipboard_update(hwnd: HWND) {
    let enabled = match get_manager().lock() {
        Ok(m) => m.enabled,
        Err(_) => false,
    };
    if !enabled {
        return;
    }

    // Try OpenClipboard with up to 5 retries (20ms each)
    let mut opened = false;
    for _ in 0..5 {
        if unsafe { OpenClipboard(hwnd) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !opened {
        return;
    }

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    let _guard = ClipboardGuard;

    unsafe {
        // 1. Check password manager exclusions
        let fmt_exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if fmt_exclude != 0 && IsClipboardFormatAvailable(fmt_exclude).is_ok() {
            mark_clipboard_unmatched();
            return;
        }

        let fmt_history = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        if fmt_history != 0 && IsClipboardFormatAvailable(fmt_history).is_ok() {
            if let Ok(handle) = GetClipboardData(fmt_history) {
                if !handle.is_invalid() {
                    let ptr = GlobalLock(HGLOBAL(handle.0));
                    if !ptr.is_null() {
                        let val = *(ptr as *const u32);
                        let _ = GlobalUnlock(HGLOBAL(handle.0));
                        if val == 0 {
                            mark_clipboard_unmatched();
                            return;
                        }
                    }
                }
            }
        }

        let fmt_cloud = RegisterClipboardFormatW(w!("CanUploadToCloudClipboard"));
        if fmt_cloud != 0 && IsClipboardFormatAvailable(fmt_cloud).is_ok() {
            if let Ok(handle) = GetClipboardData(fmt_cloud) {
                if !handle.is_invalid() {
                    let ptr = GlobalLock(HGLOBAL(handle.0));
                    if !ptr.is_null() {
                        let val = *(ptr as *const u32);
                        let _ = GlobalUnlock(HGLOBAL(handle.0));
                        if val == 0 {
                            mark_clipboard_unmatched();
                            return;
                        }
                    }
                }
            }
        }

        // 2. Must be text
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            mark_clipboard_unmatched();
            return;
        }

        let handle = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                mark_clipboard_unmatched();
                return;
            }
        };

        let ptr = GlobalLock(HGLOBAL(handle.0));
        if ptr.is_null() {
            mark_clipboard_unmatched();
            return;
        }

        let u16_ptr = ptr as *const u16;
        let mut len = 0;
        while *u16_ptr.add(len) != 0 {
            len += 1;
            if len > MAX_MONITOR_TEXT_LEN + 1 {
                break;
            }
        }

        let slice = std::slice::from_raw_parts(u16_ptr, len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(HGLOBAL(handle.0));

        // 3. Length and emptiness checks
        let trimmed = text.trim();
        if trimmed.is_empty() || text.len() > MAX_MONITOR_TEXT_LEN {
            mark_clipboard_unmatched();
            return;
        }

        // 4. Secret patterns check
        if looks_like_secret(&text) {
            mark_clipboard_unmatched();
            return;
        }

        // 5. Check if this is Dude's own write (token matching)
        let hash = simple_hash(&text);
        let skip_hash = SKIP_NEXT_HASH.load(Ordering::SeqCst);
        if skip_hash != 0 && skip_hash == hash {
            SKIP_NEXT_HASH.store(0, Ordering::SeqCst);
            return;
        }

        let last_dude = DUDE_LAST_COPIED_HASH.load(Ordering::SeqCst);
        if last_dude != 0 && last_dude == hash {
            DUDE_LAST_COPIED_HASH.store(0, Ordering::SeqCst);
            move_to_top_or_add(&text, true);
            return;
        }

        move_to_top_or_add(&text, false);
    }
}

// Background thread running the message-only window
fn start_listener_thread() {
    if LISTENER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // Already running
    }

    std::thread::Builder::new()
        .name("dude-clipboard-monitor".into())
        .spawn(move || {
            unsafe {
                let class_name = w!("DudeClipboardListenerClass");
                let wnd_class = WNDCLASSW {
                    lpfnWndProc: Some(clipboard_wnd_proc),
                    hInstance: HINSTANCE(std::ptr::null_mut()),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wnd_class);

                let hwnd = match CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class_name,
                    w!("DudeClipboardListener"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    None,
                    HINSTANCE(std::ptr::null_mut()),
                    None,
                ) {
                    Ok(h) => h,
                    Err(_) => {
                        LISTENER_RUNNING.store(false, Ordering::SeqCst);
                        return;
                    }
                };

                LISTENER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

                let _ = AddClipboardFormatListener(hwnd);

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                LISTENER_HWND.store(0, Ordering::SeqCst);
                LISTENER_RUNNING.store(false, Ordering::SeqCst);
            }
        })
        .expect("failed to spawn clipboard listener thread");
}

fn stop_listener_thread() {
    if !LISTENER_RUNNING.load(Ordering::SeqCst) {
        return;
    }
    let h_raw = LISTENER_HWND.load(Ordering::SeqCst);
    if h_raw != 0 {
        let hwnd = HWND(h_raw as *mut _);
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn init(app: &AppHandle) {
    let _ = APP_HANDLE.set(app.clone());

    // Read clipboard.enabled from store (default false)
    let enabled = app
        .store(STORE_FILENAME)
        .ok()
        .and_then(|s| {
            s.get("clipboard.enabled")
                .or_else(|| s.get("clipboard").and_then(|c| c.get("enabled").cloned()))
        })
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Ok(mut mgr) = get_manager().lock() {
        mgr.enabled = enabled;
    }

    if enabled {
        start_listener_thread();
    }
}

// Tauri Commands

#[tauri::command]
pub fn get_clipboard_state() -> ClipboardState {
    if DEBUG_PREVIEW_ACTIVE.load(Ordering::SeqCst) {
        let now = now_ms();
        return ClipboardState {
            enabled: true,
            current_copied_id: Some("preview_0".into()),
            items: vec![
                ClipboardItemPreview {
                    id: "preview_0".into(),
                    preview: "npm.cmd run tauri dev".into(),
                    copied_at: now,
                },
                ClipboardItemPreview {
                    id: "preview_1".into(),
                    preview: "github.com/shubham7-dev/Dude".into(),
                    copied_at: now.saturating_sub(120_000), // 2 min ago
                },
                ClipboardItemPreview {
                    id: "preview_2".into(),
                    preview: "Hello, this is copied text....".into(),
                    copied_at: now.saturating_sub(300_000), // 5 min ago
                },
                ClipboardItemPreview {
                    id: "preview_3".into(),
                    preview: "const app = createApp()".into(),
                    copied_at: now.saturating_sub(480_000), // 8 min ago
                },
            ],
        };
    }

    let mgr = match get_manager().lock() {
        Ok(m) => m,
        Err(_) => {
            return ClipboardState {
                enabled: false,
                current_copied_id: None,
                items: Vec::new(),
            }
        }
    };

    let items = mgr
        .items
        .iter()
        .map(|item| {
            let preview: String = if item.text.chars().count() > PREVIEW_MAX_CHARS {
                item.text.chars().take(PREVIEW_MAX_CHARS).collect()
            } else {
                item.text.clone()
            };
            ClipboardItemPreview {
                id: item.id.clone(),
                preview,
                copied_at: item.copied_at,
            }
        })
        .collect();

    ClipboardState {
        enabled: mgr.enabled,
        items,
        current_copied_id: mgr.current_copied_id.clone(),
    }
}

#[tauri::command]
pub fn copy_clipboard_item(id: String) -> Result<(), String> {
    if DEBUG_PREVIEW_ACTIVE.load(Ordering::SeqCst) {
        return Ok(());
    }

    let mut mgr = get_manager().lock().map_err(|e| e.to_string())?;
    let pos = mgr
        .items
        .iter()
        .position(|i| i.id == id)
        .ok_or_else(|| "Item not found".to_string())?;

    let mut item = mgr.items.remove(pos);
    item.copied_at = now_ms();
    let text = item.text.clone();
    let item_id = item.id.clone();
    mgr.items.insert(0, item);
    mgr.current_copied_id = Some(item_id);
    drop(mgr);

    // Set token for Dude's write
    let hash = simple_hash(&text);
    DUDE_LAST_COPIED_HASH.store(hash, Ordering::SeqCst);

    // Write to Windows clipboard directly via Win32 API
    write_to_windows_clipboard(&text)?;

    emit_update_event(get_manager().lock().map(|m| m.items.len()).unwrap_or(0));
    Ok(())
}

fn write_to_windows_clipboard(text: &str) -> Result<(), String> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);

    unsafe {
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return Err("Failed to open clipboard".to_string());
        }

        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseClipboard();
                }
            }
        }
        let _g = Guard;

        let _ = EmptyClipboard();

        let bytes = utf16.len() * std::mem::size_of::<u16>();
        let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
        if h_mem.is_invalid() {
            return Err("Failed to allocate global memory".to_string());
        }

        let ptr = GlobalLock(h_mem);
        if ptr.is_null() {
            return Err("Failed to lock global memory".to_string());
        }

        std::ptr::copy_nonoverlapping(utf16.as_ptr() as *const u8, ptr as *mut u8, bytes);
        let _ = GlobalUnlock(h_mem);

        let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(h_mem.0));
    }
    Ok(())
}

pub fn write_clipboard_skipping_history(text: &str) -> Result<(), String> {
    let hash = simple_hash(text);
    SKIP_NEXT_HASH.store(hash, Ordering::SeqCst);
    write_to_windows_clipboard(text)
}

pub fn read_windows_clipboard_text() -> Result<String, String> {
    unsafe {
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return Err("Failed to open clipboard".to_string());
        }

        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseClipboard();
                }
            }
        }
        let _g = Guard;

        let handle = GetClipboardData(CF_UNICODETEXT).map_err(|e| e.to_string())?;
        if handle.is_invalid() {
            return Err("No unicode text in clipboard".to_string());
        }

        let ptr = GlobalLock(HGLOBAL(handle.0));
        if ptr.is_null() {
            return Err("Failed to lock clipboard global memory".to_string());
        }

        let u16_ptr = ptr as *const u16;
        let mut len = 0;
        while *u16_ptr.add(len) != 0 && len < 20000 {
            len += 1;
        }

        let slice = std::slice::from_raw_parts(u16_ptr, len);
        let s = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(HGLOBAL(handle.0));
        Ok(s)
    }
}

pub fn clear_windows_clipboard_if_matches(text: &str) -> Result<(), String> {
    if let Ok(curr) = read_windows_clipboard_text() {
        if curr == text {
            unsafe {
                if OpenClipboard(HWND(std::ptr::null_mut())).is_ok() {
                    let _ = EmptyClipboard();
                    let _ = CloseClipboard();
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn delete_clipboard_item(id: String) -> Result<(), String> {
    let mut mgr = get_manager().lock().map_err(|e| e.to_string())?;
    if let Some(pos) = mgr.items.iter().position(|i| i.id == id) {
        mgr.items.remove(pos);
    }
    if mgr.current_copied_id.as_deref() == Some(&id) {
        mgr.current_copied_id = None;
    }
    let count = mgr.items.len();
    drop(mgr);

    emit_update_event(count);
    Ok(())
}

#[tauri::command]
pub fn clear_clipboard_history() -> Result<(), String> {
    let mut mgr = get_manager().lock().map_err(|e| e.to_string())?;
    mgr.items.clear();
    mgr.current_copied_id = None;
    drop(mgr);

    emit_update_event(0);
    Ok(())
}

#[tauri::command]
pub fn set_clipboard_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    // Save to store
    if let Ok(store) = app.store(STORE_FILENAME) {
        store.set("clipboard.enabled", serde_json::Value::Bool(enabled));
        let _ = store.save();
    }

    let mut mgr = get_manager().lock().map_err(|e| e.to_string())?;
    mgr.enabled = enabled;
    if !enabled {
        mgr.items.clear();
        mgr.current_copied_id = None;
    }
    drop(mgr);

    if enabled {
        start_listener_thread();
    } else {
        stop_listener_thread();
    }

    emit_update_event(0);
    Ok(())
}

#[tauri::command]
pub fn toggle_clipboard_debug_preview() -> bool {
    #[cfg(debug_assertions)]
    {
        let prev = DEBUG_PREVIEW_ACTIVE.fetch_xor(true, Ordering::SeqCst);
        let active = !prev;
        emit_update_event(if active { 4 } else { 0 });
        active
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_patterns_exclusion() {
        assert!(looks_like_secret("AIzaSyFakeKey1234567890"));
        assert!(looks_like_secret("sk-proj-1234567890abcdef"));
        assert!(looks_like_secret("ghp_1234567890abcdef1234567890abcdef"));
        assert!(looks_like_secret("AQ123456789012345678"));
        assert!(looks_like_secret("Bearer 123456789012345678901234567890"));
        assert!(looks_like_secret("-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0B"));
        // Random looking token of 40+ chars with mixed types
        assert!(looks_like_secret("aB3$kL9#pQ2!vW8^xY1*zM7@tN4&rK5~hJ6+gF0="));

        // Normal text should not be flagged
        assert!(!looks_like_secret("npm.cmd run tauri dev"));
        assert!(!looks_like_secret("github.com/shubham7-dev/Dude"));
        assert!(!looks_like_secret("Hello, this is copied text...."));
        assert!(!looks_like_secret("const app = createApp()"));
    }

    #[test]
    fn test_bounded_capacity_and_deduplication() {
        // Clear manager
        {
            let mut mgr = get_manager().lock().unwrap();
            mgr.enabled = true;
            mgr.items.clear();
            mgr.current_copied_id = None;
        }

        // Add 15 unique items
        for i in 1..=15 {
            move_to_top_or_add(&format!("item {}", i), false);
        }

        let state = get_clipboard_state();
        assert_eq!(state.items.len(), 11, "Must be bounded to max 11 items");
        assert_eq!(state.items[0].preview, "item 15", "Latest must be on top");
        assert_eq!(state.items[10].preview, "item 5", "Oldest kept must be item 5");

        // Deduplication test: re-add "item 5"
        let item5_id = state.items[10].id.clone();
        move_to_top_or_add("item 5", false);

        let state_after = get_clipboard_state();
        assert_eq!(state_after.items.len(), 11, "Count must remain 11");
        assert_eq!(state_after.items[0].preview, "item 5", "Deduplicated item moved to top");
        assert_eq!(state_after.items[0].id, item5_id, "Item ID preserved on move-to-top");
        assert_eq!(state_after.current_copied_id, Some(item5_id));

        // Clear after test
        let _ = clear_clipboard_history();
        let empty_state = get_clipboard_state();
        assert_eq!(empty_state.items.len(), 0);
    }
}
