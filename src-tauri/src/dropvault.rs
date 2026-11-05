// Dude - DropVault Module
// Secure local file repository in Documents\DropVault

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;

pub const MAX_VAULT_FILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB
pub const MAX_DROP_FILES: usize = 20;
const STORE_FILENAME: &str = "dude_store.json";
const STORE_KEY: &str = "dropvault.path";
const INDEX_FILENAME: &str = ".dropvault-index.json";

// In-memory mapping from opaque session ID to canonical PathBuf
static VAULT_SESSION_MAP: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
static WATCHER_RUNNING: AtomicBool = AtomicBool::new(false);

fn session_map() -> &'static Mutex<HashMap<String, PathBuf>> {
    VAULT_SESSION_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStatus {
    pub state: String, // "not_created" | "ready" | "missing"
    pub display: String, // e.g. "Documents\\DropVault"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFileItem {
    pub id: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub color_index: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSaveResult {
    pub count: usize,
    pub saved: usize,
    pub rejected: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultIndexRecord {
    pub added_at: u64,
}

// Win32 FFI definitions
#[repr(C)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const FOLDERID_DOCUMENTS: GUID = GUID {
    data1: 0xFDD39AD0,
    data2: 0x238F,
    data3: 0x46AF,
    data4: [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
};

#[repr(C)]
struct FILETIME {
    dw_low: u32,
    dw_high: u32,
}

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        rfid: *const GUID,
        dw_flags: u32,
        h_token: *mut std::ffi::c_void,
        ppsz_path: *mut *mut u16,
    ) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(pv: *mut std::ffi::c_void);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetDiskFreeSpaceExW(
        lp_directory_name: *const u16,
        lp_free_bytes_available: *mut u64,
        lp_total_number_of_bytes: *mut u64,
        lp_total_number_of_free_bytes: *mut u64,
    ) -> i32;
    fn GetSystemTimeAsFileTime(lp_system_time: *mut FILETIME);
    fn SetFileTime(
        h_file: *mut std::ffi::c_void,
        lp_creation_time: *const FILETIME,
        lp_last_access_time: *const FILETIME,
        lp_last_write_time: *const FILETIME,
    ) -> i32;
    fn FindFirstChangeNotificationW(
        lp_path_name: *const u16,
        b_watch_subtree: i32,
        dw_notify_filter: u32,
    ) -> *mut std::ffi::c_void;
    fn FindNextChangeNotification(h_change_handle: *mut std::ffi::c_void) -> i32;
    fn FindCloseChangeNotification(h_change_handle: *mut std::ffi::c_void) -> i32;
    fn WaitForSingleObject(h_handle: *mut std::ffi::c_void, dw_milliseconds: u32) -> u32;
}

const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x00000001;
const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x00000010;
const WAIT_OBJECT_0: u32 = 0;
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;

/// Resolves user's Documents folder using SHGetKnownFolderPath
pub fn get_documents_path() -> PathBuf {
    unsafe {
        let mut path_ptr: *mut u16 = std::ptr::null_mut();
        if SHGetKnownFolderPath(&FOLDERID_DOCUMENTS, 0, std::ptr::null_mut(), &mut path_ptr) == 0
            && !path_ptr.is_null()
        {
            let mut len = 0;
            while *path_ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(path_ptr, len);
            let os_str = OsString::from_wide(slice);
            CoTaskMemFree(path_ptr as *mut std::ffi::c_void);
            return PathBuf::from(os_str);
        }
    }

    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let doc = PathBuf::from(&userprofile).join("Documents");
        if doc.exists() {
            return doc;
        }
        return PathBuf::from(userprofile);
    }

    PathBuf::from(".")
}

/// Short human-readable display string without revealing sensitive user profile path
fn to_short_display(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.contains("Documents") {
        return "Documents\\DropVault".to_string();
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "DropVault".to_string())
}

/// Reads saved vault path from app store
pub fn get_configured_vault_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(store) = app.store(STORE_FILENAME) {
        if let Some(val) = store.get(STORE_KEY) {
            if let Some(s) = val.as_str() {
                if !s.trim().is_empty() {
                    return Some(PathBuf::from(s.trim()));
                }
            }
        }
    }
    None
}

