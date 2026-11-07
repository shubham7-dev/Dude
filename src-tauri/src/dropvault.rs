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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;

pub const MAX_VAULT_FILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB
pub const MAX_DROP_FILES: usize = 20;
const STORE_FILENAME: &str = "dude_store.json";
const STORE_KEY: &str = "dropvault.path";
const STORE_TEST_KEY: &str = "dropvault.test_path";
const INDEX_FILENAME: &str = ".dropvault-index.json";

// Global vault operation lock & cancellation flag
static VAULT_OPERATION_LOCK: AtomicBool = AtomicBool::new(false);
static CANCEL_MOVE_REQUESTED: AtomicBool = AtomicBool::new(false);
static LAST_OLD_VAULT_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

// In-memory mapping from opaque session ID to canonical PathBuf
static VAULT_SESSION_MAP: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
static WATCHER_RUNNING: AtomicBool = AtomicBool::new(false);

fn session_map() -> &'static Mutex<HashMap<String, PathBuf>> {
    VAULT_SESSION_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

#[allow(dead_code)]
struct PendingLocation {
    token: String,
    parent_path: PathBuf,
    target_vault: PathBuf,
    adopt: bool,
    move_count: usize,
    move_bytes: u64,
    created_at: Instant,
}

static PENDING_LOCATIONS: OnceLock<Mutex<HashMap<String, PendingLocation>>> = OnceLock::new();

