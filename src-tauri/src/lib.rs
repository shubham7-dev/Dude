use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Listener, Manager};
use keyring::Entry;
use tauri_plugin_store::StoreExt;

pub mod agent_hooks;
pub mod agent_receiver;
pub mod tray;
pub mod sound;
pub mod clipboard_history;
pub mod audio_visualizer;
pub mod dropvault;

const KEYRING_SERVICE: &str = "Dude";
const KEYRING_USER: &str = "GeminiAPIKey";
const STORE_FILENAME: &str = "dude_store.json";

static IS_QUITTING: AtomicBool = AtomicBool::new(false);

pub fn is_quitting() -> bool {
    IS_QUITTING.load(Ordering::SeqCst)
}

pub fn stop_background_workers() {
    IS_QUITTING.store(true, Ordering::SeqCst);
    BRIGHTNESS_CONDVAR.notify_all();
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn log_frontend_error(window: tauri::Window, message: String, stack: Option<String>) {
    #[cfg(debug_assertions)]
    {
        eprintln!("[FRONTEND ERROR - {}] {}", window.label(), message);
        if let Some(st) = stack {
            eprintln!("{}", st);
        }
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (window, message, stack);
    }
}

#[tauri::command]
fn log_perf_metric(message: String) {
    #[cfg(debug_assertions)]
    {
        println!("{}", message);
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = message;
    }
}

#[tauri::command]
fn save_asset_webp(base64_data: String) -> Result<(), String> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&base64_data)
        .map_err(|e| e.to_string())?;

    let asset_path = std::path::Path::new("assets").join("appearance-tree.webp");
    if !asset_path.exists() {
        if let Some(parent) = asset_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&asset_path, &decoded);
    }

    let dist_dir = std::path::Path::new("dist").join("assets");
    if dist_dir.exists() {
        let dist_path = dist_dir.join("appearance-tree.webp");
        if !dist_path.exists() {
            let _ = std::fs::write(&dist_path, &decoded);
        }
    }
    Ok(())
}

// --- Windows Core Audio (Volume & Mute) Cached ---
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use std::sync::atomic::AtomicU32;
use std::sync::{Condvar, Mutex, Once};

struct AudioEndpointWrapper(IAudioEndpointVolume);
unsafe impl Send for AudioEndpointWrapper {}
unsafe impl Sync for AudioEndpointWrapper {}

static CACHED_AUDIO_ENDPOINT: Mutex<Option<AudioEndpointWrapper>> = Mutex::new(None);

fn get_cached_audio_endpoint() -> Result<IAudioEndpointVolume, String> {
    let mut lock = CACHED_AUDIO_ENDPOINT.lock().unwrap();
    if let Some(wrapper) = &*lock {
        return Ok(wrapper.0.clone());
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| e.to_string())?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| e.to_string())?;
        let endpoint_volume: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| e.to_string())?;
        *lock = Some(AudioEndpointWrapper(endpoint_volume.clone()));
        Ok(endpoint_volume)
    }
}

fn invalidate_audio_endpoint() {
    let mut lock = CACHED_AUDIO_ENDPOINT.lock().unwrap();
    *lock = None;
}

fn get_volume_internal() -> Result<u32, String> {
    let endpoint = get_cached_audio_endpoint()?;
    unsafe {
        match endpoint.GetMasterVolumeLevelScalar() {
            Ok(level) => Ok((level * 100.0).round().clamp(0.0, 100.0) as u32),
            Err(_) => {
                invalidate_audio_endpoint();
                let ep2 = get_cached_audio_endpoint()?;
                let level2 = ep2.GetMasterVolumeLevelScalar().map_err(|e| e.to_string())?;
                Ok((level2 * 100.0).round().clamp(0.0, 100.0) as u32)
            }
        }
    }
}

fn set_volume_internal(volume: u32) -> Result<(), String> {
    let endpoint = get_cached_audio_endpoint()?;
    let level = (volume.clamp(0, 100) as f32) / 100.0;
    unsafe {
        if endpoint.SetMasterVolumeLevelScalar(level, std::ptr::null()).is_err() {
            invalidate_audio_endpoint();
            let ep2 = get_cached_audio_endpoint()?;
            ep2.SetMasterVolumeLevelScalar(level, std::ptr::null()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn get_mute_internal() -> Result<bool, String> {
    let endpoint = get_cached_audio_endpoint()?;
    unsafe {
        match endpoint.GetMute() {
            Ok(mute) => Ok(mute.as_bool()),
            Err(_) => {
                invalidate_audio_endpoint();
                let ep2 = get_cached_audio_endpoint()?;
                let mute2 = ep2.GetMute().map_err(|e| e.to_string())?;
                Ok(mute2.as_bool())
            }
        }
    }
}

fn set_mute_internal(mute: bool) -> Result<(), String> {
    let endpoint = get_cached_audio_endpoint()?;
    unsafe {
        if endpoint.SetMute(mute, std::ptr::null()).is_err() {
            invalidate_audio_endpoint();
            let ep2 = get_cached_audio_endpoint()?;
            ep2.SetMute(mute, std::ptr::null()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[tauri::command]
async fn get_volume() -> Result<u32, String> {
    tokio::task::spawn_blocking(get_volume_internal)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_volume(volume: u32) -> Result<(), String> {
    tokio::task::spawn_blocking(move || set_volume_internal(volume))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_mute() -> Result<bool, String> {
    tokio::task::spawn_blocking(get_mute_internal)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_mute(mute: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || set_mute_internal(mute))
        .await
        .map_err(|e| e.to_string())?
}

// --- WMI Brightness on Dedicated Worker Thread ---
static BRIGHTNESS_SUPPORTED: AtomicBool = AtomicBool::new(false);
static BRIGHTNESS_INITIALIZED: AtomicBool = AtomicBool::new(false);
static LATEST_TARGET_BRIGHTNESS: AtomicU32 = AtomicU32::new(u32::MAX);
static BRIGHTNESS_ERROR_EMITTED: AtomicBool = AtomicBool::new(false);
static LAST_KNOWN_BRIGHTNESS: AtomicU32 = AtomicU32::new(50);
static BRIGHTNESS_WORKER_ONCE: Once = Once::new();

struct BrightnessWorkerState {
    pending_set: bool,
    pending_gets: Vec<std::sync::mpsc::Sender<Result<u32, String>>>,
}

static BRIGHTNESS_STATE: Mutex<BrightnessWorkerState> = Mutex::new(BrightnessWorkerState {
    pending_set: false,
    pending_gets: Vec::new(),
});
static BRIGHTNESS_CONDVAR: Condvar = Condvar::new();
static BRIGHTNESS_APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

#[derive(serde::Deserialize, Debug)]
#[serde(rename = "WmiMonitorBrightness")]
#[serde(rename_all = "PascalCase")]
struct WmiMonitorBrightness {
    current_brightness: u8,
}

#[derive(serde::Deserialize, Debug)]
#[serde(rename = "WmiMonitorBrightnessMethods")]
#[serde(rename_all = "PascalCase")]
struct WmiMonitorBrightnessMethods {
    active: bool,
    instance_name: String,
}

struct WmiBrightnessContext {
    con: wmi::WMIConnection,
    instance_path: String,
}

impl WmiBrightnessContext {
    fn new() -> Result<Self, String> {
        let com_lib = wmi::COMLibrary::new().map_err(|e| format!("COMLibrary::new failed: {}", e))?;
        let con = wmi::WMIConnection::with_namespace_path("ROOT\\WMI", com_lib)
            .map_err(|e| format!("WMIConnection::with_namespace_path failed: {}", e))?;

        let methods: Vec<WmiMonitorBrightnessMethods> = con
            .query()
            .map_err(|e| format!("Failed to query WmiMonitorBrightnessMethods: {}", e))?;

        let active_method = methods
            .into_iter()
            .find(|m| m.active)
            .ok_or_else(|| "No active WmiMonitorBrightnessMethods found".to_string())?;

        let instance_path = format!(
            "WmiMonitorBrightnessMethods.InstanceName=\"{}\"",
            active_method.instance_name.replace('\\', "\\\\")
        );

        Ok(Self { con, instance_path })
    }

    fn set_brightness(&self, level: u32) -> Result<(), String> {
        let b = level.clamp(0, 100) as u8;
        let mut in_params = std::collections::HashMap::new();
        in_params.insert("Timeout".to_string(), wmi::Variant::I4(1));
        in_params.insert("Brightness".to_string(), wmi::Variant::UI1(b));

        self.con
            .exec_method_native_wrapper(
                "WmiMonitorBrightnessMethods",
                &self.instance_path,
                "WmiSetBrightness",
                in_params,
            )
            .map_err(|e| format!("ExecMethod WmiSetBrightness failed: {}", e))?;

        Ok(())
    }

    fn get_brightness(&self) -> Result<u32, String> {
        let results: Vec<WmiMonitorBrightness> = self
            .con
            .query()
            .map_err(|e| format!("Query WmiMonitorBrightness failed: {}", e))?;

        results
            .first()
            .map(|b| b.current_brightness as u32)
            .ok_or_else(|| "No active WmiMonitorBrightness found".to_string())
    }
}

fn start_brightness_worker() {
    std::thread::Builder::new()
        .name("brightness-worker".into())
        .spawn(move || {
            let ctx_result = WmiBrightnessContext::new();
            let ctx = match ctx_result {
                Ok(c) => {
                    BRIGHTNESS_SUPPORTED.store(true, Ordering::SeqCst);
                    if let Ok(cur) = c.get_brightness() {
                        LAST_KNOWN_BRIGHTNESS.store(cur, Ordering::SeqCst);
                    }
                    Some(c)
                }
                Err(e) => {
                    eprintln!("[BrightnessWorker] Brightness not supported: {}", e);
                    BRIGHTNESS_SUPPORTED.store(false, Ordering::SeqCst);
                    None
                }
            };
            BRIGHTNESS_INITIALIZED.store(true, Ordering::SeqCst);

            loop {
                // Wait for work
                let (target_to_set, gets_to_run) = {
                    let mut lock = BRIGHTNESS_STATE.lock().unwrap();
                    while !lock.pending_set && lock.pending_gets.is_empty() {
                        if IS_QUITTING.load(Ordering::SeqCst) {
                            return;
                        }
                        lock = BRIGHTNESS_CONDVAR.wait(lock).unwrap();
                    }
                    if IS_QUITTING.load(Ordering::SeqCst) {
                        return;
                    }
                    let target = if lock.pending_set {
                        lock.pending_set = false;
                        LATEST_TARGET_BRIGHTNESS.swap(u32::MAX, Ordering::SeqCst)
                    } else {
                        u32::MAX
                    };
                    let gets = std::mem::take(&mut lock.pending_gets);
                    (target, gets)
                };

                // Coalesced Set: only the newest value is executed
                if target_to_set != u32::MAX {
                    if let Some(c) = &ctx {
                        #[cfg(debug_assertions)]
                        let t0 = std::time::Instant::now();

                        let res = c.set_brightness(target_to_set);

                        #[cfg(debug_assertions)]
                        {
                            let elapsed = t0.elapsed();
                            eprintln!(
                                "[BrightnessWorker] set_brightness({}) took {} ms ({:?})",
                                target_to_set,
                                elapsed.as_millis(),
                                elapsed
                            );
                        }

                        if let Err(e) = res {
                            eprintln!("[BrightnessWorker] set_brightness failed: {}", e);
                            if !BRIGHTNESS_ERROR_EMITTED.swap(true, Ordering::SeqCst) {
                                if let Ok(guard) = BRIGHTNESS_APP_HANDLE.lock() {
                                    if let Some(app) = guard.as_ref() {
                                        use tauri::Emitter;
                                        let _ = app.emit("brightness-error", e);
                                    }
                                }
                            }
                        } else {
                            LAST_KNOWN_BRIGHTNESS.store(target_to_set, Ordering::SeqCst);
                        }
                    }
                }

                // Process Gets (polling)
                if !gets_to_run.is_empty() {
                    let res = if let Some(c) = &ctx {
                        c.get_brightness()
                    } else {
                        Err("unsupported".into())
                    };
                    if let Ok(lvl) = res {
                        LAST_KNOWN_BRIGHTNESS.store(lvl, Ordering::SeqCst);
                    }
                    for tx in gets_to_run {
                        let _ = tx.send(res.clone());
                    }
                }
            }
        })
        .expect("Failed to spawn brightness worker thread");
}

pub fn init_brightness_worker(app_handle: tauri::AppHandle) {
    if let Ok(mut lock) = BRIGHTNESS_APP_HANDLE.lock() {
        *lock = Some(app_handle);
    }
    BRIGHTNESS_WORKER_ONCE.call_once(start_brightness_worker);
}

fn ensure_brightness_worker_started() {
    BRIGHTNESS_WORKER_ONCE.call_once(start_brightness_worker);
}

#[tauri::command]
fn get_brightness() -> Result<u32, String> {
    ensure_brightness_worker_started();
    if !BRIGHTNESS_INITIALIZED.load(Ordering::SeqCst) {
        for _ in 0..20 {
            if BRIGHTNESS_INITIALIZED.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    if !BRIGHTNESS_SUPPORTED.load(Ordering::SeqCst) {
        return Err("unsupported".into());
    }

    let (tx, rx) = std::sync::mpsc::channel();
    {
        let mut lock = BRIGHTNESS_STATE.lock().unwrap();
        lock.pending_gets.push(tx);
        BRIGHTNESS_CONDVAR.notify_one();
    }
    match rx.recv_timeout(std::time::Duration::from_millis(800)) {
        Ok(res) => res,
        Err(_) => Ok(LAST_KNOWN_BRIGHTNESS.load(Ordering::SeqCst)),
    }
}

#[tauri::command]
fn set_brightness(brightness: u32) -> Result<(), String> {
    ensure_brightness_worker_started();
    if !BRIGHTNESS_SUPPORTED.load(Ordering::SeqCst) && BRIGHTNESS_INITIALIZED.load(Ordering::SeqCst) {
        return Err("unsupported".into());
    }
    LATEST_TARGET_BRIGHTNESS.store(brightness.clamp(0, 100), Ordering::SeqCst);
    {
        let mut lock = BRIGHTNESS_STATE.lock().unwrap();
        lock.pending_set = true;
        BRIGHTNESS_CONDVAR.notify_one();
    }
    // Fire and forget: returns immediately
    Ok(())
}

// --- Battery Status ---
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
    pub has_battery: bool,
    pub saver: bool,
}

#[tauri::command]
fn get_battery() -> Result<BatteryStatus, String> {
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            let has_battery = status.BatteryFlag != 128 && status.BatteryLifePercent <= 100;
            let percent = if has_battery { status.BatteryLifePercent } else { 0 };
            let charging = status.ACLineStatus == 1 || (status.BatteryFlag & 8) != 0;
            let saver = status.SystemStatusFlag == 1;
            Ok(BatteryStatus {
                percent,
                charging,
                has_battery,
                saver,
            })
        } else {
            Ok(BatteryStatus {
                percent: 0,
                charging: false,
                has_battery: false,
                saver: false,
            })
        }
    }
}

#[tauri::command]
fn toggle_battery_saver() -> Result<bool, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let current = unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            status.SystemStatusFlag == 1
        } else {
            false
        }
    };
    let next_saver = !current;
    let threshold = if next_saver { 100 } else { 20 };

    let cmd = format!(
        "powercfg /setdcvalueindex SCHEME_CURRENT SUB_ENERGYSAVER ESBATTTHRESHOLD {}; powercfg /setactive SCHEME_CURRENT",
        threshold
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    Ok(next_saver)
}

// --- Windows Theme (Dark / Light Mode) ---
use windows::core::w;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_VALUE_TYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

fn read_windows_theme_internal() -> Result<bool, String> {
    unsafe {
        let mut hkey = HKEY::default();
        let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey,
            0,
            KEY_READ,
            &mut hkey,
        );
        if status.is_err() {
            return Err("Failed to open registry key".into());
        }

        let mut data: u32 = 0;
        let mut data_size: u32 = std::mem::size_of::<u32>() as u32;
        let mut val_type = REG_VALUE_TYPE::default();

        let val_name = w!("AppsUseLightTheme");
        let query_status = RegQueryValueExW(
            hkey,
            val_name,
            None,
            Some(&mut val_type),
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);

        if query_status.is_ok() {
            Ok(data == 1)
        } else {
            Ok(false)
        }
    }
}

fn set_windows_theme_internal(light: bool) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey,
            0,
            KEY_SET_VALUE | KEY_READ,
            &mut hkey,
        );
        if status.is_err() {
            return Err("Failed to open registry key for writing".into());
        }

        let val: u32 = if light { 1 } else { 0 };
        let bytes = val.to_ne_bytes();

        let apps_val_name = w!("AppsUseLightTheme");
        let res1 = RegSetValueExW(
            hkey,
            apps_val_name,
            0,
            REG_DWORD,
            Some(&bytes),
        );

        let sys_val_name = w!("SystemUsesLightTheme");
        let res2 = RegSetValueExW(
            hkey,
            sys_val_name,
            0,
            REG_DWORD,
            Some(&bytes),
        );

        let _ = RegCloseKey(hkey);

        if res1.is_err() || res2.is_err() {
            return Err("Failed to write registry value".into());
        }

        // Broadcast the change with SendMessageTimeoutW so open apps update immediately
        let section = w!("ImmersiveColorSet");
        let mut result: usize = 0;
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(section.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            100,
            Some(&mut result),
        );

        Ok(())
    }
}

#[tauri::command]
fn get_windows_theme() -> Result<bool, String> {
    read_windows_theme_internal()
}

#[tauri::command]
fn toggle_windows_theme() -> Result<bool, String> {
    let current_light = read_windows_theme_internal().unwrap_or(false);
    let next_light = !current_light;
    set_windows_theme_internal(next_light)?;
    Ok(next_light)
}

#[tauri::command]
fn get_os_diagnostic_info() -> Result<String, String> {
    unsafe {
        let mut hkey = HKEY::default();
        let subkey = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey, 0, KEY_READ, &mut hkey).is_ok() {
            let read_sz = |val_name: windows::core::PCWSTR| -> Option<String> {
                let mut buf = [0u16; 256];
                let mut size: u32 = (buf.len() * 2) as u32;
                let mut val_type = REG_VALUE_TYPE::default();
                if RegQueryValueExW(
                    hkey,
                    val_name,
                    None,
                    Some(&mut val_type),
                    Some(buf.as_mut_ptr() as *mut u8),
                    Some(&mut size),
                )
                .is_ok()
                {
                    let len = (size as usize / 2).saturating_sub(1);
                    Some(String::from_utf16_lossy(&buf[..len]).trim_matches('\0').to_string())
                } else {
                    None
                }
            };

            let build = read_sz(windows::core::w!("CurrentBuild")).unwrap_or_default();
            let display_version = read_sz(windows::core::w!("DisplayVersion")).unwrap_or_default();
            let _ = RegCloseKey(hkey);

            let is_win11 = build.parse::<u32>().unwrap_or(0) >= 22000;
            let win_name = if is_win11 { "Windows 11" } else { "Windows 10" };
            if !display_version.is_empty() && !build.is_empty() {
                return Ok(format!("{} (Version {}, Build {})", win_name, display_version, build));
            } else if !build.is_empty() {
                return Ok(format!("{} (Build {})", win_name, build));
            }
        }
    }
    Ok("Windows".to_string())
}

// --- Control Center Support (Media, Network, Wi-Fi, BT, Airplane, Hotspot, Brightness, Audio) ---
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    CurrentSessionChangedEventArgs,
    GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs,
};
use windows::Networking::Connectivity::NetworkInformation;
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringManager, TetheringCapability, TetheringOperationalState,
};
use windows::Storage::Streams::DataReader;