/// Saves vault path to app store
fn save_configured_vault_path(app: &AppHandle, path: &Path) -> Result<(), String> {
    let store = app.store(STORE_FILENAME).map_err(|e| e.to_string())?;
    store.set(
        STORE_KEY.to_string(),
        serde_json::Value::String(path.to_string_lossy().to_string()),
    );
    let _ = store.save();
    Ok(())
}

/// Query current vault status
#[tauri::command]
pub fn vault_status(app: AppHandle) -> VaultStatus {
    if let Some(configured) = get_configured_vault_path(&app) {
        if configured.is_dir() {
            return VaultStatus {
                state: "ready".to_string(),
                display: to_short_display(&configured),
            };
        } else {
            return VaultStatus {
                state: "missing".to_string(),
                display: to_short_display(&configured),
            };
        }
    }

    let default_docs = get_documents_path().join("DropVault");
    if default_docs.is_dir() {
        let _ = save_configured_vault_path(&app, &default_docs);
        return VaultStatus {
            state: "ready".to_string(),
            display: to_short_display(&default_docs),
        };
    }
    VaultStatus {
        state: "not_created".to_string(),
        display: to_short_display(&default_docs),
    }
}

/// Create or adopt DropVault folder
#[tauri::command]
pub fn create_vault(app: AppHandle) -> Result<VaultStatus, String> {
    let docs = get_documents_path();
    let target = docs.join("DropVault");

    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| format!("Failed to create folder: {}", e))?;
    }

    save_configured_vault_path(&app, &target)?;
    Ok(vault_status(app))
}

/// Open vault folder in Windows Explorer
#[tauri::command]
pub fn open_vault_folder(app: AppHandle) -> Result<(), String> {
    let target = if let Some(p) = get_configured_vault_path(&app) {
        if p.is_dir() {
            p
        } else {
            get_documents_path().join("DropVault")
        }
    } else {
        get_documents_path().join("DropVault")
    };

    if target.exists() {
        let _ = std::process::Command::new("explorer").arg(&target).spawn();
    }
    Ok(())
}

/// Stable hash of filename modulo 4 for color index (0: blue, 1: violet, 2: tan, 3: green)
fn file_name_color_index(name: &str) -> u8 {
    let mut h: u32 = 5381;
    for b in name.bytes() {
        h = ((h << 5).wrapping_add(h)).wrapping_add(b as u32);
    }
    (h % 4) as u8
}

/// Read index file mapping relative filename to added_at timestamp
fn read_index(vault_dir: &Path) -> HashMap<String, u64> {
    let index_path = vault_dir.join(INDEX_FILENAME);
    if let Ok(content) = std::fs::read_to_string(index_path) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, VaultIndexRecord>>(&content) {
            return map.into_iter().map(|(k, v)| (k, v.added_at)).collect();
        }
    }
    HashMap::new()
}

/// Write index file mapping relative filename to added_at timestamp atomically
fn write_index(vault_dir: &Path, map: &HashMap<String, u64>) {
    let index_path = vault_dir.join(INDEX_FILENAME);
    let tmp_path = vault_dir.join(format!("{}.tmp", INDEX_FILENAME));
    let records: HashMap<String, VaultIndexRecord> = map
        .iter()
        .map(|(k, v)| (k.clone(), VaultIndexRecord { added_at: *v }))
        .collect();
    if let Ok(json_str) = serde_json::to_string(&records) {
        if std::fs::write(&tmp_path, json_str).is_ok() {
            let _ = std::fs::rename(&tmp_path, &index_path);
        }
    }
}