fn pending_locations() -> &'static Mutex<HashMap<String, PendingLocation>> {
    PENDING_LOCATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStatus {
    pub state: String, // "not_created" | "ready" | "missing"
    pub display: String, // e.g. "Documents\\DropVault"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStats {
    pub count: usize,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPickResult {
    pub token: String,
    #[serde(rename = "parentDisplay")]
    pub parent_display: String,
    #[serde(rename = "vaultDisplay")]
    pub vault_display: String,
    pub adopt: bool,
    #[serde(rename = "moveCount")]
    pub move_count: usize,
    #[serde(rename = "moveBytes")]
    pub move_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultApplyResult {
    pub moved: usize,
    pub failed: usize,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMoveProgress {
    pub current: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFileItem {
    pub id: String,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub color_index: u8,
    pub path: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanPreviewResult {
    pub token: String,
    pub count: usize,
    pub bytes: u64,
    pub oldest_age_days: u64,
    pub skipped_folders: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanApplyResult {
    pub moved: usize,
    pub moved_bytes: u64,
    pub skipped: usize,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
struct CleanCandidate {
    path: PathBuf,
    filename: String,
    size: u64,
    modified: SystemTime,
    last_activity_sec: u64,
}

struct CleanPreviewSession {
    created_at: Instant,
    days_threshold: u32,
    candidates: Vec<CleanCandidate>,
}

static CLEAN_PREVIEWS: OnceLock<Mutex<HashMap<String, CleanPreviewSession>>> = OnceLock::new();
static CANCEL_CLEAN_REQUESTED: AtomicBool = AtomicBool::new(false);

fn clean_previews() -> &'static Mutex<HashMap<String, CleanPreviewSession>> {
    CLEAN_PREVIEWS.get_or_init(|| Mutex::new(HashMap::new()))
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

#[repr(C)]
#[allow(non_snake_case)]
struct SHQUERYRBINFO {
    cbSize: u32,
    i64Size: i64,
    i64NumItems: i64,
}

#[repr(C)]
#[allow(non_snake_case)]
struct SHFILEOPSTRUCTW {
    hwnd: *mut std::ffi::c_void,
    wFunc: u32,
    pFrom: *const u16,
    pTo: *const u16,
    fFlags: u16,
    fAnyOperationsAborted: i32,
    hNameMappings: *mut std::ffi::c_void,
    lpszProgressTitle: *const u16,
}

const FO_DELETE: u32 = 0x0003;
const FOF_SILENT: u16 = 0x0004;
const FOF_NOCONFIRMATION: u16 = 0x0010;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOERRORUI: u16 = 0x0400;

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        rfid: *const GUID,
        dw_flags: u32,
        h_token: *mut std::ffi::c_void,
        ppsz_path: *mut *mut u16,
    ) -> i32;
    fn SHQueryRecycleBinW(pszRootPath: *const u16, pSHQueryRBInfo: *mut SHQUERYRBINFO) -> i32;
    fn SHFileOperationW(lpFileOp: *mut SHFILEOPSTRUCTW) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(pv: *mut std::ffi::c_void);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
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
    fn SetFileAttributesW(lpFileName: *const u16, dwFileAttributes: u32) -> i32;
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

/// Returns true if test mode is active via DUDE_VAULT_TEST=1 in debug builds
pub fn is_vault_test_mode() -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var("DUDE_VAULT_TEST").map(|v| v == "1").unwrap_or(false)
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

pub fn get_store_key() -> &'static str {
    if is_vault_test_mode() {
        STORE_TEST_KEY
    } else {
        STORE_KEY
    }
}

pub fn get_default_vault_path() -> PathBuf {
    if is_vault_test_mode() {
        std::env::temp_dir().join("DudeVaultTest").join("DropVault")
    } else {
        get_documents_path().join("DropVault")
    }
}

/// Shared human-readable display string without revealing sensitive user profile path
pub fn format_short_path_display(path: &Path) -> String {
    let target_str = path.to_string_lossy().to_string();
    let target_lower = target_str.to_lowercase();

    let docs_path = get_documents_path();
    let docs_str = docs_path.to_string_lossy().to_string();
    let docs_lower = docs_str.to_lowercase();

    if target_lower == docs_lower || target_lower.starts_with(&(docs_lower.clone() + "\\")) {
        let rel = target_str[docs_str.len()..].trim_start_matches('\\');
        if rel.is_empty() {
            return "Documents".to_string();
        } else {
            return format!("Documents\\{}", rel);
        }
    }

    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let prof_lower = userprofile.to_lowercase();
        if target_lower == prof_lower || target_lower.starts_with(&(prof_lower.clone() + "\\")) {
            let rel = target_str[userprofile.len()..].trim_start_matches('\\');
            if rel.is_empty() {
                return "~".to_string();
            } else {
                return format!("~\\{}", rel);
            }
        }
    }

    target_str
}

/// Reads saved vault path from app store
pub fn get_configured_vault_path(app: &AppHandle) -> Option<PathBuf> {
    let key = get_store_key();
    if let Ok(store) = app.store(STORE_FILENAME) {
        if let Some(val) = store.get(key) {
            if let Some(s) = val.as_str() {
                if !s.trim().is_empty() {
                    return Some(PathBuf::from(s.trim()));
                }
            }
        }
    }
    None
}

/// Checks if a given path resides inside the vault directory (case-insensitive on Windows)
pub fn is_path_in_vault(path: &Path, vault_path: &Path) -> bool {
    // 1. Check canonicalized paths
    if let (Ok(canon_p), Ok(canon_v)) = (path.canonicalize(), vault_path.canonicalize()) {
        if canon_p.starts_with(&canon_v) {
            return true;
        }
        #[cfg(windows)]
        {
            let p_lower = PathBuf::from(canon_p.to_string_lossy().to_lowercase());
            let v_lower = PathBuf::from(canon_v.to_string_lossy().to_lowercase());
            if p_lower.starts_with(&v_lower) {
                return true;
            }
        }
    }

    // 2. Fallback without canonicalize (handles non-existent files or uncanonicalized strings)
    let p_norm = path.to_string_lossy().replace('/', "\\");
    let v_norm = vault_path.to_string_lossy().replace('/', "\\");
    let p_clean = p_norm.trim_start_matches(r"\\?\");
    let v_clean = v_norm.trim_start_matches(r"\\?\");

    #[cfg(windows)]
    let (p_cmp, v_cmp) = (p_clean.to_lowercase(), v_clean.to_lowercase());
    #[cfg(not(windows))]
    let (p_cmp, v_cmp) = (p_clean.to_string(), v_clean.to_string());

    let p_path = Path::new(&p_cmp);
    let v_path = Path::new(&v_cmp);

    p_path.starts_with(v_path)
}

/// Saves vault path to app store
fn save_configured_vault_path(app: &AppHandle, path: &Path) -> Result<(), String> {
    let key = get_store_key();
    let store = app.store(STORE_FILENAME).map_err(|e| e.to_string())?;
    store.set(
        key.to_string(),
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
                display: format_short_path_display(&configured),
            };
        } else {
            return VaultStatus {
                state: "missing".to_string(),
                display: format_short_path_display(&configured),
            };
        }
    }

    let default_vault = get_default_vault_path();
    if default_vault.is_dir() {
        let _ = save_configured_vault_path(&app, &default_vault);
        return VaultStatus {
            state: "ready".to_string(),
            display: format_short_path_display(&default_vault),
        };
    }
    VaultStatus {
        state: "not_created".to_string(),
        display: format_short_path_display(&default_vault),
    }
}

/// Create or adopt DropVault folder
#[tauri::command]
pub fn create_vault(app: AppHandle) -> Result<VaultStatus, String> {
    let target = if let Some(p) = get_configured_vault_path(&app) {
        p
    } else {
        get_default_vault_path()
    };

    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| format!("Failed to create folder: {}", e))?;
    }

    save_configured_vault_path(&app, &target)?;
    let _ = app.emit("vault-changed", ());
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

    let mut items = Vec::new();

    for c in candidates.into_iter() {
        let canon = c.path.canonicalize().unwrap_or_else(|_| c.path.clone());
        let id = format!("dv_{:016x}", {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            canon.to_string_lossy().hash(&mut hasher);
            hasher.finish()
        });
        map.insert(id.clone(), canon.clone());

        let canon_str = canon.to_string_lossy().to_string();
        let clean_path = canon_str.strip_prefix(r"\\?\").unwrap_or(&canon_str).to_string();

        items.push(VaultFileItem {
            id,
            name: c.name,
            ext: c.ext,
            size: c.size,
            color_index: c.color_index,
            path: clean_path,
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

    let canon_str = canon.to_string_lossy().to_string();
    let clean_path = canon_str.strip_prefix(r"\\?\").unwrap_or(&canon_str);

    if EXECUTABLE_EXTS.contains(&ext.as_str()) {
        // Show in Explorer for safety
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", clean_path))
            .spawn();
        return Ok("shown".to_string());
    }

    // Open with default application via opener plugin, with shell fallback on Windows
    let res = tauri_plugin_opener::open_path(clean_path, None::<&str>);
    if let Err(e) = res {
        #[cfg(windows)]
        {
            let cmd_res = std::process::Command::new("cmd")
                .args(["/C", "start", "", clean_path])
                .spawn();
            if cmd_res.is_err() {
                return Err(format!("Failed to open file: {}", e));
            }
        }
        #[cfg(not(windows))]
        return Err(format!("Failed to open file: {}", e));
    }
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

    let canon_str = canon.to_string_lossy().to_string();
    let clean_path = canon_str.strip_prefix(r"\\?\").unwrap_or(&canon_str);

    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", clean_path))
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

    if VAULT_OPERATION_LOCK.load(Ordering::SeqCst) {
        return VaultSaveResult {
            count: total_count,
            saved: 0,
            rejected: vec!["busy".to_string()],
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

        // Prevent self-dropping / duplication of files already inside DropVault
        if is_path_in_vault(p, &vault_path) {
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

/// Returns stats for the DropVault folder on a background thread
#[tauri::command]
pub async fn vault_stats(app: AppHandle) -> Result<VaultStats, String> {
    tokio::task::spawn_blocking(move || {
        let vault_path = match get_configured_vault_path(&app) {
            Some(p) if p.is_dir() => p,
            _ => {
                let def = get_default_vault_path();
                if def.is_dir() {
                    def
                } else {
                    return Ok(VaultStats {
                        count: 0,
                        bytes: 0,
                        truncated: false,
                    });
                }
            }
        };

        let read_dir = match std::fs::read_dir(&vault_path) {
            Ok(rd) => rd,
            Err(_) => return Ok(VaultStats { count: 0, bytes: 0, truncated: false }),
        };

        let mut count = 0;
        let mut bytes = 0u64;
        let mut truncated = false;
        let mut total_scanned = 0;

        for entry in read_dir.flatten() {
            total_scanned += 1;
            if total_scanned > 5000 {
                truncated = true;
                break;
            }

            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            if !ft.is_file() || ft.is_symlink() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == INDEX_FILENAME || name.ends_with(".tmp") || name.ends_with(".partial") {
                continue;
            }

            let meta = match std::fs::symlink_metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let attrs = meta.file_attributes();
            if (attrs & 0x00000002) != 0 || (attrs & 0x00000004) != 0 || (attrs & 0x00000400) != 0 {
                continue;
            }

            count += 1;
            bytes = bytes.saturating_add(meta.len());
        }

        Ok(VaultStats {
            count,
            bytes,
            truncated,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn is_network_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\") || s.starts_with("//") {
        return true;
    }
    if let Some(root) = get_volume_root(path) {
        let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        wide.push(0);
        unsafe {
            if GetDriveTypeW(wide.as_ptr()) == 4 { // DRIVE_REMOTE = 4
                return true;
            }
        }
    }
    false
}

fn is_reparse_point(path: &Path) -> bool {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || (meta.file_attributes() & 0x00000400) != 0 {
            return true;
        }
    }
    false
}

fn has_reparse_ancestor(path: &Path) -> bool {
    let mut curr = Some(path.to_path_buf());
    while let Some(p) = curr {
        if is_reparse_point(&p) {
            return true;
        }
        curr = p.parent().map(|p| p.to_path_buf());
    }
    false
}

fn is_forbidden_system_folder(path: &Path) -> bool {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let canon_lower = canon.to_string_lossy().to_lowercase();

    let mut forbidden_dirs = Vec::new();

    if let Ok(windir) = std::env::var("windir") {
        forbidden_dirs.push(windir);
    }
    if let Ok(systemroot) = std::env::var("SystemRoot") {
        forbidden_dirs.push(systemroot);
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        forbidden_dirs.push(pf);
    }
    if let Ok(pfx86) = std::env::var("ProgramFiles(x86)") {
        forbidden_dirs.push(pfx86);
    }
    if let Ok(pd) = std::env::var("ProgramData") {
        forbidden_dirs.push(pd);
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        forbidden_dirs.push(appdata);
    }
    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        forbidden_dirs.push(localappdata);
    }
    if let Ok(userprof) = std::env::var("USERPROFILE") {
        forbidden_dirs.push(format!("{}\\AppData", userprof));
    }
    forbidden_dirs.push(r"C:\ProgramData\Dude".to_string());

    for f in forbidden_dirs {
        let f_path = PathBuf::from(&f);
        let f_canon = f_path.canonicalize().unwrap_or(f_path);
        let f_lower = f_canon.to_string_lossy().to_lowercase();

        if canon_lower == f_lower || canon_lower.starts_with(&(f_lower.clone() + "\\")) {
            return true;
        }
    }

    false
}

fn test_dir_writable(dir: &Path) -> bool {
    let test_file = dir.join(format!(
        "_dude_test_{}_{}.tmp",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
    ));
    if std::fs::write(&test_file, b"test").is_ok() {
        let _ = std::fs::remove_file(&test_file);
        true
    } else {
        false
    }
}

fn get_volume_root(path: &Path) -> Option<PathBuf> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canon.to_string_lossy();
    let trimmed = s.trim_start_matches(r"\\?\");
    if trimmed.len() >= 3 && trimmed.as_bytes()[1] == b':' && (trimmed.as_bytes()[2] == b'\\' || trimmed.as_bytes()[2] == b'/') {
        Some(PathBuf::from(format!("{}\\", &trimmed[..2])))
    } else {
        None
    }
}

fn is_same_volume(a: &Path, b: &Path) -> bool {
    match (get_volume_root(a), get_volume_root(b)) {
        (Some(ra), Some(rb)) => ra.to_string_lossy().eq_ignore_ascii_case(&rb.to_string_lossy()),
        _ => false,
    }
}

/// Opens native folder dialog off UI thread and validates proposed DropVault location
#[tauri::command]
pub async fn vault_pick_location(app: AppHandle) -> Result<VaultPickResult, String> {
    tokio::task::spawn_blocking(move || {
        let picked = rfd::FileDialog::new()
            .set_title("Choose DropVault Location")
            .pick_folder();

        let parent_path = match picked {
            Some(p) => p,
            None => return Err("cancelled".to_string()),
        };

        if !parent_path.is_dir() {
            return Err("system".to_string());
        }

        if is_network_path(&parent_path) {
            return Err("network".to_string());
        }

        if has_reparse_ancestor(&parent_path) {
            return Err("reparse".to_string());
        }

        let target_vault = parent_path.join("DropVault");
        if target_vault.exists() && is_reparse_point(&target_vault) {
            return Err("reparse".to_string());
        }

        if is_forbidden_system_folder(&parent_path) || is_forbidden_system_folder(&target_vault) {
            return Err("system".to_string());
        }

        // Compare against current vault
        let current_vault_opt = get_configured_vault_path(&app).or_else(|| {
            let def = get_default_vault_path();
            if def.is_dir() { Some(def) } else { None }
        });

        if let Some(curr) = current_vault_opt {
            let canon_curr = curr.canonicalize().unwrap_or(curr);
            let canon_target = target_vault.canonicalize().unwrap_or_else(|_| target_vault.clone());
            let canon_parent = parent_path.canonicalize().unwrap_or_else(|_| parent_path.clone());

            let curr_lower = canon_curr.to_string_lossy().to_lowercase();
            let target_lower = canon_target.to_string_lossy().to_lowercase();
            let parent_lower = canon_parent.to_string_lossy().to_lowercase();

            if target_lower == curr_lower {
                return Err("same".to_string());
            }
            if target_lower.starts_with(&(curr_lower.clone() + "\\"))
                || curr_lower.starts_with(&(target_lower.clone() + "\\"))
                || parent_lower.starts_with(&(curr_lower.clone() + "\\"))
                || parent_lower == curr_lower
            {
                return Err("inside".to_string());
            }
        }

        if !test_dir_writable(&parent_path) {
            return Err("readonly".to_string());
        }

        let adopt = if target_vault.exists() {
            if !target_vault.is_dir() {
                return Err("system".to_string());
            }
            true
        } else {
            false
        };

        let mut move_count = 0;
        let mut move_bytes = 0u64;
        if let Some(curr) = get_configured_vault_path(&app) {
            if curr.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&curr) {
                    for entry in rd.flatten() {
                        if let Ok(ft) = entry.file_type() {
                            if ft.is_file() && !ft.is_symlink() {
                                let name = entry.file_name().to_string_lossy().to_string();
                                if name.starts_with('.') || name == INDEX_FILENAME || name.ends_with(".tmp") || name.ends_with(".partial") {
                                    continue;
                                }
                                if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
                                    let attrs = meta.file_attributes();
                                    if (attrs & 0x00000002) != 0 || (attrs & 0x00000004) != 0 || (attrs & 0x00000400) != 0 {
                                        continue;
                                    }
                                    move_count += 1;
                                    move_bytes = move_bytes.saturating_add(meta.len());
                                }
                            }
                        }
                    }
                }
            }
        }

        let token = format!(
            "pv_{}_{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            std::process::id()
        );
        let parent_display = format_short_path_display(&parent_path);
        let vault_display = format_short_path_display(&target_vault);

        {
            let mut map = pending_locations().lock().unwrap();
            map.retain(|_, v| v.created_at.elapsed() <= std::time::Duration::from_secs(120));
            map.insert(token.clone(), PendingLocation {
                token: token.clone(),
                parent_path,
                target_vault,
                adopt,
                move_count,
                move_bytes,
                created_at: Instant::now(),
            });
        }

        Ok(VaultPickResult {
            token,
            parent_display,
            vault_display,
            adopt,
            move_count,
            move_bytes,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Applies chosen location, atomically saving store and optionally moving files on a background thread
#[tauri::command]
pub async fn vault_apply_location(
    app: AppHandle,
    token: String,
    mode: String,
) -> Result<VaultApplyResult, String> {
    let pending = {
        let mut map = pending_locations().lock().unwrap();
        map.remove(&token).ok_or_else(|| "token_expired".to_string())?
    };

    if pending.created_at.elapsed() > std::time::Duration::from_secs(120) {
        return Err("token_expired".to_string());
    }

    if VAULT_OPERATION_LOCK.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("busy".to_string());
    }

    struct LockGuard;
    impl Drop for LockGuard {
        fn drop(&mut self) {
            VAULT_OPERATION_LOCK.store(false, Ordering::SeqCst);
        }
    }
    let _guard = LockGuard;

    let old_vault = get_configured_vault_path(&app).unwrap_or_else(get_default_vault_path);
    let new_vault = pending.target_vault;

    if !new_vault.exists() {
        std::fs::create_dir_all(&new_vault).map_err(|e| format!("Failed to create folder: {}", e))?;
    }

    save_configured_vault_path(&app, &new_vault)?;

    {
        let mut last = LAST_OLD_VAULT_PATH.lock().unwrap();
        *last = Some(old_vault.clone());
    }

    if mode == "leave" {
        let _ = app.emit("vault-changed", ());
        return Ok(VaultApplyResult {
            moved: 0,
            failed: 0,
            cancelled: false,
        });
    }

    let app_handle = app.clone();
    tokio::task::spawn_blocking(move || {
        CANCEL_MOVE_REQUESTED.store(false, Ordering::SeqCst);

        let mut files_to_move = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&old_vault) {
            for entry in rd.flatten() {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_file() && !ft.is_symlink() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".tmp") || name.ends_with(".partial") {
                            continue;
                        }
                        if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
                            let attrs = meta.file_attributes();
                            if (attrs & 0x00000002) != 0 || (attrs & 0x00000004) != 0 || (attrs & 0x00000400) != 0 {
                                continue;
                            }
                            files_to_move.push((entry.path(), name));
                        }
                    }
                }
            }
        }

        let total = files_to_move.len();
        let mut moved = 0;
        let mut failed = 0;
        let mut cancelled = false;

        let same_volume = is_same_volume(&old_vault, &new_vault);

        for (i, (src_path, filename)) in files_to_move.into_iter().enumerate() {
            if CANCEL_MOVE_REQUESTED.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }

            let dest_path = resolve_collision_path(&new_vault, &filename);

            if same_volume {
                match std::fs::rename(&src_path, &dest_path) {
                    Ok(_) => moved += 1,
                    Err(_) => failed += 1,
                }
            } else {
                let partial_name = format!("{}.partial", dest_path.file_name().unwrap().to_string_lossy());
                let partial_path = new_vault.join(partial_name);
                let src_len = match std::fs::metadata(&src_path) {
                    Ok(m) => m.len(),
                    Err(_) => {
                        failed += 1;
                        continue;
                    }
                };

                let copy_res = std::fs::copy(&src_path, &partial_path);
                if let Ok(copied_bytes) = copy_res {
                    if copied_bytes == src_len {
                        if std::fs::rename(&partial_path, &dest_path).is_ok() {
                            let _ = std::fs::remove_file(&src_path);
                            moved += 1;
                        } else {
                            let _ = std::fs::remove_file(&partial_path);
                            failed += 1;
                        }
                    } else {
                        let _ = std::fs::remove_file(&partial_path);
                        failed += 1;
                    }
                } else {
                    let _ = std::fs::remove_file(&partial_path);
                    failed += 1;
                }
            }

            let _ = app_handle.emit("vault-move-progress", VaultMoveProgress {
                current: i + 1,
                total,
            });
        }

        let old_index_path = old_vault.join(INDEX_FILENAME);
        if old_index_path.exists() {
            let old_idx = read_index(&old_vault);
            let mut new_idx = read_index(&new_vault);
            for (k, v) in old_idx {
                new_idx.entry(k).or_insert(v);
            }
            write_index(&new_vault, &new_idx);
            let _ = std::fs::remove_file(&old_index_path);
        }

        let _ = app_handle.emit("vault-changed", ());

        Ok(VaultApplyResult {
            moved,
            failed,
            cancelled,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Cancels pending location token
#[tauri::command]
pub fn vault_cancel_location(token: String) -> Result<(), String> {
    let mut map = pending_locations().lock().unwrap();
    map.remove(&token);
    Ok(())
}

/// Signals cancellation of in-progress vault move
#[tauri::command]
pub fn vault_cancel_move() -> Result<(), String> {
    CANCEL_MOVE_REQUESTED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Opens previous vault folder in Explorer
#[tauri::command]
pub fn vault_open_old_folder() -> Result<(), String> {
    let path_opt = {
        let last = LAST_OLD_VAULT_PATH.lock().unwrap();
        last.clone()
    };
    if let Some(p) = path_opt {
        if p.is_dir() {
            let _ = std::process::Command::new("explorer").arg(&p).spawn();
        }
    }
    Ok(())
}

/// Helper to get drive root (e.g. "C:\\" or "\\\\server\\share\\")
pub fn get_drive_root(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    if let Some(std::path::Component::Prefix(prefix)) = components.next() {
        let mut root = PathBuf::from(prefix.as_os_str());
        root.push("\\");
        Some(root)
    } else {
        None
    }
}

/// Checks whether the drive supporting the path supports the Windows Recycle Bin
pub fn check_recycle_bin_supported(drive_root: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = drive_root
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut info = SHQUERYRBINFO {
            cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
            i64Size: 0,
            i64NumItems: 0,
        };
        let res = unsafe { SHQueryRecycleBinW(wide.as_ptr(), &mut info) };
        res == 0
    }
    #[cfg(not(windows))]
    {
        let _ = drive_root;
        false
    }
}

/// Moves a file to the Windows Recycle Bin using SHFileOperationW with FOF_ALLOWUNDO
pub fn move_to_recycle_bin(path: &Path) -> Result<(), u32> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        wide.push(0); // double-null terminated

        let mut op = SHFILEOPSTRUCTW {
            hwnd: std::ptr::null_mut(),
            wFunc: FO_DELETE,
            pFrom: wide.as_ptr(),
            pTo: std::ptr::null(),
            fFlags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT,
            fAnyOperationsAborted: 0,
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: std::ptr::null(),
        };

        let mut ret = unsafe { SHFileOperationW(&mut op) };
        if ret != 0 {
            // Read-only files: Windows Recycle Bin accepts them; remove read-only attribute first and retry
            unsafe {
                SetFileAttributesW(wide.as_ptr(), 0x0080); // FILE_ATTRIBUTE_NORMAL
                ret = SHFileOperationW(&mut op);
            }
        }
        if ret == 0 {
            if op.fAnyOperationsAborted != 0 {
                Err(1)
            } else {
                Ok(())
            }
        } else {
            Err(ret as u32)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(1)
    }
}

/// Scans the DropVault top level for old files and returns a preview with an opaque token
#[tauri::command]
pub async fn vault_clean_preview(app: AppHandle, days: u32) -> Result<CleanPreviewResult, String> {
    let status = vault_status(app.clone());
    if status.state != "ready" {
        return Err("DropVault is not ready.".to_string());
    }

    let vault_path = get_configured_vault_path(&app).unwrap_or_else(get_default_vault_path);
    if !vault_path.is_dir() {
        return Err("DropVault folder not found.".to_string());
    }

    // Check Recycle Bin support on vault drive
    if let Some(drive_root) = get_drive_root(&vault_path) {
        if !check_recycle_bin_supported(&drive_root) {
            return Err("This drive has no Recycle Bin, so Dude can't clean it safely. Tidy it in Explorer.".to_string());
        }
    } else {
        return Err("This drive has no Recycle Bin, so Dude can't clean it safely. Tidy it in Explorer.".to_string());
    }

    // Scan on background thread
    let result = tokio::task::spawn_blocking(move || -> Result<CleanPreviewResult, String> {
        let mut total_entries = 0;
        let mut skipped_folders = 0;
        let mut candidates = Vec::new();
        let now_sys = SystemTime::now();
        let now_epoch = now_sys.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let threshold_secs = (days as u64) * 86400;
        let index = read_index(&vault_path);

        let entries = std::fs::read_dir(&vault_path)
            .map_err(|e| format!("Failed to read folder: {}", e))?;

        for entry_res in entries {
            let entry = entry_res.map_err(|e| format!("Failed to read entry: {}", e))?;
            total_entries += 1;
            if total_entries > 5000 {
                return Err("Too many files to clean safely here. Open the folder and tidy it in Explorer.".to_string());
            }

            let file_type = entry.file_type().map_err(|e| format!("Failed to get file type: {}", e))?;
            if file_type.is_dir() {
                skipped_folders += 1;
                continue;
            }

            let path = entry.path();
            // Check symlink / reparse point
            if let Ok(sym_meta) = path.symlink_metadata() {
                if sym_meta.file_type().is_symlink() {
                    continue;
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    let attrs = sym_meta.file_attributes();
                    // FILE_ATTRIBUTE_REPARSE_POINT = 0x0400
                    if (attrs & 0x0400) != 0 {
                        continue;
                    }
                    // Hidden or system file: FILE_ATTRIBUTE_HIDDEN = 0x0002, FILE_ATTRIBUTE_SYSTEM = 0x0004
                    if (attrs & (0x0002 | 0x0004)) != 0 {
                        continue;
                    }
                }
            } else {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with('.')
                || file_name.ends_with(".partial")
                || file_name.ends_with(".tmp")
                || file_name == ".dropvault-index.json"
            {
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let modified_time = meta.modified().unwrap_or(now_sys);
            let modified_epoch = modified_time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

            let created_epoch = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
            let index_epoch = index.get(&file_name).copied();

            let added_epoch = match (index_epoch, created_epoch) {
                (Some(idx), Some(cr)) => idx.max(cr),
                (Some(idx), None) => idx,
                (None, Some(cr)) => cr,
                (None, None) => modified_epoch,
            };

            let last_activity_sec = added_epoch.max(modified_epoch);
            let age_secs = now_epoch.saturating_sub(last_activity_sec);

            if age_secs > threshold_secs {
                candidates.push(CleanCandidate {
                    path,
                    filename: file_name,
                    size: meta.len(),
                    modified: modified_time,
                    last_activity_sec,
                });
            }
        }

        let count = candidates.len();
        let bytes: u64 = candidates.iter().map(|c| c.size).sum();
        let oldest_age_days = candidates
            .iter()
            .map(|c| (now_epoch.saturating_sub(c.last_activity_sec)) / 86400)
            .max()
            .unwrap_or(0);

        let token = format!(
            "cln_{}_{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            std::process::id()
        );
        {
            let mut previews = clean_previews().lock().unwrap();
            previews.clear(); // Replace any previous review
            previews.insert(
                token.clone(),
                CleanPreviewSession {
                    created_at: Instant::now(),
                    days_threshold: days,
                    candidates,
                },
            );
        }

        Ok(CleanPreviewResult {
            token,
            count,
            bytes,
            oldest_age_days,
            skipped_folders,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    Ok(result)
}

/// Moves old files from the review session to the Windows Recycle Bin
#[tauri::command]
pub async fn vault_clean_apply(app: AppHandle, token: String) -> Result<CleanApplyResult, String> {
    if VAULT_OPERATION_LOCK.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("Vault operation already in progress".to_string());
    }

    struct LockGuard;
    impl Drop for LockGuard {
        fn drop(&mut self) {
            VAULT_OPERATION_LOCK.store(false, Ordering::SeqCst);
        }
    }
    let _guard = LockGuard;

    CANCEL_CLEAN_REQUESTED.store(false, Ordering::SeqCst);

    let session = {
        let mut previews = clean_previews().lock().unwrap();
        previews.remove(&token)
    };

    let session = match session {
        Some(s) if s.created_at.elapsed() <= Duration::from_secs(120) => s,
        _ => return Err("Review expired or invalid".to_string()),
    };

    let vault_path = get_configured_vault_path(&app).unwrap_or_else(get_default_vault_path);
    let canonical_vault = match vault_path.canonicalize() {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to canonicalize vault path: {}", e)),
    };

    let app_clone = app.clone();
    let result = tokio::task::spawn_blocking(move || -> CleanApplyResult {
        let total = session.candidates.len();
        let mut moved_count = 0;
        let mut moved_bytes = 0u64;
        let mut skipped_count = 0;
        let mut cancelled = false;

        let now_sys = SystemTime::now();
        let now_epoch = now_sys.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let threshold_secs = (session.days_threshold as u64) * 86400;

        let mut index = read_index(&vault_path);
        let mut index_changed = false;

        for (i, cand) in session.candidates.into_iter().enumerate() {
            if CANCEL_CLEAN_REQUESTED.load(Ordering::SeqCst) {
                cancelled = true;
                skipped_count += total - i;
                break;
            }

            let _ = app_clone.emit("vault-clean-progress", serde_json::json!({
                "current": i + 1,
                "total": total
            }));

            // 1. Re-verify path exists
            if !cand.path.exists() {
                skipped_count += 1;
                continue;
            }

            // 2. Canonical path prefix check
            let canonical_file = match cand.path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    skipped_count += 1;
                    continue;
                }
            };
            if !canonical_file.starts_with(&canonical_vault) {
                skipped_count += 1;
                continue;
            }

            // 3. Not a reparse point / symlink
            if let Ok(sm) = cand.path.symlink_metadata() {
                if sm.file_type().is_symlink() {
                    skipped_count += 1;
                    continue;
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if (sm.file_attributes() & 0x0400) != 0 {
                        skipped_count += 1;
                        continue;
                    }
                }
            } else {
                skipped_count += 1;
                continue;
            }

            // 4. Same size and last-write time as at preview
            let meta = match std::fs::metadata(&cand.path) {
                Ok(m) => m,
                Err(_) => {
                    skipped_count += 1;
                    continue;
                }
            };

            if meta.len() != cand.size {
                skipped_count += 1;
                continue;
            }

            let current_modified = meta.modified().unwrap_or(now_sys);
            if current_modified.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
                != cand.modified.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
            {
                skipped_count += 1;
                continue;
            }

            // 5. Still older than threshold
            let modified_epoch = current_modified.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let created_epoch = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
            let index_epoch = index.get(&cand.filename).copied();
            let added_epoch = match (index_epoch, created_epoch) {
                (Some(idx), Some(cr)) => idx.max(cr),
                (Some(idx), None) => idx,
                (None, Some(cr)) => cr,
                (None, None) => modified_epoch,
            };
            let last_activity = added_epoch.max(modified_epoch);
            if now_epoch.saturating_sub(last_activity) <= threshold_secs {
                skipped_count += 1;
                continue;
            }

            // Move to Recycle Bin
            match move_to_recycle_bin(&cand.path) {
                Ok(()) => {
                    moved_count += 1;
                    moved_bytes += cand.size;
                    if index.remove(&cand.filename).is_some() {
                        index_changed = true;
                    }
                }
                Err(_) => {
                    skipped_count += 1;
                }
            }
        }

        if index_changed {
            write_index(&vault_path, &index);
        }

        CleanApplyResult {
            moved: moved_count,
            moved_bytes,
            skipped: skipped_count,
            cancelled,
        }
    })
    .await
    .unwrap_or(CleanApplyResult {
        moved: 0,
        moved_bytes: 0,
        skipped: 0,
        cancelled: false,
    });

    let _ = app.emit("vault-changed", ());

    Ok(result)
}

/// Cancels in-progress clean apply operation
#[tauri::command]
pub fn vault_clean_cancel() -> Result<(), String> {
    CANCEL_CLEAN_REQUESTED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Discards open clean preview session and invalidates token
#[tauri::command]
pub fn vault_clean_discard(token: String) -> Result<(), String> {
    let mut previews = clean_previews().lock().unwrap();
    previews.remove(&token);
    Ok(())
}

/// Debug-only command to seed 30 dummy test files with spread timestamps in %TEMP%\DudeVaultTest\DropVault
#[tauri::command]
pub fn seed_vault_test_files(app: AppHandle) -> Result<usize, String> {
    #[cfg(debug_assertions)]
    {
        let target = get_default_vault_path();
        std::fs::create_dir_all(&target).map_err(|e| format!("Failed to create test dir: {}", e))?;
        let _ = save_configured_vault_path(&app, &target);

        let now = SystemTime::now();
        let mut index = read_index(&target);

        // 1. 30 base files aged 5 to 400 days
        for i in 1..=30 {
            let filename = format!("sample-{:02}.txt", i);
            let file_path = target.join(&filename);
            let content = format!("Dude test vault file #{:02}\nGenerated for debug testing.\n", i);
            let _ = std::fs::write(&file_path, content);

            let days_ago = 5 + ((i - 1) * 395) / 29;
            let past_time = now - std::time::Duration::from_secs(days_ago as u64 * 86400);
            let past_sec = past_time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

            let intervals = (past_sec + 11644473600) * 10_000_000;
            let ft = FILETIME {
                dw_low: (intervals & 0xFFFFFFFF) as u32,
                dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
            };

            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&file_path) {
                unsafe {
                    SetFileTime(
                        f.as_raw_handle() as *mut std::ffi::c_void,
                        &ft,
                        &ft,
                        &ft,
                    );
                }
            }
            index.insert(filename, past_sec);
        }

        // 2. Case 1: sample-recent-mod.txt: added 400 days ago, modified yesterday (1 day ago)
        {
            let filename = "sample-recent-mod.txt".to_string();
            let file_path = target.join(&filename);
            let content = "Added 400 days ago, modified yesterday. Never old.\n";
            let _ = std::fs::write(&file_path, content);

            let created_past = now - std::time::Duration::from_secs(400 * 86400);
            let created_sec = created_past.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let created_ft = {
                let intervals = (created_sec + 11644473600) * 10_000_000;
                FILETIME {
                    dw_low: (intervals & 0xFFFFFFFF) as u32,
                    dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
                }
            };

            let mod_past = now - std::time::Duration::from_secs(1 * 86400);
            let mod_sec = mod_past.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let mod_ft = {
                let intervals = (mod_sec + 11644473600) * 10_000_000;
                FILETIME {
                    dw_low: (intervals & 0xFFFFFFFF) as u32,
                    dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
                }
            };

            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&file_path) {
                unsafe {
                    SetFileTime(
                        f.as_raw_handle() as *mut std::ffi::c_void,
                        &created_ft,
                        &mod_ft,
                        &mod_ft,
                    );
                }
            }
            index.insert(filename, created_sec);
        }

        // 3. Case 2: Subfolder Archive with old file nested-old.txt (never touched)
        {
            let archive_dir = target.join("Archive");
            let _ = std::fs::create_dir_all(&archive_dir);
            let sub_file = archive_dir.join("nested-old.txt");
            let _ = std::fs::write(&sub_file, "Old nested file in subfolder. Never touched.\n");

            let past_time = now - std::time::Duration::from_secs(300 * 86400);
            let past_sec = past_time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let intervals = (past_sec + 11644473600) * 10_000_000;
            let ft = FILETIME {
                dw_low: (intervals & 0xFFFFFFFF) as u32,
                dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
            };
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&sub_file) {
                unsafe {
                    SetFileTime(
                        f.as_raw_handle() as *mut std::ffi::c_void,
                        &ft,
                        &ft,
                        &ft,
                    );
                }
            }
        }

        // 4. Case 3: Read-only old file (200 days old)
        {
            let filename = "sample-readonly-old.txt".to_string();
            let file_path = target.join(&filename);
            let content = "Read-only file aged 200 days.\n";
            let _ = std::fs::write(&file_path, content);

            let past_time = now - std::time::Duration::from_secs(200 * 86400);
            let past_sec = past_time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let intervals = (past_sec + 11644473600) * 10_000_000;
            let ft = FILETIME {
                dw_low: (intervals & 0xFFFFFFFF) as u32,
                dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
            };
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&file_path) {
                unsafe {
                    SetFileTime(
                        f.as_raw_handle() as *mut std::ffi::c_void,
                        &ft,
                        &ft,
                        &ft,
                    );
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                let wide: Vec<u16> = file_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                unsafe {
                    SetFileAttributesW(wide.as_ptr(), 0x0001); // FILE_ATTRIBUTE_READONLY
                }
            }
            index.insert(filename, past_sec);
        }

        write_index(&target, &index);

        // Terminal print of threshold counts for debug verification
        println!("[DUDE_VAULT_TEST] Seeded test files. Threshold counts:");
        println!("  30 days:  29 old files");
        println!("  60 days:  26 old files");
        println!("  90 days:  24 old files");
        println!("  180 days: 18 old files");
        println!("  365 days:  3 old files");
        println!("  Skipped folders: 1 (Archive)");
        println!("  sample-recent-mod.txt: NOT old (modified 1 day ago)");
        println!("  sample-readonly-old.txt: 200 days old (read-only)");

        let _ = app.emit("vault-changed", ());
        Ok(33)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = app;
        Err("Disabled in release build".to_string())
    }
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
    fn test_is_path_in_vault_and_self_drop_prevention() {
        let temp_dir = std::env::temp_dir().join("dude_dropvault_test_selfdrop");
        let _ = std::fs::create_dir_all(&temp_dir);

        let vault_file = temp_dir.join("existing_file.png");
        let _ = std::fs::File::create(&vault_file);

        let outside_dir = std::env::temp_dir().join("dude_dropvault_test_outside");
        let _ = std::fs::create_dir_all(&outside_dir);
        let outside_file = outside_dir.join("outside.png");
        let _ = std::fs::File::create(&outside_file);

        // Path inside vault
        assert!(is_path_in_vault(&vault_file, &temp_dir));

        // Subdirectory file inside vault
        let sub_dir = temp_dir.join("subfolder");
        let _ = std::fs::create_dir_all(&sub_dir);
        let sub_file = sub_dir.join("nested.png");
        let _ = std::fs::File::create(&sub_file);
        assert!(is_path_in_vault(&sub_file, &temp_dir));

        // Outside file
        assert!(!is_path_in_vault(&outside_file, &temp_dir));

        // Sibling folder with prefix match (should NOT match)
        let sibling_dir = std::env::temp_dir().join("dude_dropvault_test_selfdrop_other");
        let _ = std::fs::create_dir_all(&sibling_dir);
        let sibling_file = sibling_dir.join("file.png");
        let _ = std::fs::File::create(&sibling_file);
        assert!(!is_path_in_vault(&sibling_file, &temp_dir));

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_dir_all(&outside_dir);
        let _ = std::fs::remove_dir_all(&sibling_dir);
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

    #[test]
    fn test_format_short_path_display() {
        let docs = get_documents_path();
        let target1 = docs.join("DropVault");
        let disp1 = format_short_path_display(&target1);
        assert_eq!(disp1, "Documents\\DropVault");

        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let prof = PathBuf::from(userprofile);
            let target2 = prof.join("CustomVault").join("DropVault");
            let disp2 = format_short_path_display(&target2);
            assert_eq!(disp2, "~\\CustomVault\\DropVault");
        }

        let ext = PathBuf::from("D:\\ExternalData\\DropVault");
        let disp3 = format_short_path_display(&ext);
        assert_eq!(disp3, "D:\\ExternalData\\DropVault");
    }

    #[test]
    fn test_is_forbidden_system_folder() {
        assert!(is_forbidden_system_folder(&PathBuf::from("C:\\Windows")));
        assert!(is_forbidden_system_folder(&PathBuf::from("C:\\Windows\\System32")));
        assert!(is_forbidden_system_folder(&PathBuf::from("C:\\Program Files")));
        assert!(is_forbidden_system_folder(&PathBuf::from("C:\\ProgramData\\Dude")));
        assert!(!is_forbidden_system_folder(&PathBuf::from("D:\\MyProjects")));
    }

    #[test]
    fn test_vault_operation_locking() {
        VAULT_OPERATION_LOCK.store(true, Ordering::SeqCst);
        assert!(VAULT_OPERATION_LOCK.load(Ordering::SeqCst));
        VAULT_OPERATION_LOCK.store(false, Ordering::SeqCst);
        assert!(!VAULT_OPERATION_LOCK.load(Ordering::SeqCst));
    }

    #[test]
    fn test_recycle_bin_supported_check() {
        #[cfg(windows)]
        {
            let c_drive = PathBuf::from("C:\\");
            if c_drive.is_dir() {
                assert!(check_recycle_bin_supported(&c_drive), "C:\\ must support Recycle Bin");
            }
            let bad_drive = PathBuf::from("Z:\\NonExistentDrive_DudeTest\\");
            assert!(!check_recycle_bin_supported(&bad_drive), "Invalid drive should return false");
        }
    }

    #[test]
    fn test_definition_of_old_math() {
        let now_epoch = 1_700_000_000u64;

        // Case 1: Created 400 days ago, modified 1 day ago
        let added_epoch = now_epoch - (400 * 86400);
        let modified_epoch = now_epoch - (1 * 86400);
        let last_activity = added_epoch.max(modified_epoch);
        let age_secs = now_epoch.saturating_sub(last_activity);
        assert_eq!(age_secs, 1 * 86400);
        // Not old for any threshold >= 30 days
        for days in [30, 60, 90, 180, 365] {
            assert!(age_secs <= days * 86400);
        }

        // Case 2: Seed formula check: days_ago = 5 + ((i - 1) * 395) / 29 for i in 1..=30
        let mut base_ages = Vec::new();
        for i in 1..=30 {
            let days_ago = 5 + ((i - 1) * 395) / 29;
            base_ages.push(days_ago);
        }

        // Check threshold counts on base files (plus 1 readonly at 200 days)
        // Read-only is at 200 days: old for 30, 60, 90, 180; not old for 365
        for (threshold_days, expected_count) in [(30, 29), (60, 26), (90, 24), (180, 18), (365, 3)] {
            let mut count = base_ages.iter().filter(|&&d| d > threshold_days).count();
            if 200 > threshold_days {
                count += 1; // sample-readonly-old.txt
            }
            assert_eq!(count, expected_count, "Threshold {} days should yield {} files", threshold_days, expected_count);
        }
    }

    #[test]
    fn test_clean_scan_and_threshold_verification() {
        let temp_dir = std::env::temp_dir().join("dude_dropvault_clean_test");
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::create_dir_all(&temp_dir);

        let now = SystemTime::now();
        let now_sec = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

        // Subfolder Archive with old file (should be skipped)
        let archive_dir = temp_dir.join("Archive");
        let _ = std::fs::create_dir_all(&archive_dir);
        let _ = std::fs::write(archive_dir.join("nested-old.txt"), "nested");

        // Partial & temp files (should be ignored)
        let _ = std::fs::write(temp_dir.join("download.partial"), "partial");
        let _ = std::fs::write(temp_dir.join("cache.tmp"), "tmp");
        let _ = std::fs::write(temp_dir.join(".dropvault-index.json"), "{}");

        // Create 30 base files with past modified and index times
        let mut index = HashMap::new();
        for i in 1..=30 {
            let filename = format!("sample-{:02}.txt", i);
            let file_path = temp_dir.join(&filename);
            let _ = std::fs::write(&file_path, format!("content {}", i));
            let days_ago = 5 + ((i - 1) * 395) / 29;
            let past_sec = now_sec - (days_ago as u64 * 86400);

            #[cfg(windows)]
            {
                let intervals = (past_sec + 11644473600) * 10_000_000;
                let ft = FILETIME {
                    dw_low: (intervals & 0xFFFFFFFF) as u32,
                    dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
                };
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&file_path) {
                    unsafe {
                        SetFileTime(
                            f.as_raw_handle() as *mut std::ffi::c_void,
                            &ft,
                            &ft,
                            &ft,
                        );
                    }
                }
            }

            index.insert(filename, past_sec);
        }

        // sample-recent-mod.txt: added 400 days ago, modified 1 day ago
        {
            let filename = "sample-recent-mod.txt".to_string();
            let file_path = temp_dir.join(&filename);
            let _ = std::fs::write(&file_path, "recent mod");
            let added_sec = now_sec - (400 * 86400);
            let mod_sec = now_sec - (1 * 86400);

            #[cfg(windows)]
            {
                let intervals = (mod_sec + 11644473600) * 10_000_000;
                let ft = FILETIME {
                    dw_low: (intervals & 0xFFFFFFFF) as u32,
                    dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
                };
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&file_path) {
                    unsafe {
                        SetFileTime(
                            f.as_raw_handle() as *mut std::ffi::c_void,
                            &ft,
                            &ft,
                            &ft,
                        );
                    }
                }
            }

            index.insert(filename, added_sec);
        }

        // sample-readonly-old.txt: 200 days old
        {
            let filename = "sample-readonly-old.txt".to_string();
            let file_path = temp_dir.join(&filename);
            let _ = std::fs::write(&file_path, "readonly");
            let past_sec = now_sec - (200 * 86400);

            #[cfg(windows)]
            {
                let intervals = (past_sec + 11644473600) * 10_000_000;
                let ft = FILETIME {
                    dw_low: (intervals & 0xFFFFFFFF) as u32,
                    dw_high: ((intervals >> 32) & 0xFFFFFFFF) as u32,
                };
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&file_path) {
                    unsafe {
                        SetFileTime(
                            f.as_raw_handle() as *mut std::ffi::c_void,
                            &ft,
                            &ft,
                            &ft,
                        );
                    }
                }
            }

            index.insert(filename, past_sec);
        }

        // Scan the directory using the definition of old
        let mut skipped_folders = 0;
        let mut file_ages: Vec<(String, u64)> = Vec::new();

        for entry in std::fs::read_dir(&temp_dir).unwrap().flatten() {
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                skipped_folders += 1;
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.ends_with(".partial") || name.ends_with(".tmp") || name == ".dropvault-index.json" {
                continue;
            }
            let meta = entry.metadata().unwrap();
            let modified_epoch = meta.modified().unwrap_or(now).duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            let index_epoch = index.get(&name).copied().unwrap_or(modified_epoch);
            let last_activity = index_epoch.max(modified_epoch);
            let age_days = (now_sec.saturating_sub(last_activity)) / 86400;
            file_ages.push((name, age_days));
        }

        assert_eq!(skipped_folders, 1, "Archive folder must be counted as 1 skipped folder");

        // Verify counts
        for (threshold_days, expected_count) in [(30, 29), (60, 26), (90, 24), (180, 18), (365, 3)] {
            let count = file_ages.iter().filter(|(_, d)| *d > threshold_days).count();
            assert_eq!(count, expected_count, "Expected {} candidates for {} days", expected_count, threshold_days);
        }

        // Verify sample-recent-mod.txt is not old (modified 1 day ago)
        let recent_mod_age = file_ages.iter().find(|(n, _)| n == "sample-recent-mod.txt").map(|(_, d)| *d).unwrap();
        assert!(recent_mod_age <= 1, "sample-recent-mod.txt modified 1 day ago should have age <= 1 day");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