static AIRPLANE_PREV_STATE: Mutex<Option<(bool, bool)>> = Mutex::new(None);

#[tauri::command]
async fn get_radio_state(radio_type: String) -> Result<bool, String> {
    let target_kind = match radio_type.to_lowercase().as_str() {
        "wifi" | "wi-fi" => RadioKind::WiFi,
        "bluetooth" => RadioKind::Bluetooth,
        _ => return Err("Invalid radio type".into()),
    };

    let op = Radio::GetRadiosAsync().map_err(|e| e.to_string())?;
    let radios = op.get().map_err(|e| e.to_string())?;
    for radio in radios {
        if let Ok(kind) = radio.Kind() {
            if kind == target_kind {
                if let Ok(state) = radio.State() {
                    return Ok(state == RadioState::On);
                }
            }
        }
    }
    Err("Radio not found".into())
}

#[tauri::command]
async fn set_radio_state(radio_type: String, enabled: bool) -> Result<(), String> {
    let target_kind = match radio_type.to_lowercase().as_str() {
        "wifi" | "wi-fi" => RadioKind::WiFi,
        "bluetooth" => RadioKind::Bluetooth,
        _ => return Err("Invalid radio type".into()),
    };

    let op = Radio::GetRadiosAsync().map_err(|e| e.to_string())?;
    let radios = op.get().map_err(|e| e.to_string())?;
    let target_state = if enabled {
        RadioState::On
    } else {
        RadioState::Off
    };

    for radio in radios {
        if let Ok(kind) = radio.Kind() {
            if kind == target_kind {
                let set_op = radio.SetStateAsync(target_state).map_err(|e| e.to_string())?;
                let status = set_op.get().map_err(|e| e.to_string())?;
                if status == RadioAccessStatus::Allowed {
                    return Ok(());
                } else {
                    return Err("ACCESS_DENIED".into());
                }
            }
        }
    }
    Err("Radio not found".into())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaSessionPayload {
    pub has_session: bool,
    pub is_playing: bool,
    pub title: String,
    pub artist: String,
    pub thumbnail: Option<String>,
    pub elapsed_secs: u64,
    pub total_secs: u64,
    pub can_seek: bool,
    pub is_playback_position_enabled: bool,
    pub start_time_ticks: i64,
    pub end_time_ticks: i64,
    pub position_ticks: i64,
    pub position: i64,
    pub start_time: i64,
    pub end_time: i64,
    pub duration: i64,
    pub last_updated_time: i64,
    pub playback_status: String,
    pub playback_rate: f64,
    pub track_id: String,
    pub estimated_position: i64,
}

impl Default for MediaSessionPayload {
    fn default() -> Self {
        Self {
            has_session: false,
            is_playing: false,
            title: "Nothing playing".to_string(),
            artist: "".to_string(),
            thumbnail: None,
            elapsed_secs: 0,
            total_secs: 0,
            can_seek: false,
            is_playback_position_enabled: false,
            start_time_ticks: 0,
            end_time_ticks: 0,
            position_ticks: 0,
            position: 0,
            start_time: 0,
            end_time: 0,
            duration: 0,
            last_updated_time: 0,
            playback_status: "Closed".to_string(),
            playback_rate: 1.0,
            track_id: "".to_string(),
            estimated_position: 0,
        }
    }
}

fn open_system_uri(uri: &str) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(uri)
        .spawn();
}