/// Returns newest 6 files in DropVault folder
#[tauri::command]
pub fn vault_recent(app: AppHandle) -> Result<Vec<VaultFileItem>, String> {
    let vault_path = match get_configured_vault_path(&app) {
        Some(p) if p.is_dir() => p,
        _ => return Ok(Vec::new()),
    };

    let index = read_index(&vault_path);

    let read_dir = match std::fs::read_dir(&vault_path) {
        Ok(rd) => rd,
        Err(_) => return Ok(Vec::new()),
    };

    struct Candidate {
        path: PathBuf,
        name: String,
        ext: String,
        size: u64,
        created: u64,
        modified: u64,
        color_index: u8,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut scanned_count = 0;

    for entry in read_dir.flatten() {
        scanned_count += 1;
        if scanned_count > 5000 {
            break;
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name.starts_with('.') || file_name.ends_with(".tmp") || file_name.ends_with(".partial") {
            continue;
        }

        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Skip hidden, system, or reparse point files
        let attrs = meta.file_attributes();
        if (attrs & 0x00000002) != 0 || (attrs & 0x00000004) != 0 || (attrs & 0x00000400) != 0 {
            continue;
        }

        let size = meta.len();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let created_raw = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let created = index.get(&file_name).copied().unwrap_or(created_raw);
        let color_index = file_name_color_index(&file_name);

        candidates.push(Candidate {
            path,
            name: file_name,
            ext,
            size,
            created,
            modified,
            color_index,
        });
    }

    // Sort by created time descending, ties by modified descending
    candidates.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then_with(|| b.modified.cmp(&a.modified))
    });

    candidates.truncate(6);

    let mut map = session_map().lock().unwrap();
    map.clear();

    let mut items = Vec::new();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    for (idx, c) in candidates.into_iter().enumerate() {
        let id = format!("dv_{}_{}", now_ms, idx);
        if let Ok(canon) = c.path.canonicalize() {
            map.insert(id.clone(), canon);
        } else {
            map.insert(id.clone(), c.path);
        }

        items.push(VaultFileItem {
            id,
            name: c.name,
            ext: c.ext,
            size: c.size,
            color_index: c.color_index,
        });
    }

    Ok(items)
}

/// Dangerous executable extensions that must never be launched directly
const EXECUTABLE_EXTS: &[&str] = &[
    "exe", "msi", "msp", "bat", "cmd", "com", "scr", "pif", "ps1", "psm1", "vbs", "vbe", "js",
    "jse", "wsf", "wsh", "hta", "lnk", "url", "jar", "reg", "dll", "cpl", "msc", "appx", "msix",
    "application", "gadget", "inf",
];

/// Resolve opaque DropVault file ID to canonical PathBuf inside the vault
pub fn resolve_vault_path_for_id(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    let resolved_path = {
        let map = session_map().lock().unwrap();
        map.get(id).cloned().ok_or_else(|| "File not found in DropVault".to_string())?
    };

    let vault_dir = get_configured_vault_path(app)
        .ok_or_else(|| "DropVault is not configured".to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;

    let canon = resolved_path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path: {}", e))?;

    if !canon.starts_with(&vault_dir) {
        return Err("Path traversal rejected".to_string());
    }

    let meta = std::fs::symlink_metadata(&canon).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || (meta.file_attributes() & 0x400) != 0 {
        return Err("Reparse points are not supported".to_string());
    }

    if !canon.is_file() {
        return Err("Not a regular file".to_string());
    }

    Ok(canon)
}

/// Open a vault file or reveal in Explorer if executable
#[tauri::command]
pub fn vault_open(app: AppHandle, id: String) -> Result<String, String> {
    let canon = resolve_vault_path_for_id(&app, &id)?;

    let ext = canon
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if EXECUTABLE_EXTS.contains(&ext.as_str()) {
        // Show in Explorer for safety
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", canon.to_string_lossy()))
            .spawn();
        return Ok("shown".to_string());
    }

    // Open with default application via opener plugin
    let canon_str = canon.to_string_lossy().to_string();
    tauri_plugin_opener::open_path(canon_str, None::<&str>)
        .map_err(|e| format!("Failed to open file: {}", e))?;
    Ok("opened".to_string())
}

/// Reveal specific file in Explorer
#[tauri::command]
pub fn vault_show(app: AppHandle, id: String) -> Result<(), String> {
    let resolved_path = {
        let map = session_map().lock().unwrap();
        map.get(&id).cloned().ok_or_else(|| "File not found".to_string())?
    };

    let vault_dir = get_configured_vault_path(&app)
        .ok_or_else(|| "Vault not configured".to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;

    let canon = resolved_path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path: {}", e))?;

    if !canon.starts_with(&vault_dir) {
        return Err("Path traversal rejected".to_string());
    }

    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", canon.to_string_lossy()))
        .spawn();
    Ok(())
}

/// Check available free disk space on target directory in bytes
fn check_free_space(dir: &Path) -> Option<u64> {
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);

    let mut free_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free: u64 = 0;

    unsafe {
        if GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_available,
            &mut total_bytes,
            &mut total_free,
        ) != 0
        {
            Some(free_available)
        } else {
            None
        }
    }
}