async fn get_current_media_session_async() -> Result<GlobalSystemMediaTransportControlsSession, String> {
    let op = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .map_err(|e| e.to_string())?;
    let manager = op.get().map_err(|e| e.to_string())?;
    manager.GetCurrentSession().map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_media_session() -> Result<MediaSessionPayload, String> {
    let session = match get_current_media_session_async().await {
        Ok(s) => s,
        Err(_) => return Ok(MediaSessionPayload::default()),
    };

    let mut payload = MediaSessionPayload {
        has_session: true,
        ..Default::default()
    };

    let mut status_val = GlobalSystemMediaTransportControlsSessionPlaybackStatus::Closed;
    let mut rate_val = 1.0f64;

    if let Ok(playback_info) = session.GetPlaybackInfo() {
        if let Ok(status) = playback_info.PlaybackStatus() {
            status_val = status;
            payload.is_playing = status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        }
        if let Ok(rate_ref) = playback_info.PlaybackRate() {
            if let Ok(val) = rate_ref.Value() {
                if val > 0.0 {
                    rate_val = val;
                }
            }
        }
        if let Ok(controls) = playback_info.Controls() {
            let enabled = controls.IsPlaybackPositionEnabled().unwrap_or(false);
            payload.is_playback_position_enabled = enabled;
            payload.can_seek = enabled;
        }
    }
    payload.playback_rate = rate_val;
    payload.playback_status = match status_val {
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => "Playing".to_string(),
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => "Paused".to_string(),
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Stopped => "Stopped".to_string(),
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Changing => "Changing".to_string(),
        GlobalSystemMediaTransportControlsSessionPlaybackStatus::Opened => "Opened".to_string(),
        _ => "Closed".to_string(),
    };

    if let Ok(props_op) = session.TryGetMediaPropertiesAsync() {
        if let Ok(props) = props_op.get() {
            if let Ok(title) = props.Title() {
                let t = title.to_string_lossy();
                if !t.trim().is_empty() {
                    payload.title = t;
                }
            }
            if let Ok(artist) = props.Artist() {
                payload.artist = artist.to_string_lossy();
            }

            if let Ok(thumb_ref) = props.Thumbnail() {
                if let Ok(open_op) = thumb_ref.OpenReadAsync() {
                    if let Ok(stream) = open_op.get() {
                        if let Ok(size) = stream.Size() {
                            if size > 0 && size < 5 * 1024 * 1024 {
                                if let Ok(reader) = DataReader::CreateDataReader(&stream) {
                                    if let Ok(load_op) = reader.LoadAsync(size as u32) {
                                        if load_op.get().is_ok() {
                                            let mut buf = vec![0u8; size as usize];
                                            if reader.ReadBytes(&mut buf).is_ok() {
                                                let content_type = stream
                                                    .ContentType()
                                                    .map(|ct| ct.to_string_lossy())
                                                    .unwrap_or_default();
                                                let mime = if content_type.starts_with("image/") {
                                                    content_type
                                                } else {
                                                    "image/jpeg".to_string()
                                                };
                                                use base64::Engine;
                                                let b64 = base64::engine::general_purpose::STANDARD
                                                    .encode(&buf);
                                                payload.thumbnail =
                                                    Some(format!("data:{};base64,{}", mime, b64));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    payload.track_id = if !payload.title.is_empty() || !payload.artist.is_empty() {
        format!("{}:{}", payload.title, payload.artist)
    } else {
        "".to_string()
    };

    if let Ok(timeline) = session.GetTimelineProperties() {
        let pos_100ns = timeline.Position().map(|t| t.Duration).unwrap_or(0);
        let end_100ns = timeline.EndTime().map(|t| t.Duration).unwrap_or(0);
        let start_100ns = timeline.StartTime().map(|t| t.Duration).unwrap_or(0);
        let last_updated_100ns = timeline.LastUpdatedTime().map(|dt| dt.UniversalTime).unwrap_or(0);

        let dur_100ns = if end_100ns > start_100ns {
            end_100ns - start_100ns
        } else {
            0
        };
        payload.start_time_ticks = start_100ns;
        payload.end_time_ticks = end_100ns;
        payload.position_ticks = pos_100ns;
        payload.elapsed_secs = (pos_100ns.saturating_sub(start_100ns).max(0) / 10_000_000) as u64;
        payload.total_secs = (dur_100ns / 10_000_000).max(0) as u64;

        let raw_pos_ms = pos_100ns / 10_000;
        let start_ms = start_100ns / 10_000;
        let end_ms = end_100ns / 10_000;
        let last_updated_ms = last_updated_100ns / 10_000;
        let duration_ms = (end_ms - start_ms).max(0);
        let position_ms = (raw_pos_ms - start_ms).clamp(0, duration_ms);

        payload.start_time = start_ms;
        payload.end_time = end_ms;
        payload.duration = duration_ms;
        payload.position = position_ms;
        payload.last_updated_time = last_updated_ms;

        // Convert current system time to FILETIME ms epoch (11,644,473,600,000 ms between 1601 and 1970)
        let unix_now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let now_filetime_ms = unix_now_ms + 11_644_473_600_000;

        let estimated_position = if status_val == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing {
            let elapsed_ms = if last_updated_ms > 0 && now_filetime_ms >= last_updated_ms {
                (now_filetime_ms - last_updated_ms) as f64
            } else {
                0.0
            };
            let est = (position_ms as f64) + elapsed_ms * rate_val;
            (est.round() as i64).clamp(0, duration_ms)
        } else {
            position_ms
        };
        payload.estimated_position = estimated_position;

        if end_100ns == 0 {
            payload.can_seek = false;
            payload.is_playback_position_enabled = false;
        }

        // Requirement 9: Debug log for poll
        println!(
            "[Music Poll Debug] raw pos ms: {}, extrapolated ms: {}, duration ms: {}, status: {}, IsPlaybackPositionEnabled: {}",
            position_ms, estimated_position, duration_ms, payload.playback_status, payload.is_playback_position_enabled
        );
    } else {
        payload.can_seek = false;
        payload.is_playback_position_enabled = false;
    }

    Ok(payload)
}

#[tauri::command]
async fn media_play_pause() -> Result<(), String> {
    let session = get_current_media_session_async().await?;
    let op = session.TryTogglePlayPauseAsync().map_err(|e| e.to_string())?;
    let _ = op.get().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn media_previous() -> Result<(), String> {
    let session = get_current_media_session_async().await?;
    let op = session.TrySkipPreviousAsync().map_err(|e| e.to_string())?;
    let _ = op.get().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn media_next() -> Result<(), String> {
    let session = get_current_media_session_async().await?;
    let op = session.TrySkipNextAsync().map_err(|e| e.to_string())?;
    let _ = op.get().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn seek_to(target_ms: i64) -> Result<bool, String> {
    let session = get_current_media_session_async().await?;
    let start_ticks = session
        .GetTimelineProperties()
        .ok()
        .and_then(|t| t.StartTime().ok())
        .map(|t| t.Duration)
        .unwrap_or(0);
    let start_ms = start_ticks / 10_000;
    let ticks = (target_ms + start_ms) * 10_000;

    let op = session
        .TryChangePlaybackPositionAsync(ticks)
        .map_err(|e| e.to_string())?;
    let success = op.get().map_err(|e| e.to_string())?;

    // Requirement 9: Debug log for seek
    println!(
        "[Music Seek Debug] target ms: {}, start ms: {}, ticks sent: {}, result: {}",
        target_ms, start_ms, ticks, success
    );

    Ok(success)
}

#[tauri::command]
async fn media_seek(
    position_ticks: Option<i64>,
    ticks: Option<i64>,
    position_secs: Option<u64>,
) -> Result<bool, String> {
    let target = position_ticks
        .or(ticks)
        .unwrap_or_else(|| (position_secs.unwrap_or(0) as i64) * 10_000_000);
    let session = get_current_media_session_async().await?;
    let op = session
        .TryChangePlaybackPositionAsync(target)
        .map_err(|e| e.to_string())?;
    let success = op.get().map_err(|e| e.to_string())?;
    println!("[Music Seek Debug Legacy] ticks: {}, result: {}", target, success);
    Ok(success)
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct NowPlayingPayload {
    pub status: String, // "playing" | "paused" | "none"
    pub title: String,
    pub artist: String,
    pub thumbnail: Option<String>,
}

fn downscale_thumbnail_bytes(buf: &[u8]) -> Option<String> {
    if let Ok(img) = image::load_from_memory(buf) {
        let (w, h) = (img.width(), img.height());
        let resized = if w > 64 || h > 64 {
            img.resize(64, 64, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let mut cursor = std::io::Cursor::new(Vec::new());
        if resized.write_to(&mut cursor, image::ImageFormat::Png).is_ok() {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(cursor.into_inner());
            return Some(format!("data:image/png;base64,{}", b64));
        }
    }
    None
}

struct NowPlayingSessionState {
    status: String,
    title: String,
    artist: String,
    raw_thumbnail_bytes: Option<Vec<u8>>,
}

fn read_now_playing_session_state(session: &GlobalSystemMediaTransportControlsSession) -> NowPlayingSessionState {
    let mut status = "none".to_string();
    if let Ok(playback_info) = session.GetPlaybackInfo() {
        if let Ok(status_val) = playback_info.PlaybackStatus() {
            status = match status_val {
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => "playing".to_string(),
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => "paused".to_string(),
                _ => "none".to_string(),
            };
        }
    }

    let mut title = String::new();
    let mut artist = String::new();
    let mut raw_thumbnail_bytes = None;

    if let Ok(props_op) = session.TryGetMediaPropertiesAsync() {
        if let Ok(props) = props_op.get() {
            if let Ok(t) = props.Title() {
                let s = t.to_string_lossy();
                if !s.trim().is_empty() {
                    title = s;
                }
            }
            if let Ok(a) = props.Artist() {
                artist = a.to_string_lossy();
            }

            if let Ok(thumb_ref) = props.Thumbnail() {
                if let Ok(open_op) = thumb_ref.OpenReadAsync() {
                    if let Ok(stream) = open_op.get() {
                        if let Ok(size) = stream.Size() {
                            if size > 0 && size < 5 * 1024 * 1024 {
                                if let Ok(reader) = DataReader::CreateDataReader(&stream) {
                                    if let Ok(load_op) = reader.LoadAsync(size as u32) {
                                        if load_op.get().is_ok() {
                                            let mut buf = vec![0u8; size as usize];
                                            if reader.ReadBytes(&mut buf).is_ok() {
                                                raw_thumbnail_bytes = Some(buf);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    NowPlayingSessionState {
        status,
        title,
        artist,
        raw_thumbnail_bytes,
    }
}

pub fn init_now_playing_worker(app_handle: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        run_now_playing_loop(app_handle).await;
    });
}

async fn run_now_playing_loop(app_handle: tauri::AppHandle) {
    let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
        Ok(op) => match op.get() {
            Ok(m) => m,
            Err(_) => return,
        },
        Err(_) => return,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let tx_mgr = tx.clone();
    let _ = manager.CurrentSessionChanged(&TypedEventHandler::<
        GlobalSystemMediaTransportControlsSessionManager,
        CurrentSessionChangedEventArgs,
    >::new(move |_sender, _args| {
        let _ = tx_mgr.send(());
        Ok(())
    }));

    let _ = tx.send(());

    let mut last_emitted_status = String::new();
    let mut last_emitted_title = String::new();
    let mut last_emitted_artist = String::new();
    let mut last_track_key = String::new();
    let mut thumbnail_sent_for_track = false;

    let mut active_session: Option<GlobalSystemMediaTransportControlsSession> = None;

    loop {
        let has_session = active_session.is_some();

        if has_session {
            tokio::select! {
                biased;
                _ = rx.recv() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {},
            }
        } else {
            if rx.recv().await.is_none() {
                break;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}

        let current_session_res = manager.GetCurrentSession();
        match current_session_res {
            Ok(session) => {
                let is_new_session = match &active_session {
                    None => true,
                    Some(prev) => prev != &session,
                };

                if is_new_session {
                    let tx_props = tx.clone();
                    let _ = session.MediaPropertiesChanged(&TypedEventHandler::<
                        GlobalSystemMediaTransportControlsSession,
                        MediaPropertiesChangedEventArgs,
                    >::new(move |_sender, _args| {
                        let _ = tx_props.send(());
                        Ok(())
                    }));

                    let tx_playback = tx.clone();
                    let _ = session.PlaybackInfoChanged(&TypedEventHandler::<
                        GlobalSystemMediaTransportControlsSession,
                        PlaybackInfoChangedEventArgs,
                    >::new(move |_sender, _args| {
                        let _ = tx_playback.send(());
                        Ok(())
                    }));

                    active_session = Some(session.clone());
                }

                let state = read_now_playing_session_state(&session);
                let current_track_key = format!("{}:{}", state.title, state.artist);
                let track_changed = current_track_key != last_track_key;

                if track_changed {
                    last_track_key = current_track_key;
                    thumbnail_sent_for_track = false;
                }

                let mut thumbnail_to_send: Option<String> = None;
                if !thumbnail_sent_for_track {
                    if let Some(ref raw_buf) = state.raw_thumbnail_bytes {
                        if let Some(downscaled_b64) = downscale_thumbnail_bytes(raw_buf) {
                            thumbnail_to_send = Some(downscaled_b64);
                            thumbnail_sent_for_track = true;
                        }
                    }
                }

                let status_changed = state.status != last_emitted_status;
                let title_changed = state.title != last_emitted_title;
                let artist_changed = state.artist != last_emitted_artist;
                let thumb_changed = thumbnail_to_send.is_some();

                if status_changed || title_changed || artist_changed || thumb_changed {
                    #[cfg(debug_assertions)]
                    {
                        println!(
                            "[NowPlaying Event] status: {} (prev: {}), track_changed: {}, has_thumb: {}",
                            state.status, last_emitted_status, track_changed, thumbnail_to_send.is_some()
                        );
                    }

                    last_emitted_status = state.status.clone();
                    last_emitted_title = state.title.clone();
                    last_emitted_artist = state.artist.clone();

                    let payload = NowPlayingPayload {
                        status: state.status,
                        title: state.title,
                        artist: state.artist,
                        thumbnail: thumbnail_to_send,
                    };

                    use tauri::Emitter;
                    let _ = app_handle.emit("now-playing", payload);
                }
            }
            Err(_) => {
                active_session = None;
                thumbnail_sent_for_track = false;
                last_track_key.clear();

                if last_emitted_status != "none" {
                    #[cfg(debug_assertions)]
                    println!("[NowPlaying Event] status: none (session ended)");

                    last_emitted_status = "none".to_string();
                    last_emitted_title.clear();
                    last_emitted_artist.clear();

                    let payload = NowPlayingPayload {
                        status: "none".to_string(),
                        title: String::new(),
                        artist: String::new(),
                        thumbnail: None,
                    };

                    use tauri::Emitter;
                    let _ = app_handle.emit("now-playing", payload);
                }
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WifiStatus {
    pub enabled: bool,
    pub ssid: String,
}

fn get_connected_wifi_ssid_internal() -> Option<String> {
    if let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() {
        if let Ok(is_wlan) = profile.IsWlanConnectionProfile() {
            if is_wlan {
                if let Ok(details) = profile.WlanConnectionProfileDetails() {
                    if let Ok(ssid) = details.GetConnectedSsid() {
                        let s = ssid.to_string_lossy();
                        if !s.is_empty() {
                            return Some(s);
                        }
                    }
                }
                if let Ok(name) = profile.ProfileName() {
                    let s = name.to_string_lossy();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }

    if let Ok(profiles) = NetworkInformation::GetConnectionProfiles() {
        for profile in profiles {
            if let Ok(is_wlan) = profile.IsWlanConnectionProfile() {
                if is_wlan {
                    if let Ok(details) = profile.WlanConnectionProfileDetails() {
                        if let Ok(ssid) = details.GetConnectedSsid() {
                            let s = ssid.to_string_lossy();
                            if !s.is_empty() {
                                return Some(s);
                            }
                        }
                    }
                    if let Ok(name) = profile.ProfileName() {
                        let s = name.to_string_lossy();
                        if !s.is_empty() {
                            return Some(s);
                        }
                    }
                }
            }
        }
    }

    None
}

#[tauri::command]
async fn get_wifi_status() -> Result<WifiStatus, String> {
    let enabled = get_radio_state("wifi".into()).await.unwrap_or(false);
    let ssid = if enabled {
        get_connected_wifi_ssid_internal().unwrap_or_else(|| "Not Connected".to_string())
    } else {
        "Off".to_string()
    };
    Ok(WifiStatus { enabled, ssid })
}

#[tauri::command]
async fn toggle_wifi() -> Result<WifiStatus, String> {
    let current = get_radio_state("wifi".into()).await.unwrap_or(false);
    let next_state = !current;
    if let Err(e) = set_radio_state("wifi".into(), next_state).await {
        open_system_uri("ms-settings:network-wifi");
        return Err(e);
    }
    get_wifi_status().await
}

#[tauri::command]
async fn toggle_bluetooth() -> Result<bool, String> {
    let current = get_radio_state("bluetooth".into()).await.unwrap_or(false);
    let next_state = !current;
    if let Err(e) = set_radio_state("bluetooth".into(), next_state).await {
        open_system_uri("ms-settings:bluetooth");
        return Err(e);
    }
    Ok(next_state)
}

#[tauri::command]
async fn get_airplane_status() -> Result<bool, String> {
    let wifi_on = get_radio_state("wifi".into()).await.unwrap_or(false);
    let bt_on = get_radio_state("bluetooth".into()).await.unwrap_or(false);
    Ok(!wifi_on && !bt_on)
}

#[tauri::command]
async fn toggle_airplane() -> Result<bool, String> {
    let wifi_on = get_radio_state("wifi".into()).await.unwrap_or(false);
    let bt_on = get_radio_state("bluetooth".into()).await.unwrap_or(false);
    let is_airplane = !wifi_on && !bt_on;

    if is_airplane {
        // Restore previously remembered states or turn both on
        let (prev_wifi, prev_bt) = {
            let mut lock = AIRPLANE_PREV_STATE.lock().map_err(|e| e.to_string())?;
            lock.take().unwrap_or((true, true))
        };
        let _ = set_radio_state("wifi".into(), prev_wifi).await;
        let _ = set_radio_state("bluetooth".into(), prev_bt).await;
        Ok(false)
    } else {
        // Save current states and turn both off
        {
            let mut lock = AIRPLANE_PREV_STATE.lock().map_err(|e| e.to_string())?;
            *lock = Some((wifi_on, bt_on));
        }
        if wifi_on {
            let _ = set_radio_state("wifi".into(), false).await;
        }
        if bt_on {
            let _ = set_radio_state("bluetooth".into(), false).await;
        }
        Ok(true)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HotspotStatus {
    pub supported: bool,
    pub enabled: bool,
    pub status_text: String,
}

fn query_hotspot_internal() -> HotspotStatus {
    let profile = match NetworkInformation::GetInternetConnectionProfile() {
        Ok(p) => p,
        Err(_) => {
            return HotspotStatus {
                supported: false,
                enabled: false,
                status_text: "Unavailable".to_string(),
            }
        }
    };

    let cap = match NetworkOperatorTetheringManager::GetTetheringCapabilityFromConnectionProfile(&profile) {
        Ok(c) => c,
        Err(_) => {
            return HotspotStatus {
                supported: false,
                enabled: false,
                status_text: "Unavailable".to_string(),
            }
        }
    };

    if cap != TetheringCapability::Enabled {
        return HotspotStatus {
            supported: false,
            enabled: false,
            status_text: "Unavailable".to_string(),
        };
    }

    let manager = match NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile) {
        Ok(m) => m,
        Err(_) => {
            return HotspotStatus {
                supported: false,
                enabled: false,
                status_text: "Unavailable".to_string(),
            }
        }
    };

    let op_state = manager.TetheringOperationalState().unwrap_or(TetheringOperationalState::Off);
    let enabled = op_state == TetheringOperationalState::On;
    HotspotStatus {
        supported: true,
        enabled,
        status_text: if enabled { "On".to_string() } else { "Off".to_string() },
    }
}

#[tauri::command]
fn get_hotspot_status() -> HotspotStatus {
    query_hotspot_internal()
}

#[tauri::command]
async fn toggle_hotspot() -> Result<HotspotStatus, String> {
    let profile = match NetworkInformation::GetInternetConnectionProfile() {
        Ok(p) => p,
        Err(_) => {
            open_system_uri("ms-settings:network-mobilehotspot");
            return Err("Unavailable".to_string());
        }
    };

    let cap = match NetworkOperatorTetheringManager::GetTetheringCapabilityFromConnectionProfile(&profile) {
        Ok(c) => c,
        Err(_) => {
            open_system_uri("ms-settings:network-mobilehotspot");
            return Err("Unavailable".to_string());
        }
    };

    if cap != TetheringCapability::Enabled {
        open_system_uri("ms-settings:network-mobilehotspot");
        return Err("Unavailable".to_string());
    }

    let manager = match NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile) {
        Ok(m) => m,
        Err(_) => {
            open_system_uri("ms-settings:network-mobilehotspot");
            return Err("Unavailable".to_string());
        }
    };

    let current_state = manager.TetheringOperationalState().unwrap_or(TetheringOperationalState::Off);
    let is_on = current_state == TetheringOperationalState::On;

    let op = if is_on {
        manager.StopTetheringAsync().map_err(|e| e.to_string())?
    } else {
        manager.StartTetheringAsync().map_err(|e| e.to_string())?
    };

    let _ = op.get().map_err(|e| e.to_string())?;
    Ok(query_hotspot_internal())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrightnessStatus {
    pub supported: bool,
    pub level: u32,
}

#[tauri::command]
fn get_brightness_status() -> BrightnessStatus {
    match get_brightness() {
        Ok(lvl) => BrightnessStatus {
            supported: true,
            level: lvl,
        },
        Err(_) => BrightnessStatus {
            supported: false,
            level: 0,
        },
    }
}

#[tauri::command]
fn toggle_volume_mute() -> Result<bool, String> {
    let current = get_mute_internal().unwrap_or(false);
    let next_mute = !current;
    set_mute_internal(next_mute)?;
    Ok(next_mute)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ControlCenterState {
    pub media: MediaSessionPayload,
    pub wifi: WifiStatus,
    pub bluetooth: bool,
    pub airplane: bool,
    pub hotspot: HotspotStatus,
    pub brightness: BrightnessStatus,
    pub volume: u32,
    pub muted: bool,
    pub battery: BatteryStatus,
    pub is_light_theme: bool,
}

#[tauri::command]
async fn get_control_center_state() -> Result<ControlCenterState, String> {
    let media = get_media_session().await.unwrap_or_default();
    let wifi = get_wifi_status().await.unwrap_or(WifiStatus {
        enabled: false,
        ssid: "Off".to_string(),
    });
    let bluetooth = get_radio_state("bluetooth".into()).await.unwrap_or(false);
    let airplane = !wifi.enabled && !bluetooth;
    let hotspot = query_hotspot_internal();
    let brightness = get_brightness_status();
    let volume = get_volume_internal().unwrap_or(50);
    let muted = get_mute_internal().unwrap_or(false);
    let battery = get_battery().unwrap_or(BatteryStatus {
        percent: 0,
        charging: false,
        has_battery: false,
        saver: false,
    });
    let is_light_theme = read_windows_theme_internal().unwrap_or(false);

    Ok(ControlCenterState {
        media,
        wifi,
        bluetooth,
        airplane,
        hotspot,
        brightness,
        volume,
        muted,
        battery,
        is_light_theme,
    })
}


// --- System Actions: Lock, Sleep, Screenshot, Settings URI ---
use windows::Win32::System::Shutdown::LockWorkStation;

#[tauri::command]
fn lock_pc() -> Result<(), String> {
    unsafe {
        LockWorkStation().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn lock_workstation() -> Result<(), String> {
    lock_pc()
}

#[tauri::command]
fn sleep_pc() -> Result<(), String> {
    let _ = std::process::Command::new("rundll32.exe")
        .args(["powrprof.dll,SetSuspendState", "0,1,0"])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn open_screenclip() -> Result<(), String> {
    let _ = std::process::Command::new("explorer.exe")
        .arg("ms-screenclip:")
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn open_settings_uri(uri: String) -> Result<(), String> {
    let _ = std::process::Command::new("explorer.exe")
        .arg(&uri)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}


pub fn open_dude_settings_impl(app: tauri::AppHandle, view: Option<String>) -> Result<(), String> {
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = get_or_refresh_models(&app_handle).await;
    });

    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        #[cfg(target_os = "windows")]
        {
            if let Ok(hwnd) = win.hwnd() {
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOW, SetForegroundWindow};
                    let _ = ShowWindow(windows::Win32::Foundation::HWND(hwnd.0 as _), SW_SHOW);
                    let _ = SetForegroundWindow(windows::Win32::Foundation::HWND(hwnd.0 as _));
                }
            }
        }
        if let Some(ref v) = view {
            let _ = win.emit("navigate-view", v);
        }
    } else {
        let url = match &view {
            Some(v) => format!("settings.html?view={}", v),
            None => "settings.html".to_string(),
        };
        let win = tauri::WebviewWindowBuilder::new(
            &app,
            "settings",
            tauri::WebviewUrl::App(url.into()),
        )
        .title("Dude")
        .inner_size(1000.0, 625.0)
        .center()
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .build()
        .map_err(|e| e.to_string())?;

        let _ = win.show();
        let _ = win.set_focus();
        #[cfg(target_os = "windows")]
        {
            if let Ok(hwnd) = win.hwnd() {
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOW, SetForegroundWindow};
                    let _ = ShowWindow(windows::Win32::Foundation::HWND(hwnd.0 as _), SW_SHOW);
                    let _ = SetForegroundWindow(windows::Win32::Foundation::HWND(hwnd.0 as _));
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn open_dude_settings(app: tauri::AppHandle, view: Option<String>) -> Result<(), String> {
    open_dude_settings_impl(app, view)
}

pub fn toggle_mascot_window_impl(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(menu_win) = app.get_webview_window("context-menu") {
        let _ = menu_win.hide();
    }
    if let Some(win) = app.get_webview_window("main") {
        let is_visible = win.is_visible().unwrap_or(true);
        if is_visible {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
    Ok(())
}

#[tauri::command]
fn toggle_mascot_window(app: tauri::AppHandle) -> Result<(), String> {
    toggle_mascot_window_impl(app)
}

pub fn quit_app_impl(app: tauri::AppHandle) {
    IS_QUITTING.store(true, Ordering::SeqCst);
    tray::remove_tray(&app);
    stop_background_workers();
    for (_, win) in app.webview_windows() {
        let _ = win.destroy();
    }
    app.exit(0);
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    quit_app_impl(app);
}

#[tauri::command]
fn show_context_menu(app: tauri::AppHandle, x: f64, y: f64) -> Result<(), String> {
    let target_pos = tauri::Position::Physical(tauri::PhysicalPosition {
        x: x.round() as i32,
        y: y.round() as i32,
    });
    if let Some(win) = app.get_webview_window("context-menu") {
        let _ = win.set_position(target_pos);
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        let win = tauri::WebviewWindowBuilder::new(
            &app,
            "context-menu",
            tauri::WebviewUrl::App("menu.html".into()),
        )
        .title("Dude Menu")
        .inner_size(230.0, 190.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .build()
        .map_err(|e| e.to_string())?;
        let _ = win.set_position(target_pos);
        let _ = win.show();
        let _ = win.set_focus();
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AppSettings {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub personality: Option<String>,
    pub mascot_size: Option<u32>,
    pub pill_color: Option<String>,
    pub position: Option<String>,
    pub position_x: Option<i32>,
    pub position_y: Option<i32>,
    pub roundness: Option<u32>,
    pub opacity: Option<u32>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub parts: Option<Vec<serde_json::Value>>,
}

fn get_settings_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        dir.join("dude_settings.json")
    } else {
        std::path::PathBuf::from("dude_settings.json")
    }
}

fn save_api_key_to_store(key: &str) -> Result<(), String> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| format!("Failed to access credential store: {}", e))?;
    entry.set_password(key)
        .map_err(|e| format!("Failed to save API key to Credential Manager: {}", e))
}

fn get_api_key_from_store() -> Result<Option<String>, String> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| format!("Failed to access credential store: {}", e))?;
    match entry.get_password() {
        Ok(pw) => Ok(Some(pw)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to read API key from Credential Manager: {}", e)),
    }
}

fn delete_api_key_from_store() -> Result<(), String> {
    let entry = Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| format!("Failed to access credential store: {}", e))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Failed to delete API key: {}", e)),
    }
}

#[tauri::command]
fn save_api_key(app: tauri::AppHandle, key: String) -> Result<(), String> {
    let trimmed = key.trim();
    save_api_key_to_store(trimmed)?;
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = get_or_refresh_models(&app_clone).await;
    });
    Ok(())
}

#[tauri::command]
fn get_api_key() -> Result<Option<String>, String> {
    get_api_key_from_store()
}

#[tauri::command]
fn has_api_key() -> Result<bool, String> {
    match get_api_key_from_store() {
        Ok(Some(k)) => Ok(!k.trim().is_empty()),
        _ => Ok(false),
    }
}

#[tauri::command]
fn delete_api_key() -> Result<(), String> {
    delete_api_key_from_store()
}

#[tauri::command]
fn get_onboarded(app: tauri::AppHandle) -> Result<bool, String> {
    let store = app.store(STORE_FILENAME).map_err(|e| e.to_string())?;
    match store.get("onboarded") {
        Some(v) => Ok(v.as_bool().unwrap_or(false)),
        None => Ok(false),
    }
}

#[tauri::command]
fn set_onboarded(app: tauri::AppHandle, onboarded: bool) -> Result<(), String> {
    let store = app.store(STORE_FILENAME).map_err(|e| e.to_string())?;
    store.set("onboarded", serde_json::Value::Bool(onboarded));
    let _ = store.save();
    Ok(())
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> AppSettings {
    let path = get_settings_path(&app);
    let mut current = if let Ok(content) = std::fs::read_to_string(&path) {
        serde_json::from_str::<AppSettings>(&content).unwrap_or_default()
    } else {
        AppSettings::default()
    };
    current.api_key = get_api_key_from_store().ok().flatten().or(current.api_key);
    if current.model.as_deref().map_or(true, |m| m.trim().is_empty() || m == "gemini-flash-latest") {
        current.model = Some("gemini-3.5-flash".to_string());
    }
    current
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: AppSettings) -> Result<(), String> {
    let path = get_settings_path(&app);
    let mut current = get_settings(app.clone());
    if settings.provider.is_some() { current.provider = settings.provider; }
    if let Some(ref k) = settings.api_key {
        let trimmed = k.trim();
        if !trimmed.is_empty() {
            let _ = save_api_key_to_store(trimmed);
            let app_clone = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = get_or_refresh_models(&app_clone).await;
            });
        } else {
            let _ = delete_api_key_from_store();
        }
        current.api_key = Some(trimmed.to_string());
    }
    if settings.model.is_some() { current.model = settings.model; }
    if settings.personality.is_some() { current.personality = settings.personality; }
    if settings.mascot_size.is_some() { current.mascot_size = settings.mascot_size; }
    if settings.pill_color.is_some() { current.pill_color = settings.pill_color; }
    if settings.position.is_some() { current.position = settings.position; }
    if settings.position_x.is_some() { current.position_x = settings.position_x; }
    if settings.position_y.is_some() { current.position_y = settings.position_y; }
    if settings.roundness.is_some() { current.roundness = settings.roundness; }
    if settings.opacity.is_some() { current.opacity = settings.opacity; }
    for (k, v) in settings.extra {
        current.extra.insert(k, v);
    }
    // Never write the API key into the settings JSON file
    let mut to_save = current.clone();
    to_save.api_key = None;
    let json = serde_json::to_string_pretty(&to_save).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    let _ = app.emit("appearance-changed", &current);
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiListModelsResponse {
    #[serde(default)]
    models: Vec<GeminiModelItem>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModelItem {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

static CACHED_MODELS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

async fn fetch_models_from_google(api_key: &str) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let mut page_token: Option<String> = None;
    let mut raw_models: Vec<String> = Vec::new();

    for _ in 0..10 {
        let mut url = "https://generativelanguage.googleapis.com/v1beta/models?pageSize=100".to_string();
        if let Some(ref pt) = page_token {
            if !pt.trim().is_empty() {
                url.push_str("&pageToken=");
                url.push_str(pt.trim());
            }
        }

        let res = client
            .get(&url)
            .header("x-goog-api-key", api_key.trim())
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !res.status().is_success() {
            let status = res.status().as_u16();
            return Err(format!("ListModels failed with status {}", status));
        }

        let body: GeminiListModelsResponse = res
            .json()
            .await
            .map_err(|e| format!("Failed to parse ListModels response: {}", e))?;

        for item in body.models {
            let supports_generate = item
                .supported_generation_methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case("generateContent"));
            if supports_generate {
                let clean_name = item.name.strip_prefix("models/").unwrap_or(&item.name).trim().to_string();
                if !clean_name.is_empty() {
                    raw_models.push(clean_name);
                }
            }
        }

        match body.next_page_token {
            Some(ref pt) if !pt.trim().is_empty() => {
                page_token = Some(pt.clone());
            }
            _ => break,
        }
    }

    let priority_models = [
        "gemini-3.5-flash",
        "gemini-3.5-flash-lite",
        "gemini-3-flash-preview",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
    ];

    let mut top_models: Vec<String> = Vec::new();
    let mut other_flash_models: Vec<String> = Vec::new();
    let mut other_models: Vec<String> = Vec::new();

    for p in priority_models {
        if raw_models.iter().any(|m| m.eq_ignore_ascii_case(p)) {
            top_models.push(p.to_string());
        }
    }

    for m in raw_models {
        if top_models.iter().any(|t| t.eq_ignore_ascii_case(&m)) {
            continue;
        }
        if m == "gemini-flash-latest" {
            continue;
        }
        if m.to_lowercase().contains("flash") {
            if !other_flash_models.contains(&m) {
                other_flash_models.push(m);
            }
        } else {
            if !other_models.contains(&m) {
                other_models.push(m);
            }
        }
    }

    let mut result = Vec::new();
    if top_models.is_empty() {
        result.push("gemini-3.5-flash".to_string());
    } else {
        result.extend(top_models);
    }
    result.extend(other_flash_models);
    result.extend(other_models);

    Ok(result)
}

async fn get_or_refresh_models(app: &tauri::AppHandle) -> Vec<String> {
    let api_key = get_api_key_from_store()
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| get_settings(app.clone()).api_key.filter(|k| !k.trim().is_empty()))
        .or_else(|| std::env::var("GEMINI_API_KEY").ok().filter(|k| !k.trim().is_empty()));

    let default_models = vec![
        "gemini-3.5-flash".to_string(),
        "gemini-3.5-flash-lite".to_string(),
        "gemini-3-flash-preview".to_string(),
    ];

    let api_key = match api_key {
        Some(k) if !k.trim().is_empty() => k,
        _ => {
            *CACHED_MODELS.lock().unwrap() = Some(default_models.clone());
            return default_models;
        }
    };

    let models = match fetch_models_from_google(&api_key).await {
        Ok(m) if !m.is_empty() => m,
        _ => default_models.clone(),
    };

    *CACHED_MODELS.lock().unwrap() = Some(models.clone());
    let _ = app.emit("models-refreshed", models.clone());

    let mut current_settings = get_settings(app.clone());
    let current_model = current_settings.model.as_deref().unwrap_or("gemini-3.5-flash").trim().to_string();
    if !models.contains(&current_model) || current_model == "gemini-flash-latest" {
        let best_model = models.first().cloned().unwrap_or_else(|| "gemini-3.5-flash".to_string());
        current_settings.model = Some(best_model.clone());
        let _ = save_settings(app.clone(), current_settings);
        let _ = app.emit("model-switched", best_model);
    }

    models
}

#[tauri::command]
async fn get_available_models(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    Ok(get_or_refresh_models(&app).await)
}

fn clean_and_truncate_text(text: &str, api_key: &str, max_chars: usize) -> String {
    let mut s = text.to_string();
    let trimmed_key = api_key.trim();
    if !trimmed_key.is_empty() {
        s = s.replace(trimmed_key, "[REDACTED]");
    }
    s = s.replace('\r', " ").replace('\n', " ");
    let trimmed = s.trim();
    if trimmed.chars().count() > max_chars {
        let truncated: String = trimmed.chars().take(max_chars).collect();
        format!("{}...", truncated.trim_end())
    } else {
        trimmed.to_string()
    }
}

fn build_validated_turns(history: &[ChatMessage], new_user_message: &str) -> Vec<(&'static str, String)> {
    let mut history_turns: Vec<(&'static str, String)> = Vec::new();

    for msg in history {
        let text = match msg.text.as_ref().or(msg.content.as_ref()) {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => continue,
        };
        let role = if msg.role.eq_ignore_ascii_case("model") || msg.role.eq_ignore_ascii_case("assistant") {
            "model"
        } else {
            "user"
        };

        if history_turns.is_empty() {
            if role == "user" {
                history_turns.push((role, text));
            }
        } else {
            let last_role = history_turns.last().unwrap().0;
            if role != last_role {
                history_turns.push((role, text));
            } else if role == "user" {
                // If two user turns in a row, replace with the newer one
                history_turns.pop();
                history_turns.push((role, text));
            }
        }
    }

    // Since the new turn will be "user", history must end with a "model" turn so turns alternate.
    if let Some(&(last_role, _)) = history_turns.last() {
        if last_role == "user" {
            history_turns.pop();
        }
    }

    // Append the new user message as the final turn
    history_turns.push(("user", new_user_message.trim().to_string()));

    // Keep the last 10 turns, trimming from the front so it still starts with "user".
    while history_turns.len() > 10 {
        if history_turns.len() >= 2 {
            history_turns.drain(0..2);
        } else {
            break;
        }
    }

    history_turns
}

async fn send_gemini_request_internal(
    app: tauri::AppHandle,
    message: String,
    history: Vec<ChatMessage>,
    include_system_instruction: bool,
) -> Result<String, String> {
    let settings = get_settings(app.clone());
    let api_key = get_api_key_from_store()
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| settings.api_key.filter(|k| !k.trim().is_empty()))
        .or_else(|| std::env::var("GEMINI_API_KEY").ok().filter(|k| !k.trim().is_empty()));

    let api_key = match api_key {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return Err("NO_API_KEY".into()),
    };

    let raw_model = settings
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "gemini-3.5-flash".into());

    let mut model_clean = raw_model.trim();
    if let Some(stripped) = model_clean.strip_prefix("models/") {
        model_clean = stripped.trim();
    }
    if model_clean.is_empty() || model_clean == "gemini-flash-latest" {
        model_clean = "gemini-3.5-flash";
    }

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
        model_clean
    );

    let valid_turns = build_validated_turns(&history, &message);

    let mut body_map = serde_json::Map::new();

    if include_system_instruction {
        let personality = settings
            .personality
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "You are a cute, friendly mascot".into());

        let trimmed_personality = personality.trim();
        if !trimmed_personality.is_empty() {
            body_map.insert(
                "systemInstruction".to_string(),
                serde_json::json!({
                    "parts": [{ "text": trimmed_personality }]
                }),
            );
        }
    } else {
        body_map.insert(
            "generationConfig".to_string(),
            serde_json::json!({
                "maxOutputTokens": 5,
                "temperature": 0.0
            }),
        );
    }

    let contents_val = serde_json::Value::Array(
        valid_turns
            .iter()
            .map(|(role, text)| {
                serde_json::json!({
                    "role": role,
                    "parts": [{ "text": text }]
                })
            })
            .collect(),
    );
    body_map.insert("contents".to_string(), contents_val);

    let has_system_instruction = body_map.contains_key("systemInstruction");
    let roles_in_order: Vec<&str> = valid_turns.iter().map(|(r, _)| *r).collect();
    let roles_str = roles_in_order.join(", ");

    let request_body = serde_json::Value::Object(body_map);

    let req_timeout = if include_system_instruction {
        std::time::Duration::from_secs(25)
    } else {
        std::time::Duration::from_secs(8)
    };

    let client = reqwest::Client::builder()
        .timeout(req_timeout)
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    const RETRY_DELAYS: [u64; 2] = [1, 2];
    let max_attempts = if include_system_instruction { 2 } else { 0 };

    for attempt in 0..=max_attempts {
        if attempt > 0 {
            let delay_secs = RETRY_DELAYS[(attempt - 1).min(RETRY_DELAYS.len() - 1)];
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        }

        let res = match client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", api_key.trim())
            .json(&request_body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if attempt < max_attempts && (e.is_timeout() || e.is_connect()) {
                    continue;
                }
                let err_msg = if e.is_timeout() {
                    "Request timed out. Please try again.".to_string()
                } else {
                    "Network error. Please check your connection.".to_string()
                };

                println!(
                    "[Gemini Debug] model: \"{}\", turns: {}, roles: [{}], systemInstruction: {}, http_status: \"ERROR\", google_error_status: None, google_error_message: {:?}",
                    model_clean,
                    valid_turns.len(),
                    roles_str,
                    has_system_instruction,
                    err_msg
                );
                log::debug!(
                    "[Gemini Debug] model: \"{}\", turns: {}, roles: [{}], systemInstruction: {}, http_status: \"ERROR\", google_error_status: None, google_error_message: {:?}",
                    model_clean,
                    valid_turns.len(),
                    roles_str,
                    has_system_instruction,
                    err_msg
                );

                return Err(err_msg);
            }
        };

        let status = res.status();
        let status_code = status.as_u16();

        if !status.is_success() {
            let err_body = res.text().await.unwrap_or_default();

            let (error_status, error_message) = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&err_body) {
                let err_obj = parsed.get("error");
                let st = err_obj
                    .and_then(|e| e.get("status"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let msg = err_obj
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty());
                (st, msg)
            } else {
                (None, None)
            };

            println!(
                "[Gemini Debug] model: \"{}\", turns: {}, roles: [{}], systemInstruction: {}, http_status: {}, google_error_status: {:?}, google_error_message: {:?}",
                model_clean,
                valid_turns.len(),
                roles_str,
                has_system_instruction,
                status_code,
                error_status,
                error_message
            );
            log::debug!(
                "[Gemini Debug] model: \"{}\", turns: {}, roles: [{}], systemInstruction: {}, http_status: {}, google_error_status: {:?}, google_error_message: {:?}",
                model_clean,
                valid_turns.len(),
                roles_str,
                has_system_instruction,
                status_code,
                error_status,
                error_message
            );

            // Safety net in ask_gemini and test_connection:
            // if Google returns 400, 404, or 503 for the selected model,
            // retry ONCE with modern fallback model "gemini-3.5-flash" (or "gemini-3-flash-preview").
            let is_key_error = err_body.contains("API_KEY_INVALID") || err_body.contains("key not valid");
            let is_model_error = !is_key_error && (
                status_code == 404
                    || status_code == 503
                    || err_body.to_lowercase().contains("model")
                    || err_body.to_lowercase().contains(&model_clean.to_lowercase())
            );

            let fallback_model = if model_clean != "gemini-3.5-flash" {
                "gemini-3.5-flash"
            } else {
                "gemini-3-flash-preview"
            };

            if is_model_error && model_clean != fallback_model {
                let fb_url = format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", fallback_model);
                if let Ok(fb_res) = client
                    .post(&fb_url)
                    .header("Content-Type", "application/json")
                    .header("x-goog-api-key", api_key.trim())
                    .json(&request_body)
                    .send()
                    .await
                {
                    if fb_res.status().is_success() {
                        if let Ok(json_val) = fb_res.json::<serde_json::Value>().await {
                            if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array()) {
                                if let Some(candidate) = candidates.first() {
                                    if let Some(parts) = candidate
                                        .get("content")
                                        .and_then(|c| c.get("parts"))
                                        .and_then(|p| p.as_array())
                                    {
                                        let full_text: String = parts
                                            .iter()
                                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        let trimmed = full_text.trim();
                                        if !trimmed.is_empty() {
                                            let mut s = get_settings(app.clone());
                                            s.model = Some(fallback_model.to_string());
                                            let _ = save_settings(app.clone(), s);
                                            let _ = app.emit("model-switched", fallback_model);

                                            let note = format!("Switched to {} because the chosen model didn't work.", fallback_model);
                                            if include_system_instruction {
                                                return Ok(format!("{}\n\n{}", trimmed, note));
                                            } else {
                                                return Ok(format!("Connected ✓\n{}", note));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let formatted_error = if let Some(ref msg) = error_message {
                let cleaned_msg = clean_and_truncate_text(msg, &api_key, 200);
                if let Some(ref st) = error_status {
                    format!("Gemini error {} ({}): {}", status_code, st, cleaned_msg)
                } else {
                    format!("Gemini error {}: {}", status_code, cleaned_msg)
                }
            } else {
                let trimmed_body = err_body.trim();
                if !trimmed_body.is_empty() && !trimmed_body.starts_with('{') && !trimmed_body.starts_with('<') {
                    let cleaned_body = clean_and_truncate_text(trimmed_body, &api_key, 200);
                    if let Some(ref st) = error_status {
                        format!("Gemini error {} ({}): {}", status_code, st, cleaned_body)
                    } else {
                        format!("Gemini error {}: {}", status_code, cleaned_body)
                    }
                } else if let Some(ref st) = error_status {
                    format!("Gemini error {} ({}).", status_code, st)
                } else {
                    format!("Gemini error {}.", status_code)
                }
            };

            if (status_code == 503 || status_code == 500 || status_code == 429) && attempt < max_attempts {
                continue;
            }

            return Err(formatted_error);
        }

        println!(
            "[Gemini Debug] model: \"{}\", turns: {}, roles: [{}], systemInstruction: {}, http_status: {}, google_error_status: None, google_error_message: None",
            model_clean,
            valid_turns.len(),
            roles_str,
            has_system_instruction,
            status_code
        );
        log::debug!(
            "[Gemini Debug] model: \"{}\", turns: {}, roles: [{}], systemInstruction: {}, http_status: {}, google_error_status: None, google_error_message: None",
            model_clean,
            valid_turns.len(),
            roles_str,
            has_system_instruction,
            status_code
        );

        let json_val: serde_json::Value = res
            .json()
            .await
            .map_err(|_| "Failed to read Gemini response.".to_string())?;

        if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array()) {
            if let Some(candidate) = candidates.first() {
                if let Some(parts) = candidate
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    let full_text: String = parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let trimmed = full_text.trim();
                    if !trimmed.is_empty() {
                        return Ok(trimmed.to_string());
                    }
                }
            }
        }

        return Err("Received an empty response from Gemini.".into());
    }

    Err("Failed to connect to Gemini after retries.".into())
}

#[tauri::command]
async fn ask_gemini(
    app: tauri::AppHandle,
    message: String,
    history: Vec<ChatMessage>,
) -> Result<String, String> {
    send_gemini_request_internal(app, message, history, true).await
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttachmentMeta {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: usize,
    pub kind: String, // "image", "pdf", "text"
    pub has_secret: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttachmentBatchResult {
    pub attachments: Vec<AttachmentMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct StoredAttachment {
    pub meta: AttachmentMeta,
    pub bytes: Vec<u8>,
    pub text_content: Option<String>,
}

static ATTACHMENT_STORE: std::sync::Mutex<Option<std::collections::HashMap<String, StoredAttachment>>> =
    std::sync::Mutex::new(None);

fn with_attachment_store<F, R>(f: F) -> R
where
    F: FnOnce(&mut std::collections::HashMap<String, StoredAttachment>) -> R,
{
    let mut lock = ATTACHMENT_STORE.lock().unwrap_or_else(|p| p.into_inner());
    let store = lock.get_or_insert_with(std::collections::HashMap::new);
    f(store)
}

fn rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    use image::codecs::png::PngEncoder;
    use image::ColorType;
    let mut buf = Vec::new();
    let encoder = PngEncoder::new(&mut buf);
    image::ImageEncoder::write_image(
        encoder,
        rgba,
        width,
        height,
        ColorType::Rgba8.into(),
    )
    .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(buf)
}

fn scan_for_secrets(text: &str) -> bool {
    if text.contains("BEGIN PRIVATE KEY")
        || text.contains("BEGIN RSA PRIVATE KEY")
        || text.contains("BEGIN EC PRIVATE KEY")
        || text.contains("BEGIN OPENSSH PRIVATE KEY")
    {
        return true;
    }

    for word in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`' || c == ';' || c == ',') {
        let clean = word.trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
        if clean.starts_with("AIza") && clean.len() >= 20 {
            return true;
        }
        if clean.starts_with("sk-") && clean.len() >= 15 {
            return true;
        }
        if clean.starts_with("ghp_") && clean.len() >= 20 {
            return true;
        }
        if clean.starts_with("AQ") && clean.len() >= 16 {
            return true;
        }
    }

    let mut iter = text.split_whitespace();
    while let Some(word) = iter.next() {
        if word.eq_ignore_ascii_case("bearer") {
            if let Some(token) = iter.next() {
                let clean_token = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ';' || c == ',');
                if clean_token.len() > 20 {
                    return true;
                }
            }
        }
    }

    false
}

fn mask_secrets(text: &str) -> String {
    let mut out = String::new();
    let words: Vec<&str> = text.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`').collect();
    for piece in words {
        let trimmed = piece.trim_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`');
        if (trimmed.starts_with("AIza") && trimmed.len() >= 20)
            || (trimmed.starts_with("sk-") && trimmed.len() >= 15)
            || (trimmed.starts_with("ghp_") && trimmed.len() >= 20)
            || (trimmed.starts_with("AQ") && trimmed.len() >= 16)
            || trimmed.contains("BEGIN PRIVATE KEY")
            || trimmed.contains("BEGIN RSA PRIVATE KEY")
        {
            out.push_str("[SECRET_MASKED]");
        } else {
            out.push_str(piece);
        }
    }
    out
}

fn get_file_info(ext: &str) -> Option<(&'static str, &'static str)> {
    match ext.to_lowercase().as_str() {
        "png" => Some(("image", "image/png")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "webp" => Some(("image", "image/webp")),
        "gif" => Some(("image", "image/gif")),
        "pdf" => Some(("pdf", "application/pdf")),
        "txt" | "md" | "json" | "csv" | "tsv" | "log" | "js" | "ts" | "jsx" | "tsx"
        | "py" | "rs" | "go" | "java" | "c" | "cpp" | "h" | "cs" | "html" | "css"
        | "xml" | "yml" | "yaml" | "toml" | "ini" | "sql" | "sh" | "ps1" => {
            Some(("text", "text/plain"))
        }
        _ => None,
    }
}

static ATTACHMENT_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn process_file_path(
    path: &std::path::Path,
    current_count: usize,
    current_total_bytes: usize,
) -> Result<StoredAttachment, String> {
    if current_count >= 4 {
        return Err("Maximum 4 attachments allowed.".to_string());
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid file name.".to_string())?
        .to_string();

    if is_sensitive_filename(&file_name) {
        return Err("This file looks sensitive and was not attached.".to_string());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let (kind, mime) = get_file_info(&ext).ok_or_else(|| {
        format!("Unsupported file type: .{}", ext)
    })?;

    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let size = bytes.len();

    let id = format!(
        "att_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
        ATTACHMENT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );

    if kind == "text" {
        if size > 200 * 1024 {
            return Err("Text file exceeds 200 KB limit.".to_string());
        }
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| "Text file is not valid UTF-8.".to_string())?;
        if current_total_bytes + size > 15 * 1024 * 1024 {
            return Err("Total attachments exceed 15 MB limit.".to_string());
        }
        let has_secret = scan_for_secrets(&text);
        Ok(StoredAttachment {
            meta: AttachmentMeta {
                id,
                name: file_name,
                mime: mime.to_string(),
                size,
                kind: kind.to_string(),
                has_secret,
                preview: None,
            },
            bytes,
            text_content: Some(text),
        })
    } else {
        // Image or PDF
        if size > 10 * 1024 * 1024 {
            return Err("File exceeds 10 MB limit.".to_string());
        }
        if current_total_bytes + size > 15 * 1024 * 1024 {
            return Err("Total attachments exceed 15 MB limit.".to_string());
        }
        Ok(StoredAttachment {
            meta: AttachmentMeta {
                id,
                name: file_name,
                mime: mime.to_string(),
                size,
                kind: kind.to_string(),
                has_secret: false,
                preview: None,
            },
            bytes,
            text_content: None,
        })
    }
}

fn sanitize_parts_for_gemini(parts: &[serde_json::Value]) -> Vec<serde_json::Value> {
    parts
        .iter()
        .map(|p| {
            if let Some(obj) = p.as_object() {
                let mut cleaned = serde_json::Map::new();
                for (k, v) in obj {
                    if !k.starts_with('_') {
                        cleaned.insert(k.clone(), v.clone());
                    }
                }
                serde_json::Value::Object(cleaned)
            } else {
                p.clone()
            }
        })
        .collect()
}

fn build_validated_content_turns(
    history: &[ChatMessage],
    new_user_parts: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut turns: Vec<(String, Vec<serde_json::Value>)> = Vec::new();

    for msg in history {
        let role = if msg.role.eq_ignore_ascii_case("model")
            || msg.role.eq_ignore_ascii_case("assistant")
        {
            "model"
        } else {
            "user"
        };

        let parts: Vec<serde_json::Value> = if let Some(ref p) = msg.parts {
            sanitize_parts_for_gemini(p)
        } else if let Some(ref t) = msg.text.as_ref().or(msg.content.as_ref()) {
            let trimmed = t.trim();
            if trimmed.is_empty() {
                continue;
            }
            vec![serde_json::json!({ "text": trimmed })]
        } else {
            continue;
        };

        if parts.is_empty() {
            continue;
        }

        if turns.is_empty() {
            if role == "user" {
                turns.push((role.to_string(), parts));
            }
        } else {
            let last_role = &turns.last().unwrap().0;
            if role != last_role {
                turns.push((role.to_string(), parts));
            } else if role == "user" {
                turns.pop();
                turns.push((role.to_string(), parts));
            }
        }
    }

    if let Some((last_role, _)) = turns.last() {
        if last_role == "user" {
            turns.pop();
        }
    }

    let sanitized_new_parts = sanitize_parts_for_gemini(&new_user_parts);
    turns.push(("user".to_string(), sanitized_new_parts));

    while turns.len() > 10 {
        if turns.len() >= 2 {
            turns.drain(0..2);
        } else {
            break;
        }
    }

    turns
        .into_iter()
        .map(|(role, parts)| {
            serde_json::json!({
                "role": role,
                "parts": parts
            })
        })
        .collect()
}

struct ActiveChatRequest {
    request_id: String,
    user_message: String,
    user_parts: Vec<serde_json::Value>,
    accumulated_text: std::sync::Arc<std::sync::Mutex<String>>,
    abort_handle: tokio::task::AbortHandle,
}

struct ChatManager {
    history: Vec<ChatMessage>,
    active: Option<ActiveChatRequest>,
}

static CHAT_MANAGER: std::sync::Mutex<Option<ChatManager>> = std::sync::Mutex::new(None);

fn with_chat_manager<F, R>(f: F) -> R
where
    F: FnOnce(&mut ChatManager) -> R,
{
    let mut lock = CHAT_MANAGER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mgr = lock.get_or_insert_with(|| ChatManager {
        history: Vec::new(),
        active: None,
    });
    f(mgr)
}

#[tauri::command]
fn chat_cancel(request_id: String) -> Result<(), String> {
    with_chat_manager(|mgr| {
        let should_cancel = if let Some(ref active) = mgr.active {
            active.request_id == request_id || request_id.is_empty()
        } else {
            false
        };

        if should_cancel {
            if let Some(active) = mgr.active.take() {
                active.abort_handle.abort();
                let partial = {
                    let lock = active.accumulated_text.lock().unwrap_or_else(|p| p.into_inner());
                    lock.clone()
                };
                let trimmed = partial.trim().to_string();
                if !trimmed.is_empty() {
                    mgr.history.push(ChatMessage {
                        role: "user".to_string(),
                        text: Some(active.user_message),
                        content: None,
                        parts: Some(active.user_parts),
                    });
                    mgr.history.push(ChatMessage {
                        role: "model".to_string(),
                        text: Some(trimmed),
                        content: None,
                        parts: None,
                    });
                    while mgr.history.len() > 10 {
                        mgr.history.drain(0..1);
                    }
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn chat_clear() -> Result<(), String> {
    with_chat_manager(|mgr| {
        if let Some(active) = mgr.active.take() {
            active.abort_handle.abort();
        }
        mgr.history.clear();
    });
    with_attachment_store(|store| {
        store.clear();
    });
    Ok(())
}

#[tauri::command]
fn chat_pop_last_turn() -> Result<(), String> {
    with_chat_manager(|mgr| {
        if let Some(last) = mgr.history.last() {
            if last.role == "model" {
                mgr.history.pop();
            }
        }
        if let Some(last) = mgr.history.last() {
            if last.role == "user" {
                mgr.history.pop();
            }
        }
    });
    Ok(())
}

static IS_FILE_DRAG_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static IS_PROCESSING_DROP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DRAG_FILE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[allow(dead_code)]
#[derive(serde::Serialize, Clone)]
pub struct FileDropResult {
    pub count: usize,
    pub accepted: usize,
    pub rejected: Vec<String>,
    pub attachments: Vec<AttachmentMeta>,
}

#[allow(dead_code)]
fn process_dropped_files_for_pipeline(paths: &[std::path::PathBuf]) -> FileDropResult {
    with_attachment_store(|store| {
        let mut added = Vec::new();
        let mut rejected = Vec::new();
        let mut staged_bytes: usize = store.values().map(|a| a.bytes.len()).sum();
        let mut count = store.len();

        for path in paths {
            if path.is_dir() {
                rejected.push("folder".to_string());
                continue;
            }
            if count >= 4 {
                rejected.push("too-many".to_string());
                continue;
            }
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => {
                    rejected.push("unsupported".to_string());
                    continue;
                }
            };
            if is_sensitive_filename(&file_name) {
                rejected.push("sensitive".to_string());
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let (kind, mime) = match get_file_info(&ext) {
                Some(km) => km,
                None => {
                    rejected.push("unsupported".to_string());
                    continue;
                }
            };
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(_) => {
                    rejected.push("unsupported".to_string());
                    continue;
                }
            };
            let size = bytes.len();
            let id = format!(
                "att_{}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                ATTACHMENT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );

            if kind == "text" {
                if size > 200 * 1024 || (staged_bytes + size > 15 * 1024 * 1024) {
                    rejected.push("too-large".to_string());
                    continue;
                }
                let text = match String::from_utf8(bytes.clone()) {
                    Ok(t) => t,
                    Err(_) => {
                        rejected.push("unsupported".to_string());
                        continue;
                    }
                };
                if scan_for_secrets(&text) {
                    rejected.push("needs-check".to_string());
                    continue;
                }
                count += 1;
                staged_bytes += size;
                let meta = AttachmentMeta {
                    id: id.clone(),
                    name: file_name,
                    mime: mime.to_string(),
                    size,
                    kind: kind.to_string(),
                    has_secret: false,
                    preview: None,
                };
                added.push(meta.clone());
                store.insert(id, StoredAttachment {
                    meta,
                    bytes,
                    text_content: Some(text),
                });
            } else {
                if size > 10 * 1024 * 1024 || (staged_bytes + size > 15 * 1024 * 1024) {
                    rejected.push("too-large".to_string());
                    continue;
                }
                count += 1;
                staged_bytes += size;
                let meta = AttachmentMeta {
                    id: id.clone(),
                    name: file_name,
                    mime: mime.to_string(),
                    size,
                    kind: kind.to_string(),
                    has_secret: false,
                    preview: None,
                };
                added.push(meta.clone());
                store.insert(id, StoredAttachment {
                    meta,
                    bytes,
                    text_content: None,
                });
            }
        }

        FileDropResult {
            count: paths.len(),
            accepted: added.len(),
            rejected,
            attachments: added,
        }
    })
}

#[tauri::command]
fn is_lbutton_down() -> bool {
    extern "system" {
        fn GetAsyncKeyState(vKey: i32) -> i16;
    }
    unsafe { (GetAsyncKeyState(0x01) as u16 & 0x8000) != 0 }
}

#[allow(dead_code)]
fn process_dropped_file_paths(paths: Vec<std::path::PathBuf>) -> AttachmentBatchResult {
    with_attachment_store(|store| {
        let mut added = Vec::new();
        let mut first_error = None;

        let mut staged_bytes: usize = store.values().map(|a| a.bytes.len()).sum();
        let mut count = store.len();

        for path in paths {
            if count >= 4 {
                if first_error.is_none() {
                    first_error = Some("Maximum 4 attachments allowed.".to_string());
                }
                break;
            }

            match process_file_path(&path, count, staged_bytes) {
                Ok(att) => {
                    count += 1;
                    staged_bytes += att.bytes.len();
                    added.push(att.meta.clone());
                    store.insert(att.meta.id.clone(), att);
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        AttachmentBatchResult {
            attachments: added,
            error: first_error,
        }
    })
}

#[tauri::command]
async fn pick_attachments(current_ids: Vec<String>) -> Result<AttachmentBatchResult, String> {
    if current_ids.len() >= 4 {
        return Err("Maximum 4 attachments allowed.".to_string());
    }

    let picked = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title("Attach image or file")
            .add_filter(
                "Supported files",
                &[
                    "png", "jpg", "jpeg", "webp", "gif", "pdf", "txt", "md", "json", "csv", "tsv",
                    "log", "js", "ts", "jsx", "tsx", "py", "rs", "go", "java", "c", "cpp", "h",
                    "cs", "html", "css", "xml", "yml", "yaml", "toml", "ini", "sql", "sh", "ps1",
                ],
            )
            .add_filter("Images (*.png, *.jpg, *.webp, *.gif)", &["png", "jpg", "jpeg", "webp", "gif"])
            .add_filter("Documents (*.pdf)", &["pdf"])
            .add_filter(
                "Text & Code",
                &[
                    "txt", "md", "json", "csv", "tsv", "log", "js", "ts", "jsx", "tsx", "py",
                    "rs", "go", "java", "c", "cpp", "h", "cs", "html", "css", "xml", "yml",
                    "yaml", "toml", "ini", "sql", "sh", "ps1",
                ],
            )
            .pick_files()
    })
    .await
    .map_err(|e| format!("Dialog error: {}", e))?;

    let files = match picked {
        Some(f) if !f.is_empty() => f,
        _ => return Ok(AttachmentBatchResult { attachments: Vec::new(), error: None }),
    };

    let mut added_metas = Vec::new();
    let mut first_error: Option<String> = None;

    with_attachment_store(|store| {
        let mut staged_bytes: usize = current_ids
            .iter()
            .filter_map(|id| store.get(id))
            .map(|a| a.bytes.len())
            .sum();
        let mut count = current_ids.len();

        for path in files {
            if count >= 4 {
                if first_error.is_none() {
                    first_error = Some("Maximum 4 attachments allowed.".to_string());
                }
                break;
            }

            match process_file_path(&path, count, staged_bytes) {
                Ok(att) => {
                    count += 1;
                    staged_bytes += att.bytes.len();
                    added_metas.push(att.meta.clone());
                    store.insert(att.meta.id.clone(), att);
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }
    });

    Ok(AttachmentBatchResult {
        attachments: added_metas,
        error: first_error,
    })
}

#[tauri::command]
async fn paste_from_clipboard(
    app: tauri::AppHandle,
    current_ids: Vec<String>,
) -> Result<AttachmentMeta, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    if current_ids.len() >= 4 {
        return Err("Maximum 4 attachments allowed.".to_string());
    }

    let current_bytes: usize = with_attachment_store(|store| {
        current_ids
            .iter()
            .filter_map(|id| store.get(id))
            .map(|a| a.bytes.len())
            .sum()
    });

    // 1. Try reading image
    if let Ok(img) = app.clipboard().read_image() {
        let rgba = img.rgba();
        let w = img.width();
        let h = img.height();
        if !rgba.is_empty() && w > 0 && h > 0 {
            let png_bytes = rgba_to_png(rgba, w, h)?;
            let size = png_bytes.len();
            if size > 10 * 1024 * 1024 {
                return Err("Pasted image exceeds 10 MB limit.".to_string());
            }
            if current_bytes + size > 15 * 1024 * 1024 {
                return Err("Total attachments exceed 15 MB limit.".to_string());
            }

            let id = format!(
                "att_{}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
                ATTACHMENT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );

            let meta = AttachmentMeta {
                id: id.clone(),
                name: "Pasted image.png".to_string(),
                mime: "image/png".to_string(),
                size,
                kind: "image".to_string(),
                has_secret: false,
                preview: None,
            };

            with_attachment_store(|store| {
                store.insert(
                    id,
                    StoredAttachment {
                        meta: meta.clone(),
                        bytes: png_bytes,
                        text_content: None,
                    },
                );
            });

            return Ok(meta);
        }
    }

    // 2. Try reading text
    if let Ok(text) = app.clipboard().read_text() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let bytes = text.as_bytes().to_vec();
            let size = bytes.len();
            if size > 200 * 1024 {
                return Err("Clipboard text exceeds 200 KB limit.".to_string());
            }
            if current_bytes + size > 15 * 1024 * 1024 {
                return Err("Total attachments exceed 15 MB limit.".to_string());
            }

            let has_secret = scan_for_secrets(&text);
            let preview_raw: String = text.chars().take(200).collect();
            let preview = mask_secrets(&preview_raw);

            let id = format!(
                "att_{}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
                ATTACHMENT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );

            let meta = AttachmentMeta {
                id: id.clone(),
                name: "Clipboard text".to_string(),
                mime: "text/plain".to_string(),
                size: text.chars().count(),
                kind: "text".to_string(),
                has_secret,
                preview: Some(preview),
            };

            with_attachment_store(|store| {
                store.insert(
                    id,
                    StoredAttachment {
                        meta: meta.clone(),
                        bytes,
                        text_content: Some(text),
                    },
                );
            });

            return Ok(meta);
        }
    }

    Err("The clipboard is empty.".to_string())
}

#[tauri::command]
fn remove_attachment(id: String) -> Result<(), String> {
    with_attachment_store(|store| {
        store.remove(&id);
    });
    Ok(())
}

#[tauri::command]
async fn attach_from_vault(
    app: tauri::AppHandle,
    id: String,
    current_ids: Vec<String>,
) -> Result<AttachmentMeta, String> {
    if current_ids.len() >= 4 {
        return Err("Maximum 4 attachments allowed.".to_string());
    }

    let current_bytes: usize = with_attachment_store(|store| {
        current_ids
            .iter()
            .filter_map(|id| store.get(id))
            .map(|a| a.bytes.len())
            .sum()
    });

    let path = dropvault::resolve_vault_path_for_id(&app, &id)?;

    let att = process_file_path(&path, current_ids.len(), current_bytes)?;
    let meta = att.meta.clone();

    with_attachment_store(|store| {
        store.insert(meta.id.clone(), att);
    });

    Ok(meta)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenLinkResponse {
    pub status: String, // "opened", "needs_confirm", "rejected"
    pub host: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkValidationResult {
    Valid(String),
    NeedsConfirm(String),
    Rejected,
}

pub fn validate_link(url_str: &str, confirmed: bool) -> LinkValidationResult {
    let trimmed = url_str.trim();
    if trimmed.len() > 2048 {
        return LinkValidationResult::Rejected;
    }

    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return LinkValidationResult::Rejected;
    }

    let parsed = match url::Url::parse(trimmed) {
        Ok(u) => u,
        Err(_) => return LinkValidationResult::Rejected,
    };

    let scheme = parsed.scheme().to_lowercase();
    if scheme != "http" && scheme != "https" {
        return LinkValidationResult::Rejected;
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return LinkValidationResult::Rejected;
    }

    let host = match parsed.host() {
        Some(h) => h,
        None => return LinkValidationResult::Rejected,
    };

    let domain = match host {
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => {
            return LinkValidationResult::Rejected;
        }
        url::Host::Domain(d) => d,
    };

    let d_lower = domain.to_lowercase();
    if !d_lower.contains('.')
        || d_lower == "localhost"
        || d_lower.ends_with(".localhost")
        || d_lower.ends_with(".local")
        || d_lower.ends_with(".internal")
        || d_lower.ends_with(".lan")
    {
        return LinkValidationResult::Rejected;
    }

    let is_punycode_or_non_ascii = d_lower.starts_with("xn--")
        || d_lower.contains(".xn--")
        || !domain.is_ascii();

    if is_punycode_or_non_ascii && !confirmed {
        return LinkValidationResult::NeedsConfirm(domain.to_string());
    }

    LinkValidationResult::Valid(parsed.to_string())
}

pub fn process_open_link(url_str: &str, confirmed: bool) -> OpenLinkResponse {
    match validate_link(url_str, confirmed) {
        LinkValidationResult::Rejected => OpenLinkResponse {
            status: "rejected".into(),
            host: None,
            message: Some("This link can't be opened.".into()),
        },
        LinkValidationResult::NeedsConfirm(host) => OpenLinkResponse {
            status: "needs_confirm".into(),
            host: Some(host),
            message: None,
        },
        LinkValidationResult::Valid(normalized) => {
            match tauri_plugin_opener::open_url(&normalized, None::<&str>) {
                Ok(_) => OpenLinkResponse {
                    status: "opened".into(),
                    host: None,
                    message: None,
                },
                Err(_) => OpenLinkResponse {
                    status: "rejected".into(),
                    host: None,
                    message: Some("This link can't be opened.".into()),
                },
            }
        }
    }
}

#[tauri::command]
async fn open_link(url: String, confirmed: Option<bool>) -> Result<OpenLinkResponse, String> {
    Ok(process_open_link(&url, confirmed.unwrap_or(false)))
}

#[tauri::command]
async fn chat_stream(
    app: tauri::AppHandle,
    request_id: String,
    message: String,
    attachment_ids: Vec<String>,
) -> Result<(), String> {
    let (history_snapshot, old_active) = with_chat_manager(|mgr| {
        let old = mgr.active.take();
        (mgr.history.clone(), old)
    });

    if let Some(old) = old_active {
        old.abort_handle.abort();
        let partial = {
            let lock = old.accumulated_text.lock().unwrap_or_else(|p| p.into_inner());
            lock.clone()
        };
        let trimmed = partial.trim().to_string();
        if !trimmed.is_empty() {
            with_chat_manager(|mgr| {
                mgr.history.push(ChatMessage {
                    role: "user".to_string(),
                    text: Some(old.user_message),
                    content: None,
                    parts: Some(old.user_parts),
                });
                mgr.history.push(ChatMessage {
                    role: "model".to_string(),
                    text: Some(trimmed),
                    content: None,
                    parts: None,
                });
                while mgr.history.len() > 10 {
                    mgr.history.drain(0..1);
                }
            });
        }
    }

    let settings = get_settings(app.clone());
    let api_key = get_api_key_from_store()
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| settings.api_key.filter(|k| !k.trim().is_empty()))
        .or_else(|| std::env::var("GEMINI_API_KEY").ok().filter(|k| !k.trim().is_empty()));

    let api_key = match api_key {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => {
            let _ = app.emit(
                "chat-error",
                serde_json::json!({
                    "requestId": &request_id,
                    "message": "NO_API_KEY"
                }),
            );
            return Err("NO_API_KEY".into());
        }
    };

    let user_msg = if message.trim().is_empty() && !attachment_ids.is_empty() {
        "Look at the attached content, tell me what it is and the key points.".to_string()
    } else {
        message.trim().to_string()
    };

    if user_msg.is_empty() && attachment_ids.is_empty() {
        let _ = app.emit(
            "chat-error",
            serde_json::json!({
                "requestId": &request_id,
                "message": "Cannot send an empty message."
            }),
        );
        return Err("Cannot send an empty message.".into());
    }

    let mut user_parts: Vec<serde_json::Value> = Vec::new();
    user_parts.push(serde_json::json!({ "text": &user_msg }));

    let mut att_sizes: Vec<usize> = Vec::new();
    let mut att_mimes: Vec<String> = Vec::new();

    with_attachment_store(|store| {
        for id in &attachment_ids {
            if let Some(att) = store.get(id) {
                att_sizes.push(att.bytes.len());
                att_mimes.push(att.meta.mime.clone());

                if att.meta.kind == "text" {
                    let content = att.text_content.as_deref().unwrap_or("");
                    user_parts.push(serde_json::json!({
                        "text": format!("File: {}\n```\n{}\n```", att.meta.name, content)
                    }));
                } else {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&att.bytes);
                    user_parts.push(serde_json::json!({
                        "inline_data": {
                            "mime_type": att.meta.mime,
                            "data": b64
                        },
                        "_name": att.meta.name,
                        "_size": att.bytes.len()
                    }));
                }
            }
        }
    });

    #[cfg(debug_assertions)]
    {
        if !att_sizes.is_empty() {
            println!(
                "[Chat Debug] attachments sent count: {}, sizes: {:?}, mime_types: {:?}",
                att_sizes.len(),
                att_sizes,
                att_mimes
            );
        }
    }

    let raw_model = settings
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "gemini-3.5-flash".into());

    let mut model_clean = raw_model.trim();
    if let Some(stripped) = model_clean.strip_prefix("models/") {
        model_clean = stripped.trim();
    }
    if model_clean.is_empty() || model_clean == "gemini-flash-latest" {
        model_clean = "gemini-3.5-flash";
    }

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse",
        model_clean
    );

    let content_turns = build_validated_content_turns(&history_snapshot, user_parts.clone());

    let mut body_map = serde_json::Map::new();

    let personality = settings
        .personality
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "You are a cute, friendly mascot".into());

    let trimmed_personality = personality.trim();
    const HIDDEN_LINK_INSTRUCTION: &str = "When the user asks for a video, song, article, map or website, give a clickable Markdown link. Never invent a direct link to a specific video, song or page unless you are certain it exists. For videos use a search link such as [Search on YouTube](https://www.youtube.com/results?search_query=URL_ENCODED_QUERY), and similarly for Google (https://www.google.com/search?q=...), Google Maps (https://www.google.com/maps/search/?api=1&query=...), Spotify (https://open.spotify.com/search/...) and Wikipedia (https://en.wikipedia.org/w/index.php?search=...). Give a direct link only to well-known official pages when you are certain. Always URL-encode the query.";

    let combined_instruction = if trimmed_personality.is_empty() {
        HIDDEN_LINK_INSTRUCTION.to_string()
    } else {
        format!("{}\n\n{}", trimmed_personality, HIDDEN_LINK_INSTRUCTION)
    };

    body_map.insert(
        "systemInstruction".to_string(),
        serde_json::json!({
            "parts": [{ "text": combined_instruction }]
        }),
    );

    body_map.insert("contents".to_string(), serde_json::Value::Array(content_turns));

    let request_body = serde_json::Value::Object(body_map);

    let accumulated_text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let accumulated_clone = accumulated_text.clone();
    let req_id_clone = request_id.clone();
    let user_msg_clone = user_msg.clone();
    let user_parts_clone = user_parts.clone();
    let app_handle = app.clone();
    let had_attachments = !att_sizes.is_empty();

    let task_handle = tokio::spawn(async move {
        use futures_util::StreamExt;

        let start_time = std::time::Instant::now();
        let mut time_to_first_chunk: Option<std::time::Duration> = None;
        let mut chunk_count: usize = 0;
        let mut first_chunk_received = false;

        let client = match reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = app_handle.emit(
                    "chat-error",
                    serde_json::json!({
                        "requestId": &req_id_clone,
                        "message": format!("Client error: {}", e)
                    }),
                );
                with_chat_manager(|mgr| {
                    if let Some(ref a) = mgr.active {
                        if a.request_id == req_id_clone {
                            mgr.active = None;
                        }
                    }
                });
                return;
            }
        };

        const RETRY_DELAYS: [u64; 2] = [1, 2];
        let max_attempts = 2; // 3 attempts total

        for attempt in 0..=max_attempts {
            if attempt > 0 {
                let delay_secs = RETRY_DELAYS[(attempt - 1).min(RETRY_DELAYS.len() - 1)];
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            }

            let send_res = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("x-goog-api-key", api_key.trim())
                .json(&request_body)
                .send()
                .await;

            let res = match send_res {
                Ok(r) => r,
                Err(e) => {
                    if attempt < max_attempts && (e.is_timeout() || e.is_connect()) {
                        continue;
                    }
                    let err_msg = if e.is_timeout() {
                        "Request timed out. Please try again.".to_string()
                    } else {
                        "Network error. Please check your connection.".to_string()
                    };

                    let _ = app_handle.emit(
                        "chat-error",
                        serde_json::json!({
                            "requestId": &req_id_clone,
                            "message": err_msg
                        }),
                    );
                    with_chat_manager(|mgr| {
                        if let Some(ref a) = mgr.active {
                            if a.request_id == req_id_clone {
                                mgr.active = None;
                            }
                        }
                    });
                    return;
                }
            };

            let status = res.status();
            let status_code = status.as_u16();

            if !status.is_success() {
                let err_body = res.text().await.unwrap_or_default();

                if status_code == 400 && had_attachments {
                    let body_lower = err_body.to_lowercase();
                    if body_lower.contains("content")
                        || body_lower.contains("size")
                        || body_lower.contains("inline")
                        || body_lower.contains("payload")
                        || body_lower.contains("blob")
                        || body_lower.contains("attachment")
                        || body_lower.contains("image")
                        || body_lower.contains("document")
                        || body_lower.contains("bytes")
                    {
                        let _ = app_handle.emit(
                            "chat-error",
                            serde_json::json!({
                                "requestId": &req_id_clone,
                                "message": "Gemini could not read this attachment. Try a smaller or different file.",
                                "isAttachmentError": true
                            }),
                        );
                        with_chat_manager(|mgr| {
                            if let Some(ref a) = mgr.active {
                                if a.request_id == req_id_clone {
                                    mgr.active = None;
                                }
                            }
                        });
                        return;
                    }
                }

                let (error_status, error_message) = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&err_body) {
                    let err_obj = parsed.get("error");
                    let st = err_obj
                        .and_then(|e| e.get("status"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let msg = err_obj
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(|m| m.trim().to_string())
                        .filter(|m| !m.is_empty());
                    (st, msg)
                } else {
                    (None, None)
                };

                if (status_code == 503 || status_code == 500 || status_code == 429) && attempt < max_attempts {
                    continue;
                }

                let formatted_error = if let Some(ref msg) = error_message {
                    let cleaned_msg = clean_and_truncate_text(msg, &api_key, 200);
                    if let Some(ref st) = error_status {
                        format!("Gemini error {} ({}): {}", status_code, st, cleaned_msg)
                    } else {
                        format!("Gemini error {}: {}", status_code, cleaned_msg)
                    }
                } else {
                    let trimmed_body = err_body.trim();
                    if !trimmed_body.is_empty() && !trimmed_body.starts_with('{') && !trimmed_body.starts_with('<') {
                        let cleaned_body = clean_and_truncate_text(trimmed_body, &api_key, 200);
                        if let Some(ref st) = error_status {
                            format!("Gemini error {} ({}): {}", status_code, st, cleaned_body)
                        } else {
                            format!("Gemini error {}: {}", status_code, cleaned_body)
                        }
                    } else if let Some(ref st) = error_status {
                        format!("Gemini error {} ({}).", status_code, st)
                    } else {
                        format!("Gemini error {}.", status_code)
                    }
                };

                let _ = app_handle.emit(
                    "chat-error",
                    serde_json::json!({
                        "requestId": &req_id_clone,
                        "message": formatted_error
                    }),
                );
                with_chat_manager(|mgr| {
                    if let Some(ref a) = mgr.active {
                        if a.request_id == req_id_clone {
                            mgr.active = None;
                        }
                    }
                });
                return;
            }

            // Stream response
            let mut byte_stream = res.bytes_stream();
            let mut sse_buffer = String::new();
            let mut last_finish_reason: Option<String> = None;

            loop {
                let next_item = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    byte_stream.next(),
                )
                .await;

                match next_item {
                    Err(_elapsed) => {
                        if !first_chunk_received && attempt < max_attempts {
                            break;
                        }
                        let _ = app_handle.emit(
                            "chat-error",
                            serde_json::json!({
                                "requestId": &req_id_clone,
                                "message": "Request timed out. Please try again."
                            }),
                        );
                        with_chat_manager(|mgr| {
                            if let Some(ref a) = mgr.active {
                                if a.request_id == req_id_clone {
                                    mgr.active = None;
                                }
                            }
                        });
                        return;
                    }
                    Ok(None) => {
                        if !sse_buffer.trim().is_empty() {
                            let line = sse_buffer.trim().to_string();
                            if let Some(data_str) = line.strip_prefix("data:") {
                                let trimmed_data = data_str.trim();
                                if !trimmed_data.is_empty() && trimmed_data != "[DONE]" {
                                    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed_data) {
                                        if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array()) {
                                            if let Some(candidate) = candidates.first() {
                                                if let Some(fr) = candidate.get("finishReason").and_then(|f| f.as_str()) {
                                                    last_finish_reason = Some(fr.to_string());
                                                }
                                                if let Some(parts) = candidate
                                                    .get("content")
                                                    .and_then(|c| c.get("parts"))
                                                    .and_then(|p| p.as_array())
                                                {
                                                    for part in parts {
                                                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                                            if !t.is_empty() {
                                                                if !first_chunk_received {
                                                                    first_chunk_received = true;
                                                                    time_to_first_chunk = Some(start_time.elapsed());
                                                                }
                                                                chunk_count += 1;
                                                                accumulated_clone
                                                                    .lock()
                                                                    .unwrap_or_else(|p| p.into_inner())
                                                                    .push_str(t);
                                                                let _ = app_handle.emit(
                                                                    "chat-chunk",
                                                                    serde_json::json!({
                                                                        "requestId": &req_id_clone,
                                                                        "text": t
                                                                    }),
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let total_text = {
                            let lock = accumulated_clone.lock().unwrap_or_else(|p| p.into_inner());
                            lock.clone()
                        };

                        if total_text.trim().is_empty() {
                            if attempt < max_attempts {
                                break;
                            }
                            let _ = app_handle.emit(
                                "chat-error",
                                serde_json::json!({
                                    "requestId": &req_id_clone,
                                    "message": "Received an empty response from Gemini."
                                }),
                            );
                            with_chat_manager(|mgr| {
                                if let Some(ref a) = mgr.active {
                                    if a.request_id == req_id_clone {
                                        mgr.active = None;
                                    }
                                }
                            });
                            return;
                        }

                        let finish_reason = last_finish_reason.unwrap_or_else(|| "STOP".into());
                        let _ = app_handle.emit(
                            "chat-done",
                            serde_json::json!({
                                "requestId": &req_id_clone,
                                "finishReason": finish_reason
                            }),
                        );

                        with_chat_manager(|mgr| {
                            if let Some(ref a) = mgr.active {
                                if a.request_id == req_id_clone {
                                    mgr.active = None;
                                    mgr.history.push(ChatMessage {
                                        role: "user".to_string(),
                                        text: Some(user_msg_clone.clone()),
                                        content: None,
                                        parts: Some(user_parts_clone),
                                    });
                                    mgr.history.push(ChatMessage {
                                        role: "model".to_string(),
                                        text: Some(total_text.trim().to_string()),
                                        content: None,
                                        parts: None,
                                    });

                                    // Cap total attachment bytes in history at 15 MB
                                    let mut total_att_bytes: usize = 0;
                                    for msg in &mgr.history {
                                        if let Some(ref parts) = msg.parts {
                                            for p in parts {
                                                if let Some(sz) = p.get("_size").and_then(|s| s.as_u64()) {
                                                    total_att_bytes += sz as usize;
                                                }
                                            }
                                        }
                                    }

                                    while total_att_bytes > 15 * 1024 * 1024 {
                                        let mut replaced = false;
                                        for msg in &mut mgr.history {
                                            if let Some(ref mut parts) = msg.parts {
                                                for p in parts.iter_mut() {
                                                    if let Some(sz) = p.get("_size").and_then(|s| s.as_u64()) {
                                                        let name = p
                                                            .get("_name")
                                                            .and_then(|n| n.as_str())
                                                            .unwrap_or("attachment");
                                                        total_att_bytes = total_att_bytes.saturating_sub(sz as usize);
                                                        *p = serde_json::json!({
                                                            "text": format!("[attachment removed: {}]", name)
                                                        });
                                                        replaced = true;
                                                        break;
                                                    }
                                                }
                                            }
                                            if replaced {
                                                break;
                                            }
                                        }
                                        if !replaced {
                                            break;
                                        }
                                    }

                                    while mgr.history.len() > 10 {
                                        mgr.history.drain(0..1);
                                    }
                                }
                            }
                        });

                        #[cfg(debug_assertions)]
                        {
                            let total_time = start_time.elapsed();
                            println!(
                                "[Chat Stream] time_to_first_chunk: {:?}, chunks: {}, total_time: {:?}",
                                time_to_first_chunk.unwrap_or(total_time),
                                chunk_count,
                                total_time
                            );
                        }

                        return;
                    }
                    Ok(Some(Err(e))) => {
                        if !first_chunk_received && attempt < max_attempts {
                            break;
                        }
                        let err_msg = if e.is_timeout() {
                            "Request timed out. Please try again."
                        } else {
                            "Network error. Please check your connection."
                        };
                        let _ = app_handle.emit(
                            "chat-error",
                            serde_json::json!({
                                "requestId": &req_id_clone,
                                "message": err_msg
                            }),
                        );
                        with_chat_manager(|mgr| {
                            if let Some(ref a) = mgr.active {
                                if a.request_id == req_id_clone {
                                    mgr.active = None;
                                }
                            }
                        });
                        return;
                    }
                    Ok(Some(Ok(bytes))) => {
                        let text_chunk = String::from_utf8_lossy(&bytes);
                        sse_buffer.push_str(&text_chunk);

                        while let Some(pos) = sse_buffer.find('\n') {
                            let line = sse_buffer[..pos].trim_end_matches('\r').to_string();
                            sse_buffer.drain(..=pos);

                            let trimmed_line = line.trim();
                            if trimmed_line.is_empty() {
                                continue;
                            }

                            if let Some(data_payload) = trimmed_line.strip_prefix("data:") {
                                let trimmed_data = data_payload.trim();
                                if trimmed_data.is_empty() || trimmed_data == "[DONE]" {
                                    continue;
                                }

                                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed_data) {
                                    if let Some(err_obj) = json_val.get("error") {
                                        let msg = err_obj
                                            .get("message")
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("Gemini stream error");
                                        let clean_msg = clean_and_truncate_text(msg, &api_key, 200);
                                        let _ = app_handle.emit(
                                            "chat-error",
                                            serde_json::json!({
                                                "requestId": &req_id_clone,
                                                "message": format!("Gemini error: {}", clean_msg)
                                            }),
                                        );
                                        with_chat_manager(|mgr| {
                                            if let Some(ref a) = mgr.active {
                                                if a.request_id == req_id_clone {
                                                    mgr.active = None;
                                                }
                                            }
                                        });
                                        return;
                                    }

                                    if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array()) {
                                        if let Some(candidate) = candidates.first() {
                                            if let Some(fr) = candidate.get("finishReason").and_then(|f| f.as_str()) {
                                                last_finish_reason = Some(fr.to_string());
                                            }
                                            if let Some(parts) = candidate
                                                .get("content")
                                                .and_then(|c| c.get("parts"))
                                                .and_then(|p| p.as_array())
                                            {
                                                for part in parts {
                                                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                                        if !t.is_empty() {
                                                            if !first_chunk_received {
                                                                first_chunk_received = true;
                                                                time_to_first_chunk = Some(start_time.elapsed());
                                                            }
                                                            chunk_count += 1;
                                                            accumulated_clone
                                                                .lock()
                                                                .unwrap_or_else(|p| p.into_inner())
                                                                .push_str(t);
                                                            let _ = app_handle.emit(
                                                                "chat-chunk",
                                                                serde_json::json!({
                                                                    "requestId": &req_id_clone,
                                                                    "text": t
                                                                }),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if attempt < max_attempts {
                continue;
            }
        }
    });

    with_chat_manager(|mgr| {
        mgr.active = Some(ActiveChatRequest {
            request_id,
            user_message: user_msg,
            user_parts,
            accumulated_text,
            abort_handle: task_handle.abort_handle(),
        });
    });

    Ok(())
}

#[tauri::command]
async fn test_connection(app: tauri::AppHandle) -> Result<String, String> {
    send_gemini_request_internal(
        app,
        "Say hi in one word".to_string(),
        Vec::new(),
        false,
    )
    .await
}

#[tauri::command]
fn agents_status() -> Result<agent_hooks::AgentsStatusInfo, String> {
    agent_hooks::agents_status_internal()
}

#[tauri::command]
fn agents_install() -> Result<String, String> {
    agent_hooks::agents_install_internal()
}

#[tauri::command]
fn agents_set_enabled(enabled: bool) -> Result<String, String> {
    agent_hooks::agents_set_enabled_internal(enabled)
}

#[tauri::command]
fn agents_reinstall() -> Result<String, String> {
    agent_hooks::agents_reinstall_internal()
}

#[tauri::command]
fn agents_uninstall() -> Result<String, String> {
    agent_hooks::agents_uninstall_internal()
}

#[tauri::command]
fn agents_last_event() -> Result<Option<agent_receiver::AgentLastEventInfo>, String> {
    Ok(agent_receiver::get_last_agent_event())
}

#[tauri::command]
fn resolve_agent_approval(id: String, choice: String) -> Result<(), String> {
    agent_receiver::resolve_pending_approval(&id, choice);
    Ok(())
}

#[tauri::command]
fn set_approvals_failsafe(enabled: bool) -> Result<(), String> {
    agent_hooks::sync_approvals_flag(enabled);
    Ok(())
}


#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AgentFileReadResult {
    pub lines: Vec<String>,
    pub start_line: usize,
    pub is_sensitive: bool,
}

fn is_sensitive_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == ".env" || lower.starts_with(".env.") {
        return true;
    }
    if lower.ends_with(".pem") || lower.ends_with(".key") || lower.ends_with(".pfx") || lower.ends_with(".p12") {
        return true;
    }
    if lower.starts_with("id_rsa") || lower.starts_with("credentials") || lower.starts_with("secrets") {
        return true;
    }
    false
}

fn strip_unc(p: &std::path::Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        std::path::PathBuf::from(stripped)
    } else {
        p.to_path_buf()
    }
}

#[tauri::command]
fn read_agent_file(
    file_path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    workspace_paths: Vec<String>,
) -> Result<AgentFileReadResult, String> {
    use std::path::Path;

    let path = Path::new(&file_path);
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();

    if is_sensitive_filename(file_name) {
        return Ok(AgentFileReadResult {
            lines: vec!["Hidden (sensitive file)".to_string()],
            start_line: 1,
            is_sensitive: true,
        });
    }

    let canonical_file_raw = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let canonical_file = strip_unc(&canonical_file_raw);

    if !workspace_paths.is_empty() {
        let norm_file = canonical_file.to_string_lossy().to_lowercase().replace('/', "\\");
        let in_workspace = workspace_paths.iter().any(|ws| {
            let ws_path = Path::new(ws);
            if let Ok(c_ws) = std::fs::canonicalize(ws_path) {
                let norm_ws = strip_unc(&c_ws).to_string_lossy().to_lowercase().replace('/', "\\");
                norm_file.starts_with(&norm_ws)
            } else {
                let norm_ws = ws.to_lowercase().replace('/', "\\");
                norm_file.starts_with(&norm_ws)
            }
        });

        if !in_workspace {
            return Err("File path is outside workspace paths".to_string());
        }
    }

    let metadata = std::fs::metadata(&canonical_file).map_err(|e| e.to_string())?;
    if metadata.len() > 65536 {
        return Err("File too large (> 64KB)".to_string());
    }

    let bytes = std::fs::read(&canonical_file).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Err("Binary file skipped".to_string());
    }

    let content = String::from_utf8(bytes).map_err(|_| "Binary/non-UTF8 file skipped".to_string())?;
    let all_lines: Vec<&str> = content.lines().collect();

    let start = start_line.unwrap_or(1).max(1);
    let start_idx = start.saturating_sub(1);
    let default_end = start + 599;
    let end = end_line.unwrap_or(default_end).min(start + 599);
    let end_idx = end.min(all_lines.len());

    let slice = if start_idx < all_lines.len() {
        all_lines[start_idx..end_idx]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    };

    Ok(AgentFileReadResult {
        lines: slice,
        start_line: start,
        is_sensitive: false,
    })
}

#[tauri::command]
fn log_agent_debug(event_name: String, transition: String) {
    #[cfg(debug_assertions)]
    println!("[agent-event] {} | {}", event_name, transition);
    #[cfg(not(debug_assertions))]
    {
        let _ = (event_name, transition);
    }
}

#[tauri::command]
fn is_debug() -> bool {
    cfg!(debug_assertions)
}


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                for _ in 0..15 {
                    if let Some(win) = handle.get_webview_window("main") {
                        let _ = win.unminimize();
                        let _ = win.show();
                        #[cfg(target_os = "windows")]
                        {
                            if let Ok(hwnd) = win.hwnd() {
                                unsafe {
                                    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOW};
                                    let _ = ShowWindow(windows::Win32::Foundation::HWND(hwnd.0 as _), SW_SHOW);
                                }
                            }
                        }
                    }
                    if handle.get_webview_window("settings").is_some() {
                        let _ = open_dude_settings_impl(handle.clone(), None);
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let _ = open_dude_settings_impl(handle, None);
            });
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            open_dude_settings,
            toggle_mascot_window,
            get_os_diagnostic_info,
            quit_app,
            show_context_menu,
            ask_gemini,
            chat_stream,
            chat_cancel,
            chat_clear,
            chat_pop_last_turn,
            pick_attachments,
            paste_from_clipboard,
            remove_attachment,
            attach_from_vault,
            open_link,
            test_connection,
            get_settings,
            save_settings,
            get_available_models,
            save_api_key,
            get_api_key,
            has_api_key,
            delete_api_key,
            get_onboarded,
            set_onboarded,
            get_volume,
            set_volume,
            get_mute,
            set_mute,
            get_brightness,
            set_brightness,
            get_battery,
            get_radio_state,
            set_radio_state,
            lock_pc,
            lock_workstation,
            sleep_pc,
            open_screenclip,
            open_settings_uri,
            get_media_session,
            media_play_pause,
            media_previous,
            media_next,
            media_seek,
            seek_to,
            get_wifi_status,
            toggle_wifi,
            toggle_bluetooth,
            get_airplane_status,
            toggle_airplane,
            get_hotspot_status,
            toggle_hotspot,
            get_brightness_status,
            toggle_volume_mute,
            get_control_center_state,
            get_windows_theme,
            toggle_windows_theme,
            toggle_battery_saver,
            log_frontend_error,
            agents_status,
            agents_install,
            agents_set_enabled,
            agents_reinstall,
            agents_uninstall,
            agents_last_event,
            read_agent_file,
            log_agent_debug,
            is_debug,
            resolve_agent_approval,
            set_approvals_failsafe,
            log_perf_metric,
            save_asset_webp,
            sound::play_sound,
            sound::stop_all_sounds,
            sound::get_sound_debug_info,
            sound::play_startup_sound_if_manual,
            clipboard_history::get_clipboard_state,
            clipboard_history::copy_clipboard_item,
            clipboard_history::delete_clipboard_item,
            clipboard_history::clear_clipboard_history,
            clipboard_history::set_clipboard_enabled,
            clipboard_history::toggle_clipboard_debug_preview,
            audio_visualizer::start_audio_visualizer,
            audio_visualizer::stop_audio_visualizer,
            is_lbutton_down,
            dropvault::vault_status,
            dropvault::create_vault,
            dropvault::vault_recent,
            dropvault::vault_open,
            dropvault::vault_show,
            dropvault::open_vault_folder,
            dropvault::start_vault_watcher,
            dropvault::stop_vault_watcher,
            dropvault::vault_stats,
            dropvault::vault_pick_location,
            dropvault::vault_apply_location,
            dropvault::vault_cancel_location,
            dropvault::vault_cancel_move,
            dropvault::vault_open_old_folder,
            dropvault::vault_clean_preview,
            dropvault::vault_clean_apply,
            dropvault::vault_clean_cancel,
            dropvault::vault_clean_discard,
            dropvault::seed_vault_test_files
        ])
        .setup(|app| {
            // Procedurally generate sound buffers in memory at startup
            let _ = sound::get_sound_engine();
            clipboard_history::init(&app.handle());

            let handle = app.handle().clone();
            handle.listen("behavior-changed", move |event| {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                    if val.get("soundEnabled") == Some(&serde_json::Value::Bool(false)) {
                        sound::stop_all_sounds_internal();
                    }
                }
            });

            init_brightness_worker(app.handle().clone());
            init_now_playing_worker(app.handle().clone());

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                agent_receiver::start_agent_pipe_server(handle).await;
            });

            // Sync approvals fail-safe flag on startup
            let approve_commands = app
                .store(STORE_FILENAME)
                .ok()
                .and_then(|s| {
                    s.get("agents.approveCommands")
                        .or_else(|| s.get("agents").and_then(|a| a.get("approveCommands").cloned()))
                })
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            agent_hooks::sync_approvals_flag(approve_commands);

            tray::setup_tray(app)?;

            let is_autostart = std::env::args().any(|arg| arg == "--autostart");
            let has_key = match get_api_key_from_store() {
                Ok(Some(k)) => !k.trim().is_empty(),
                _ => false,
            };
            let is_onboarded = app
                .store(STORE_FILENAME)
                .ok()
                .and_then(|s| s.get("onboarded"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if !is_autostart {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let view = if !has_key && !is_onboarded {
                        Some("welcome".to_string())
                    } else {
                        None
                    };
                    let _ = open_dude_settings_impl(app_handle, view);
                });
            } else if !has_key && !is_onboarded {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let _ = open_dude_settings_impl(app_handle, Some("welcome".to_string()));
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    if IS_QUITTING.load(Ordering::SeqCst) {
                        return;
                    }
                    // Prevent app from quitting when settings, main, or menu windows are closed
                    api.prevent_close();
                    let _ = window.hide();
                }
                tauri::WindowEvent::DragDrop(drag_event) => {
                    if window.label() != "main" || !window.is_visible().unwrap_or(false) {
                        return;
                    }
                    match drag_event {
                        tauri::DragDropEvent::Enter { paths, .. } => {
                            if paths.is_empty() {
                                return;
                            }
                            if IS_PROCESSING_DROP.load(Ordering::SeqCst) {
                                return;
                            }
                            IS_FILE_DRAG_ACTIVE.store(true, Ordering::SeqCst);
                            DRAG_FILE_COUNT.store(paths.len(), Ordering::SeqCst);
                            let _ = window.emit(
                                "file-drop",
                                serde_json::json!({
                                    "state": "enter",
                                    "count": paths.len(),
                                    "accepted": 0,
                                    "rejected": []
                                }),
                            );
                        }
                        tauri::DragDropEvent::Over { .. } => {
                            if IS_FILE_DRAG_ACTIVE.load(Ordering::SeqCst) {
                                let _ = window.emit(
                                    "file-drop",
                                    serde_json::json!({
                                        "state": "over",
                                        "count": DRAG_FILE_COUNT.load(Ordering::SeqCst),
                                        "accepted": 0,
                                        "rejected": []
                                    }),
                                );
                            }
                        }
                        tauri::DragDropEvent::Leave => {
                            if IS_FILE_DRAG_ACTIVE.swap(false, Ordering::SeqCst) {
                                let _ = window.emit(
                                    "file-drop",
                                    serde_json::json!({
                                        "state": "leave",
                                        "count": 0,
                                        "accepted": 0,
                                        "rejected": []
                                    }),
                                );
                            }
                        }
                        tauri::DragDropEvent::Drop { paths, .. } => {
                            if paths.is_empty() {
                                if IS_FILE_DRAG_ACTIVE.swap(false, Ordering::SeqCst) {
                                    let _ = window.emit(
                                        "file-drop",
                                        serde_json::json!({
                                            "state": "leave",
                                            "count": 0,
                                            "accepted": 0,
                                            "rejected": []
                                        }),
                                    );
                                }
                                return;
                            }
                            if IS_PROCESSING_DROP.swap(true, Ordering::SeqCst) {
                                return;
                            }
                            IS_FILE_DRAG_ACTIVE.store(false, Ordering::SeqCst);

                            let _ = window.emit(
                                "file-drop",
                                serde_json::json!({
                                    "state": "dropped",
                                    "count": paths.len(),
                                    "accepted": 0,
                                    "rejected": []
                                }),
                            );

                            let app_clone = window.app_handle().clone();
                            let win_clone = window.clone();
                            let paths_clone = paths.clone();
                            tauri::async_runtime::spawn_blocking(move || {
                                let res = dropvault::save_to_vault_internal(&app_clone, &paths_clone);
                                let is_error = res.saved == 0 && !res.rejected.is_empty();
                                let _ = win_clone.emit(
                                    "vault-save",
                                    serde_json::json!({
                                        "state": if is_error { "error" } else { "done" },
                                        "saved": res.saved,
                                        "rejected": res.rejected
                                    }),
                                );
                                let _ = win_clone.emit(
                                    "file-drop",
                                    serde_json::json!({
                                        "state": "result",
                                        "count": res.count,
                                        "accepted": res.saved,
                                        "rejected": res.rejected
                                    }),
                                );
                                IS_PROCESSING_DROP.store(false, Ordering::SeqCst);
                            });
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        match event {
            tauri::RunEvent::ExitRequested { .. } => {
                if !IS_QUITTING.load(Ordering::SeqCst) {
                    IS_QUITTING.store(true, Ordering::SeqCst);
                    tray::remove_tray(app_handle);
                    stop_background_workers();
                    for (_, win) in app_handle.webview_windows() {
                        let _ = win.destroy();
                    }
                }
            }
            tauri::RunEvent::Exit => {
                tray::remove_tray(app_handle);
            }
            _ => {}
        }
    });

    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_quitting_flow() {
        let prev = IS_QUITTING.swap(false, Ordering::SeqCst);
        assert!(!is_quitting());
        stop_background_workers();
        assert!(is_quitting());
        IS_QUITTING.store(prev, Ordering::SeqCst);
    }

    #[test]
    fn test_keyring_presence() {
        let key_opt = get_api_key_from_store().ok().flatten();
        println!("Keyring key present: {}", key_opt.is_some());
    }

    #[tokio::test]
    #[ignore]
    async fn test_live_stream_call() {
        let key = match get_api_key_from_store().ok().flatten() {
            Some(k) => k,
            None => {
                println!("No keyring key available, skipping live test");
                return;
            }
        };

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        let available_models = fetch_models_from_google(&key).await.unwrap_or_else(|_| vec!["gemini-1.5-flash".to_string()]);
        println!("Available models: {:?}", available_models);
        let model = available_models.first().cloned().unwrap_or_else(|| "gemini-1.5-flash".to_string());
        println!("Testing streaming with model: {}", model);

        let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse", model);
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": "Write a short poem about stars (5 lines)" }]
            }]
        });

        use futures_util::StreamExt;
        let start = std::time::Instant::now();
        let mut time_to_first_chunk = None;
        let mut chunk_count = 0;

        let res = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", key.trim())
            .json(&body)
            .send()
            .await
            .unwrap();

        assert!(res.status().is_success(), "HTTP status: {}", res.status());

        let mut stream = res.bytes_stream();
        let mut buffer = String::new();

        while let Some(item) = stream.next().await {
            let bytes = item.unwrap();
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                if let Some(payload) = line.strip_prefix("data:") {
                    let trimmed = payload.trim();
                    if trimmed.is_empty() || trimmed == "[DONE]" {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        if let Some(cands) = v.get("candidates").and_then(|c| c.as_array()) {
                            if let Some(c) = cands.first() {
                                if let Some(parts) = c.get("content").and_then(|co| co.get("parts")).and_then(|p| p.as_array()) {
                                    for p in parts {
                                        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                            if !t.is_empty() {
                                                if time_to_first_chunk.is_none() {
                                                    time_to_first_chunk = Some(start.elapsed());
                                                }
                                                chunk_count += 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let total = start.elapsed();
        println!("\n>>> LIVE STREAM RESULT <<<");
        println!("Time to first chunk: {:?}", time_to_first_chunk);
        println!("Chunk count: {}", chunk_count);
        println!("Total time: {:?}", total);
        assert!(chunk_count > 0);
    }

    #[test]
    fn test_chat_manager_cancel_and_partial_history() {
        chat_clear().unwrap();

        // 1. When canceled with partial reply, history records user message + partial reply
        let dummy_task = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });

        let accum = std::sync::Arc::new(std::sync::Mutex::new(String::from("Hello, I am part")));
        with_chat_manager(|mgr| {
            mgr.active = Some(ActiveChatRequest {
                request_id: "req_test_1".to_string(),
                user_message: "Tell me a joke".to_string(),
                user_parts: Vec::new(),
                accumulated_text: accum,
                abort_handle: dummy_task.abort_handle(),
            });
        });

        assert!(chat_cancel("req_test_1".to_string()).is_ok());

        with_chat_manager(|mgr| {
            assert!(mgr.active.is_none());
            assert_eq!(mgr.history.len(), 2);
            assert_eq!(mgr.history[0].role, "user");
            assert_eq!(mgr.history[0].text.as_deref(), Some("Tell me a joke"));
            assert_eq!(mgr.history[1].role, "model");
            assert_eq!(mgr.history[1].text.as_deref(), Some("Hello, I am part"));
        });

        // 2. When canceled before ANY text arrives, history appends nothing
        let dummy_task_2 = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });

        let empty_accum = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        with_chat_manager(|mgr| {
            mgr.active = Some(ActiveChatRequest {
                request_id: "req_test_2".to_string(),
                user_message: "Another message".to_string(),
                user_parts: Vec::new(),
                accumulated_text: empty_accum,
                abort_handle: dummy_task_2.abort_handle(),
            });
        });

        assert!(chat_cancel("req_test_2".to_string()).is_ok());

        with_chat_manager(|mgr| {
            assert!(mgr.active.is_none());
            // Length remains 2 because no text arrived for req_test_2
            assert_eq!(mgr.history.len(), 2);
        });

        // 3. chat_clear clears history
        assert!(chat_clear().is_ok());
        with_chat_manager(|mgr| {
            assert_eq!(mgr.history.len(), 0);
            assert!(mgr.active.is_none());
        });
    }

    #[test]
    fn test_chat_turn_validation_and_trimming() {
        let mut history = Vec::new();
        for i in 1..=8 {
            history.push(ChatMessage {
                role: "user".to_string(),
                text: Some(format!("user {}", i)),
                content: None,
                parts: None,
            });
            history.push(ChatMessage {
                role: "model".to_string(),
                text: Some(format!("model {}", i)),
                content: None,
                parts: None,
            });
        }

        let turns = build_validated_turns(&history, "final prompt");
        // Must have at most 10 turns
        assert!(turns.len() <= 10);
        // Must start with user and end with final prompt
        assert_eq!(turns.first().unwrap().0, "user");
        assert_eq!(turns.last().unwrap().0, "user");
        assert_eq!(turns.last().unwrap().1, "final prompt");

        // Roles must alternate
        for i in 0..(turns.len() - 1) {
            assert_ne!(turns[i].0, turns[i + 1].0);
        }
    }

    #[test]
    fn test_sse_buffering_across_network_chunks() {
        let chunk1 = "data: {\"candidates\": [{\"content\": {\"parts\": [{\"text\": \"He";
        let chunk2 = "llo\"}]}}]}\n\ndata: {\"candidates\": [{\"content\": {\"parts\": [{\"text\": \" world!\"}]}}]}\n";

        let mut sse_buffer = String::new();
        let mut extracted_texts = Vec::new();

        // Push chunk 1 (incomplete JSON)
        sse_buffer.push_str(chunk1);
        while let Some(pos) = sse_buffer.find('\n') {
            let line = sse_buffer[..pos].trim_end_matches('\r').to_string();
            sse_buffer.drain(..=pos);
            if let Some(data_payload) = line.strip_prefix("data:") {
                let trimmed = data_payload.trim();
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array()) {
                        if let Some(cand) = candidates.first() {
                            if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                                for p in parts {
                                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                        extracted_texts.push(t.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Chunk 1 has no newline, so buffer holds incomplete text and no texts extracted yet
        assert_eq!(extracted_texts.len(), 0);
        assert!(!sse_buffer.is_empty());

        // Push chunk 2 (completes chunk 1 and adds chunk 2)
        sse_buffer.push_str(chunk2);
        while let Some(pos) = sse_buffer.find('\n') {
            let line = sse_buffer[..pos].trim_end_matches('\r').to_string();
            sse_buffer.drain(..=pos);
            if let Some(data_payload) = line.strip_prefix("data:") {
                let trimmed = data_payload.trim();
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if let Some(candidates) = json_val.get("candidates").and_then(|c| c.as_array()) {
                        if let Some(cand) = candidates.first() {
                            if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                                for p in parts {
                                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                        extracted_texts.push(t.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        assert_eq!(extracted_texts, vec!["Hello".to_string(), " world!".to_string()]);
    }

    #[test]
    fn test_chat_pop_last_turn() {
        with_chat_manager(|mgr| {
            mgr.history.clear();
            mgr.history.push(ChatMessage {
                role: "user".to_string(),
                text: Some("Question 1".to_string()),
                content: None,
                parts: None,
            });
            mgr.history.push(ChatMessage {
                role: "model".to_string(),
                text: Some("Answer 1".to_string()),
                content: None,
                parts: None,
            });
            mgr.history.push(ChatMessage {
                role: "user".to_string(),
                text: Some("Question 2".to_string()),
                content: None,
                parts: None,
            });
            mgr.history.push(ChatMessage {
                role: "model".to_string(),
                text: Some("Answer 2".to_string()),
                content: None,
                parts: None,
            });
        });

        assert_eq!(chat_pop_last_turn(), Ok(()));

        with_chat_manager(|mgr| {
            assert_eq!(mgr.history.len(), 2);
            assert_eq!(mgr.history[0].role, "user");
            assert_eq!(mgr.history[0].text.as_deref(), Some("Question 1"));
            assert_eq!(mgr.history[1].role, "model");
            assert_eq!(mgr.history[1].text.as_deref(), Some("Answer 1"));
        });
    }

    #[test]
    fn test_sensitive_filenames() {
        assert!(is_sensitive_filename(".env"));
        assert!(is_sensitive_filename(".env.local"));
        assert!(is_sensitive_filename(".env.production"));
        assert!(is_sensitive_filename("cert.pem"));
        assert!(is_sensitive_filename("private.key"));
        assert!(is_sensitive_filename("cert.pfx"));
        assert!(is_sensitive_filename("cert.p12"));
        assert!(is_sensitive_filename("id_rsa"));
        assert!(is_sensitive_filename("id_rsa.pub"));
        assert!(is_sensitive_filename("credentials.json"));
        assert!(is_sensitive_filename("secrets.toml"));

        assert!(!is_sensitive_filename("main.rs"));
        assert!(!is_sensitive_filename("app.py"));
        assert!(!is_sensitive_filename("photo.png"));
        assert!(!is_sensitive_filename("document.pdf"));
    }

    #[test]
    fn test_secret_patterns_scanning() {
        assert!(scan_for_secrets("AIzaSyD-fakeKeyExample12345678901234"));
        assert!(scan_for_secrets("key: 'AIzaFAKEFAKEFAKEFAKEFAKEFAKE1234'"));
        assert!(scan_for_secrets("sk-proj-1234567890abcdef"));
        assert!(scan_for_secrets("ghp_1234567890abcdef1234567890abcdef"));
        assert!(scan_for_secrets("AQ123456789012345678"));
        assert!(scan_for_secrets("Authorization: Bearer 1234567890123456789012345"));
        assert!(scan_for_secrets("-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0B"));

        assert!(!scan_for_secrets("def hello():\n    print('Hello World!')\n"));
        assert!(!scan_for_secrets("Just a regular chat message with no secret tokens."));
    }

    #[test]
    fn test_masking_secrets() {
        let input = "Here is my key: AIzaFAKEFAKEFAKEFAKEFAKEFAKE1234 and done.";
        let masked = mask_secrets(input);
        assert!(!masked.contains("AIzaFAKE"));
        assert!(masked.contains("[SECRET_MASKED]"));
    }

    #[test]
    fn test_rgba_to_png_encoding() {
        let rgba = vec![255u8, 0, 0, 255]; // 1x1 red pixel
        let res = rgba_to_png(&rgba, 1, 1);
        assert!(res.is_ok());
        let png = res.unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn test_attachment_history_capping() {
        let mut parts: Vec<serde_json::Value> = Vec::new();
        // 16 MB dummy inline_data part
        parts.push(serde_json::json!({
            "inline_data": {
                "mime_type": "image/png",
                "data": "dummy"
            },
            "_name": "big_image.png",
            "_size": 16 * 1024 * 1024
        }));

        let msg = ChatMessage {
            role: "user".to_string(),
            text: Some("Look at this image".to_string()),
            content: None,
            parts: Some(parts),
        };

        with_chat_manager(|mgr| {
            mgr.history.clear();
            mgr.history.push(msg);

            let mut total_att_bytes: usize = 0;
            for m in &mgr.history {
                if let Some(ref p_list) = m.parts {
                    for p in p_list {
                        if let Some(sz) = p.get("_size").and_then(|s| s.as_u64()) {
                            total_att_bytes += sz as usize;
                        }
                    }
                }
            }

            while total_att_bytes > 15 * 1024 * 1024 {
                let mut replaced = false;
                for m in &mut mgr.history {
                    if let Some(ref mut p_list) = m.parts {
                        for p in p_list.iter_mut() {
                            if let Some(sz) = p.get("_size").and_then(|s| s.as_u64()) {
                                let name = p.get("_name").and_then(|n| n.as_str()).unwrap_or("attachment");
                                total_att_bytes = total_att_bytes.saturating_sub(sz as usize);
                                *p = serde_json::json!({
                                    "text": format!("[attachment removed: {}]", name)
                                });
                                replaced = true;
                                break;
                            }
                        }
                    }
                    if replaced {
                        break;
                    }
                }
                if !replaced {
                    break;
                }
            }

            let first_turn = &mgr.history[0];
            let first_part = &first_turn.parts.as_ref().unwrap()[0];
            assert_eq!(
                first_part.get("text").and_then(|t| t.as_str()),
                Some("[attachment removed: big_image.png]")
            );
        });
    }

    #[test]
    fn test_process_file_path_scenarios() {
        let temp_dir = std::env::temp_dir().join(format!("dude_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // 1. .env file
        let env_file = temp_dir.join(".env");
        std::fs::write(&env_file, "SECRET=123").unwrap();
        let res = process_file_path(&env_file, 0, 0);
        assert_eq!(res.err(), Some("This file looks sensitive and was not attached.".to_string()));

        // 2. .py file valid
        let py_file = temp_dir.join("script.py");
        std::fs::write(&py_file, "print('hello from python')").unwrap();
        let res = process_file_path(&py_file, 0, 0);
        assert!(res.is_ok());
        let att = res.unwrap();
        assert_eq!(att.meta.kind, "text");
        assert_eq!(att.meta.mime, "text/plain");
        assert!(!att.meta.has_secret);

        // 3. Text file with fake secret key
        let secret_txt = temp_dir.join("config.txt");
        std::fs::write(&secret_txt, "google_api_key = 'AIzaFAKEFAKEFAKEFAKEFAKEFAKE1234'").unwrap();
        let res = process_file_path(&secret_txt, 0, 0);
        assert!(res.is_ok());
        let att = res.unwrap();
        assert!(att.meta.has_secret);

        // 4. Text file with invalid UTF-8
        let invalid_utf8 = temp_dir.join("broken.txt");
        std::fs::write(&invalid_utf8, &[0xFF, 0xFE, 0xFD]).unwrap();
        let res = process_file_path(&invalid_utf8, 0, 0);
        assert_eq!(res.err(), Some("Text file is not valid UTF-8.".to_string()));

        // 5. Text file > 200 KB
        let large_txt = temp_dir.join("large.txt");
        let big_content = vec![b'a'; 201 * 1024];
        std::fs::write(&large_txt, &big_content).unwrap();
        let res = process_file_path(&large_txt, 0, 0);
        assert_eq!(res.err(), Some("Text file exceeds 200 KB limit.".to_string()));

        // 6. Max 4 attachments limit (current_count == 4)
        let res = process_file_path(&py_file, 4, 0);
        assert_eq!(res.err(), Some("Maximum 4 attachments allowed.".to_string()));

        // 7. Total size > 15 MB limit
        let res = process_file_path(&py_file, 1, 15 * 1024 * 1024);
        assert_eq!(res.err(), Some("Total attachments exceed 15 MB limit.".to_string()));

        // 8. Image > 10 MB limit
        let large_img = temp_dir.join("large.png");
        let big_img_data = vec![0u8; 11 * 1024 * 1024];
        std::fs::write(&large_img, &big_img_data).unwrap();
        let res = process_file_path(&large_img, 0, 0);
        assert_eq!(res.err(), Some("File exceeds 10 MB limit.".to_string()));

        // 9. Valid PDF
        let pdf_file = temp_dir.join("doc.pdf");
        std::fs::write(&pdf_file, b"%PDF-1.4 dummy pdf content").unwrap();
        let res = process_file_path(&pdf_file, 0, 0);
        assert!(res.is_ok());
        let att = res.unwrap();
        assert_eq!(att.meta.kind, "pdf");
        assert_eq!(att.meta.mime, "application/pdf");

        // 10. Valid PNG
        let png_file = temp_dir.join("sample.png");
        std::fs::write(&png_file, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let res = process_file_path(&png_file, 0, 0);
        assert!(res.is_ok());
        let att = res.unwrap();
        assert_eq!(att.meta.kind, "image");
        assert_eq!(att.meta.mime, "image/png");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_dropvault_attachment_pipeline() {
        let temp_vault = std::env::temp_dir().join(format!("dude_vault_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        std::fs::create_dir_all(&temp_vault).unwrap();

        // 1. PNG file
        let png = temp_vault.join("picture.png");
        std::fs::write(&png, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let res_png = process_file_path(&png, 0, 0);
        assert!(res_png.is_ok(), "PNG should be accepted");
        let att_png = res_png.unwrap();
        assert_eq!(att_png.meta.kind, "image");

        // 2. PDF file
        let pdf = temp_vault.join("document.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 test content").unwrap();
        let res_pdf = process_file_path(&pdf, 1, 100);
        assert!(res_pdf.is_ok(), "PDF should be accepted");
        let att_pdf = res_pdf.unwrap();
        assert_eq!(att_pdf.meta.kind, "pdf");

        // 3. Small .py file
        let py = temp_vault.join("script.py");
        std::fs::write(&py, "print('testing vault')").unwrap();
        let res_py = process_file_path(&py, 2, 200);
        assert!(res_py.is_ok(), "Python script should be accepted");
        let att_py = res_py.unwrap();
        assert_eq!(att_py.meta.kind, "text");

        // 4. .env file
        let env_file = temp_vault.join(".env");
        std::fs::write(&env_file, "API_KEY=sensitive").unwrap();
        let res_env = process_file_path(&env_file, 3, 300);
        assert_eq!(res_env.err(), Some("This file looks sensitive and was not attached.".to_string()));

        // 5. Fake secret in file
        let secret_file = temp_vault.join("config_secret.txt");
        std::fs::write(&secret_file, "key = 'AIzaFakeSecretKeyHere1234567890'").unwrap();
        let res_sec = process_file_path(&secret_file, 3, 300);
        assert!(res_sec.is_ok());
        assert!(res_sec.unwrap().meta.has_secret, "Secret must be detected");

        // 6. Oversized file (>10 MB)
        let large_file = temp_vault.join("heavy.png");
        let large_bytes = vec![0u8; 11 * 1024 * 1024];
        std::fs::write(&large_file, &large_bytes).unwrap();
        let res_large = process_file_path(&large_file, 3, 300);
        assert_eq!(res_large.err(), Some("File exceeds 10 MB limit.".to_string()));

        // 7. 5th attachment rejection
        let fifth_file = temp_vault.join("fifth.txt");
        std::fs::write(&fifth_file, "fifth").unwrap();
        let res_fifth = process_file_path(&fifth_file, 4, 300);
        assert_eq!(res_fifth.err(), Some("Maximum 4 attachments allowed.".to_string()));

        let _ = std::fs::remove_dir_all(&temp_vault);
    }

    #[test]
    fn test_link_validation_scenarios() {
        // 1. Valid URLs
        assert_eq!(
            validate_link("https://www.youtube.com/results?search_query=lofi+music", false),
            LinkValidationResult::Valid("https://www.youtube.com/results?search_query=lofi+music".to_string())
        );
        assert_eq!(
            validate_link("https://en.wikipedia.org/wiki/Rust_(programming_language)", false),
            LinkValidationResult::Valid("https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string())
        );

        // 2. Localhost, loopback, private IP, IPv6
        assert_eq!(validate_link("http://localhost:3000", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("http://192.168.1.1", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("http://127.0.0.1:8080", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("http://[::1]:8080", false), LinkValidationResult::Rejected);

        // 3. Credentials in URL
        assert_eq!(validate_link("http://user:pass@example.com", false), LinkValidationResult::Rejected);

        // 4. Punycode / IDN / non-ASCII host
        assert_eq!(
            validate_link("https://xn--pple-43d.com", false),
            LinkValidationResult::NeedsConfirm("xn--pple-43d.com".to_string())
        );
        assert_eq!(
            validate_link("https://xn--pple-43d.com", true),
            LinkValidationResult::Valid("https://xn--pple-43d.com/".to_string())
        );

        // 5. 3000-character URL
        let long_url = format!("https://example.com/{}", "a".repeat(3000));
        assert_eq!(validate_link(&long_url, false), LinkValidationResult::Rejected);

        // 6. Schemes other than http/https
        assert_eq!(validate_link("javascript:alert(1)", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("file:///C:/Windows/win.ini", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("data:text/html,test", false), LinkValidationResult::Rejected);

        // 7. Single-label, .local, .internal, .lan
        assert_eq!(validate_link("http://singlelabel", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("http://myhost.local", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("http://test.internal", false), LinkValidationResult::Rejected);
        assert_eq!(validate_link("http://router.lan", false), LinkValidationResult::Rejected);

        // 8. Normal domain
        assert_eq!(
            validate_link("https://evil.example/login", false),
            LinkValidationResult::Valid("https://evil.example/login".to_string())
        );
    }
}