/// Set creation time of file to now
fn touch_file_creation_time(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        unsafe {
            let mut ft = FILETIME { dw_low: 0, dw_high: 0 };
            GetSystemTimeAsFileTime(&mut ft);
            SetFileTime(
                file.as_raw_handle() as *mut std::ffi::c_void,
                &ft,
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }
}

/// Find unique destination path avoiding collisions: name (1).ext, name (2).ext
fn resolve_collision_path(vault_dir: &Path, original_name: &str) -> PathBuf {
    let p = Path::new(original_name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext_suffix = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e)).unwrap_or_default();

    let mut dest = vault_dir.join(original_name);
    let mut counter = 1;

    while dest.exists() {
        dest = vault_dir.join(format!("{} ({}){}", stem, counter, ext_suffix));
        counter += 1;
    }

    dest
}

/// Process files dropped onto the island into DropVault
pub fn save_to_vault_internal(app: &AppHandle, paths: &[PathBuf]) -> VaultSaveResult {
    let total_count = paths.len();
    if total_count == 0 {
        return VaultSaveResult {
            count: 0,
            saved: 0,
            rejected: Vec::new(),
        };
    }

    let vault_path = match get_configured_vault_path(app) {
        Some(p) if p.is_dir() => p,
        _ => {
            return VaultSaveResult {
                count: total_count,
                saved: 0,
                rejected: vec!["no-vault".to_string()],
            };
        }
    };

    let mut rejected = Vec::new();
    let mut valid_to_copy: Vec<(PathBuf, u64)> = Vec::new();
    let mut total_copy_bytes: u64 = 0;

    for (i, p) in paths.iter().enumerate() {
        if i >= MAX_DROP_FILES {
            rejected.push("too-many".to_string());
            continue;
        }

        let meta = match std::fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(_) => {
                rejected.push("denied".to_string());
                continue;
            }
        };

        if meta.file_type().is_dir() {
            rejected.push("folder".to_string());
            continue;
        }

        let len = meta.len();
        if len > MAX_VAULT_FILE_BYTES {
            rejected.push("too-large".to_string());
            continue;
        }

        total_copy_bytes = total_copy_bytes.saturating_add(len);
        valid_to_copy.push((p.clone(), len));
    }

    if valid_to_copy.is_empty() {
        return VaultSaveResult {
            count: total_count,
            saved: 0,
            rejected,
        };
    }

    // Check disk space
    if let Some(free_space) = check_free_space(&vault_path) {
        if free_space < total_copy_bytes.saturating_add(10 * 1024 * 1024) {
            rejected.push("disk".to_string());
            return VaultSaveResult {
                count: total_count,
                saved: 0,
                rejected,
            };
        }
    }

    let mut index = read_index(&vault_path);
    let mut saved_count = 0;
    let now_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for (src, _len) in valid_to_copy {
        let orig_name = match src.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                rejected.push("denied".to_string());
                continue;
            }
        };

        let dest = resolve_collision_path(&vault_path, &orig_name);
        let dest_filename = dest.file_name().and_then(|n| n.to_str()).unwrap_or(&orig_name).to_string();
        let partial_dest = vault_path.join(format!("{}.partial", dest_filename));

        // Copy preserving alternate data streams to .partial
        match std::fs::copy(&src, &partial_dest) {
            Ok(_) => {
                if let Err(_) = std::fs::rename(&partial_dest, &dest) {
                    let _ = std::fs::remove_file(&partial_dest);
                    rejected.push("denied".to_string());
                    continue;
                }
                touch_file_creation_time(&dest);
                index.insert(dest_filename, now_sec);
                saved_count += 1;
            }
            Err(_) => {
                let _ = std::fs::remove_file(&partial_dest);
                rejected.push("denied".to_string());
            }
        }
    }

    write_index(&vault_path, &index);

    VaultSaveResult {
        count: total_count,
        saved: saved_count,
        rejected,
    }
}

/// Background watcher for DropVault folder while panel is open
#[tauri::command]
pub fn start_vault_watcher(app: AppHandle, window: tauri::Window) -> Result<(), String> {
    if WATCHER_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let vault_path = match get_configured_vault_path(&app) {
        Some(p) if p.is_dir() => p,
        _ => {
            WATCHER_RUNNING.store(false, Ordering::SeqCst);
            return Ok(());
        }
    };

    std::thread::spawn(move || {
        let mut wide: Vec<u16> = vault_path.as_os_str().encode_wide().collect();
        wide.push(0);

        unsafe {
            let handle = FindFirstChangeNotificationW(
                wide.as_ptr(),
                0,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
            );

            if handle == INVALID_HANDLE_VALUE || handle.is_null() {
                WATCHER_RUNNING.store(false, Ordering::SeqCst);
                return;
            }

            while WATCHER_RUNNING.load(Ordering::SeqCst) {
                let wait_res = WaitForSingleObject(handle, 400);
                if !WATCHER_RUNNING.load(Ordering::SeqCst) {
                    break;
                }

                if wait_res == WAIT_OBJECT_0 {
                    // Debounce by 300ms
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = window.emit("vault-changed", ());
                    let _ = FindNextChangeNotification(handle);
                }
            }

            FindCloseChangeNotification(handle);
        }
        WATCHER_RUNNING.store(false, Ordering::SeqCst);
    });

    Ok(())
}

/// Stop background watcher when panel closes or window is hidden
#[tauri::command]
pub fn stop_vault_watcher() -> Result<(), String> {
    WATCHER_RUNNING.store(false, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_name_color_index_range_and_stability() {
        for name in &["drop-folder-dropped.png", "drop-file-dropped.png", "Desktop - 57", "Lavender AI", "test.docx"] {
            let idx = file_name_color_index(name);
            assert!(idx < 4, "color index must be in 0..4");
            // Deterministic
            assert_eq!(idx, file_name_color_index(name));
        }
    }

    #[test]
    fn test_executable_extensions_blocked() {
        for ext in &["exe", "msi", "bat", "cmd", "ps1", "vbs", "js", "jar", "reg", "dll"] {
            assert!(EXECUTABLE_EXTS.contains(ext), "{} must be treated as executable", ext);
        }
        for safe in &["png", "jpg", "pdf", "docx", "txt", "zip", "md"] {
            assert!(!EXECUTABLE_EXTS.contains(safe), "{} should be safe", safe);
        }
    }

    #[test]
    fn test_resolve_collision_path_naming() {
        let temp_dir = std::env::temp_dir().join("dude_dropvault_test_collision");
        let _ = std::fs::create_dir_all(&temp_dir);

        let p1 = resolve_collision_path(&temp_dir, "notes.txt");
        assert_eq!(p1, temp_dir.join("notes.txt"));

        // Create the file and check collision resolution
        let _ = std::fs::File::create(&p1);
        let p2 = resolve_collision_path(&temp_dir, "notes.txt");
        assert_eq!(p2, temp_dir.join("notes (1).txt"));

        let _ = std::fs::File::create(&p2);
        let p3 = resolve_collision_path(&temp_dir, "notes.txt");
        assert_eq!(p3, temp_dir.join("notes (2).txt"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_index_read_write_roundtrip() {
        let temp_dir = std::env::temp_dir().join("dude_dropvault_test_index");
        let _ = std::fs::create_dir_all(&temp_dir);

        let mut sample_index = HashMap::new();
        sample_index.insert("photo.png".to_string(), 1700000000);
        sample_index.insert("doc.pdf".to_string(), 1700000100);

        write_index(&temp_dir, &sample_index);
        let loaded = read_index(&temp_dir);

        assert_eq!(loaded.get("photo.png"), Some(&1700000000));
        assert_eq!(loaded.get("doc.pdf"), Some(&1700000100));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_session_map_lookup() {
        let temp_dir = std::env::temp_dir().join("dude_dropvault_test_lookup");
        let _ = std::fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("sample.png");
        let _ = std::fs::File::create(&test_file);

        let test_id = "test_item_999";
        {
            let mut map = session_map().lock().unwrap();
            map.insert(test_id.to_string(), test_file.clone());
        }

        {
            let map = session_map().lock().unwrap();
            assert_eq!(map.get(test_id), Some(&test_file));
            assert_eq!(map.get("non_existent_id"), None);
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

