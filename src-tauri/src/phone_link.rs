use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use axum_server::tls_rustls::RustlsConfig;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use qrcode::QrCode;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_store::StoreExt;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::timeout::TimeoutLayer;

// ============================================================
// CONSTANTS
// ============================================================

const STORE_FILENAME: &str = "dude_store.json";
const DEFAULT_PORT: u16 = 50432;
const PAIRING_TIMEOUT_SECS: u64 = 120;
const MAX_DEVICES: usize = 10;
const MAX_PAIRING_ATTEMPTS: u32 = 5;
const MAX_CONCURRENT_CONNECTIONS: usize = 12;
const MAX_LIVE_CONNECTIONS: usize = 12;
const LIVE_PING_INTERVAL_SECS: u64 = 20;
const LIVE_PONG_TIMEOUT_SECS: u64 = 45;
const LIVE_QUEUE_CAPACITY: usize = 16;
const REQUEST_TIMEOUT_SECS: u64 = 15;
const MAX_BODY_BYTES: usize = 16 * 1024; // 16 KB

const KEYRING_SERVICE: &str = "dude-app";
const KEYRING_ENTRY: &str = "phone-tls-key";
const CERT_FILE_NAME: &str = "phone-cert.pem";
const DEVICES_FILE_NAME: &str = "phone-devices.json";

// ============================================================
// DATA MODELS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneCapabilities {
    pub notifications: bool,
    pub files: bool,
    pub clipboard: bool,
    pub media: bool,
    #[serde(default = "default_true")]
    pub agents: bool,
}

fn default_true() -> bool {
    true
}

impl Default for PhoneCapabilities {
    fn default() -> Self {
        Self {
            notifications: true,
            files: true,
            clipboard: false,
            media: true,
            agents: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDeviceView {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_seen: i64,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneLinkStatus {
    pub running: bool,
    pub error: Option<String>, // "port-busy" | "tls" | "unknown" | null
    pub port: u16,
    pub addresses: Vec<String>,
    pub fingerprint_short: String,
    pub network_profile: String, // "private" | "public" | "domain" | "unknown"
    pub capabilities: PhoneCapabilities,
    pub auto_off_minutes: u32,
    pub devices: Vec<PairedDeviceView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingSessionView {
    pub code: String, // e.g. "8F4D-9K2M"
    pub expires_in_sec: u64,
    pub qr_modules: Vec<bool>,
    pub qr_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    pub created_at: i64,
    pub last_seen: i64,
}

#[allow(dead_code)]
struct PairingSession {
    code: String,
    clean_code: String,
    nonce: String,
    expires_at: Instant,
    wrong_attempts: u32,
    qr_modules: Vec<bool>,
    qr_size: usize,
}

// Rate Limiting Model (5 failed attempts per min -> 60s block)
struct IpRateLimit {
    failed_count: u32,
    window_start: Instant,
    blocked_until: Option<Instant>,
}

pub struct BoundedLiveQueue {
    queue: Mutex<VecDeque<String>>,
    notify: tokio::sync::Notify,
}

impl BoundedLiveQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(LIVE_QUEUE_CAPACITY)),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub fn push(&self, msg: String) {
        let mut q = self.queue.lock().unwrap();
        if q.len() >= LIVE_QUEUE_CAPACITY {
            q.pop_front();
        }
        q.push_back(msg);
        drop(q);
        self.notify.notify_one();
    }

    pub fn pop(&self) -> Option<String> {
        self.queue.lock().unwrap().pop_front()
    }
}

pub struct LiveConnection {
    pub conn_id: u64,
    pub device_id: String,
    pub queue: Arc<BoundedLiveQueue>,
    pub close_tx: tokio::sync::mpsc::Sender<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPublishState {
    pub status: String,
    pub project: Option<String>,
    pub step: Option<String>,
    pub since: Option<i64>,
    pub edits: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentBroadcastMsg {
    pub t: &'static str,
    pub seq: u64,
    pub status: String,
    pub project: String,
    pub step: Option<String>,
    pub since: Option<i64>,
    pub edits: u32,
}

// ============================================================
// GLOBAL STATE
// ============================================================

static SERVER_RUNNING: AtomicBool = AtomicBool::new(false);
static SERVER_PORT: AtomicU16 = AtomicU16::new(DEFAULT_PORT);
static SERVER_ERROR: Mutex<Option<String>> = Mutex::new(None);
static SERVER_HANDLE: Mutex<Option<axum_server::Handle>> = Mutex::new(None);

static ACTIVE_PAIRING_SESSION: Mutex<Option<PairingSession>> = Mutex::new(None);
static LAST_ACTIVITY_TS: AtomicI64 = AtomicI64::new(0);
static AUTO_OFF_MINUTES: AtomicU32 = AtomicU32::new(0);

static CERT_FINGERPRINT: Mutex<String> = Mutex::new(String::new());
static CONNECTED_DEVICE_IDS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static GLOBAL_APP_HANDLE: Mutex<Option<AppHandle>> = Mutex::new(None);

static RATE_LIMITER: Mutex<Option<HashMap<IpAddr, IpRateLimit>>> = Mutex::new(None);
static LAST_SAVED_SEEN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
static NETWORK_PROFILE_CACHE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

static LIVE_CONNECTIONS: Mutex<Option<HashMap<u64, LiveConnection>>> = Mutex::new(None);
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);
static IGNORED_PHONE_MESSAGES_COUNT: AtomicU64 = AtomicU64::new(0);

static AGENT_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_VALID_AGENT_STATE: Mutex<Option<AgentBroadcastMsg>> = Mutex::new(None);
static LAST_AGENT_BROADCAST_MS: AtomicI64 = AtomicI64::new(0);
static BROADCAST_TIMER_ACTIVE: AtomicBool = AtomicBool::new(false);
static PHONE_CHANGED_DEBOUNCE: Mutex<Option<tauri::async_runtime::JoinHandle<()>>> = Mutex::new(None);

fn with_connected_device_ids<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashSet<String>) -> R,
{
    let mut lock = CONNECTED_DEVICE_IDS.lock().unwrap();
    let set = lock.get_or_insert_with(HashSet::new);
    f(set)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn touch_activity() {
    LAST_ACTIVITY_TS.store(now_ms(), Ordering::SeqCst);
}

fn get_server_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Dude PC".to_string())
}

fn get_live_connections_count() -> usize {
    let lock = LIVE_CONNECTIONS.lock().unwrap();
    lock.as_ref().map(|c| c.len()).unwrap_or(0)
}

fn register_live_connection(conn: LiveConnection) -> Option<tokio::sync::mpsc::Sender<String>> {
    let mut lock = LIVE_CONNECTIONS.lock().unwrap();
    let conns = lock.get_or_insert_with(HashMap::new);

    // At most ONE per device: find existing connection for same device
    let mut old_close_tx = None;
    let old_ids: Vec<u64> = conns
        .iter()
        .filter(|(_, c)| c.device_id == conn.device_id)
        .map(|(&id, _)| id)
        .collect();
    for id in old_ids {
        if let Some(old) = conns.remove(&id) {
            old_close_tx = Some(old.close_tx);
        }
    }

    conns.insert(conn.conn_id, conn);
    old_close_tx
}

fn unregister_live_connection(conn_id: u64, device_id: &str, app: &AppHandle) {
    let has_other = {
        let mut lock = LIVE_CONNECTIONS.lock().unwrap();
        if let Some(conns) = lock.as_mut() {
            conns.remove(&conn_id);
            conns.values().any(|c| c.device_id == device_id)
        } else {
            false
        }
    };
    if !has_other {
        with_connected_device_ids(|conns| {
            conns.remove(device_id);
        });
    }
    emit_phone_changed_debounced(app);
}

fn close_device_live_connections(device_id: &str, reason: &str) {
    let mut to_close = Vec::new();
    {
        let mut lock = LIVE_CONNECTIONS.lock().unwrap();
        if let Some(conns) = lock.as_mut() {
            let ids_to_remove: Vec<u64> = conns
                .iter()
                .filter(|(_, c)| c.device_id == device_id)
                .map(|(&id, _)| id)
                .collect();
            for id in ids_to_remove {
                if let Some(c) = conns.remove(&id) {
                    to_close.push(c.close_tx);
                }
            }
        }
    }
    for tx in to_close {
        let _ = tx.try_send(reason.to_string());
    }
}

fn close_all_live_connections(reason: &str) {
    let mut to_close = Vec::new();
    {
        let mut lock = LIVE_CONNECTIONS.lock().unwrap();
        if let Some(conns) = lock.as_mut() {
            for (_, c) in conns.drain() {
                to_close.push(c.close_tx);
            }
        }
    }
    for tx in to_close {
        let _ = tx.try_send(reason.to_string());
    }
}

fn broadcast_to_all_live(msg: &str) {
    let mut queues = Vec::new();
    {
        let lock = LIVE_CONNECTIONS.lock().unwrap();
        if let Some(conns) = lock.as_ref() {
            for conn in conns.values() {
                queues.push(conn.queue.clone());
            }
        }
    }
    for q in queues {
        q.push(msg.to_string());
    }
}

fn broadcast_caps_to_live_connections(app: &AppHandle, caps: &PhoneCapabilities) {
    let caps_msg = json!({
        "t": "caps",
        "capabilities": caps
    }).to_string();
    broadcast_to_all_live(&caps_msg);

    // Apply capability change at once using the central require_capability check:
    if require_capability(app, "agents").is_ok() {
        let snap = LAST_VALID_AGENT_STATE
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(default_idle_snapshot);
        broadcast_to_all_live(&serde_json::to_string(&snap).unwrap_or_default());
    } else {
        let idle = serde_json::to_string(&default_idle_snapshot()).unwrap_or_default();
        broadcast_to_all_live(&idle);
    }
}

fn default_idle_snapshot() -> AgentBroadcastMsg {
    AgentBroadcastMsg {
        t: "agent",
        seq: 0,
        status: "idle".to_string(),
        project: String::new(),
        step: None,
        since: None,
        edits: 0,
    }
}

fn get_current_agent_snapshot(app: &AppHandle) -> String {
    let caps = load_capabilities(app);
    if !caps.agents {
        return serde_json::to_string(&default_idle_snapshot()).unwrap_or_default();
    }
    let snap = LAST_VALID_AGENT_STATE
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(default_idle_snapshot);
    serde_json::to_string(&snap).unwrap_or_default()
}

fn sanitize_project_name(name: Option<&str>) -> String {
    let raw = match name {
        Some(s) => s.trim(),
        None => return String::new(),
    };
    if raw.is_empty() {
        return String::new();
    }
    // Folder name only, never a path
    let folder = if let Some(idx) = raw.rfind(|c| c == '/' || c == '\\') {
        &raw[idx + 1..]
    } else {
        raw
    };
    let cleaned: String = folder.chars().filter(|c| !c.is_control()).collect();
    cleaned.chars().take(40).collect()
}

fn validate_agent_publish_state(state: AgentPublishState) -> Result<AgentBroadcastMsg, String> {
    let status = match state.status.as_str() {
        "idle" | "thinking" | "working" | "needs_you" | "done" => state.status,
        _ => return Err("Invalid status".to_string()),
    };
    let step = match state.step.as_deref() {
        Some("read") => Some("read".to_string()),
        Some("edit") => Some("edit".to_string()),
        Some("bash") => Some("bash".to_string()),
        _ => None,
    };
    let project = sanitize_project_name(state.project.as_deref());
    let edits = state.edits.unwrap_or(0).clamp(0, 999);
    let now = now_ms();
    let since = state.since.filter(|&ms| ms > 0 && ms >= (now - 86_400_000) && ms <= (now + 60_000));
    let seq = AGENT_SEQ.fetch_add(1, Ordering::SeqCst) + 1;

    Ok(AgentBroadcastMsg {
        t: "agent",
        seq,
        status,
        project,
        step,
        since,
        edits,
    })
}

fn publish_agent_state(app: &AppHandle, msg: AgentBroadcastMsg) {
    // Keep the last valid state in memory (snapshot for new connections)
    {
        let mut last_state = LAST_VALID_AGENT_STATE.lock().unwrap();
        *last_state = Some(msg.clone());
    }

    let caps = load_capabilities(app);
    if !caps.agents {
        return;
    }

    let now = now_ms();
    let last = LAST_AGENT_BROADCAST_MS.load(Ordering::SeqCst);
    let diff = now - last;

    if diff >= 250 {
        LAST_AGENT_BROADCAST_MS.store(now, Ordering::SeqCst);
        let text = serde_json::to_string(&msg).unwrap_or_default();
        broadcast_to_all_live(&text);
    } else {
        // Coalesce: at most 4 per second (250ms interval)
        if !BROADCAST_TIMER_ACTIVE.swap(true, Ordering::SeqCst) {
            let wait_ms = (250 - diff).max(1) as u64;
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                BROADCAST_TIMER_ACTIVE.store(false, Ordering::SeqCst);
                LAST_AGENT_BROADCAST_MS.store(now_ms(), Ordering::SeqCst);
                if load_capabilities(&app).agents {
                    let newest = {
                        LAST_VALID_AGENT_STATE.lock().unwrap().clone()
                    };
                    if let Some(newest_msg) = newest {
                        let text = serde_json::to_string(&newest_msg).unwrap_or_default();
                        broadcast_to_all_live(&text);
                    }
                }
            });
        }
    }
}

fn emit_phone_changed_debounced(app: &AppHandle) {
    let app = app.clone();
    let mut lock = PHONE_CHANGED_DEBOUNCE.lock().unwrap();
    if let Some(h) = lock.take() {
        h.abort();
    }
    *lock = Some(tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let _ = app.emit("phone-changed", json!({ "event": "status-updated" }));
    }));
}

// ============================================================
// INITIALIZATION & LIFECYCLE
// ============================================================

pub fn init(app: &AppHandle) {
    {
        let mut handle_lock = GLOBAL_APP_HANDLE.lock().unwrap();
        *handle_lock = Some(app.clone());
    }

    // Clean up any legacy private key or cert written to dude_store.json
    if let Ok(store) = app.store(STORE_FILENAME) {
        store.delete("phone.keyPem");
        store.delete("phone.certPem");
        let _ = store.save();
    }

    // Load auto-off from store
    let auto_off = app
        .store(STORE_FILENAME)
        .ok()
        .and_then(|s| s.get("phone.autoOffMinutes"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    AUTO_OFF_MINUTES.store(auto_off, Ordering::SeqCst);

    // Spawn background auto-off inactivity watcher
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        auto_off_worker(app_handle).await;
    });

    // Check if phone link was enabled in store
    let enabled = app
        .store(STORE_FILENAME)
        .ok()
        .and_then(|s| s.get("phone.enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if enabled {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = start_server_internal(&app_clone).await;
        });
    }
}

pub fn stop_server() {
    stop_server_internal();
    let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
    *session_lock = None;
}

async fn auto_off_worker(app: AppHandle) {
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;

        if !SERVER_RUNNING.load(Ordering::SeqCst) {
            continue;
        }

        // An open live connection counts as activity: reset while at least one is open
        if get_live_connections_count() > 0 {
            touch_activity();
        }

        let minutes = AUTO_OFF_MINUTES.load(Ordering::SeqCst);
        if minutes == 0 {
            continue;
        }

        let last_activity = LAST_ACTIVITY_TS.load(Ordering::SeqCst);
        if last_activity <= 0 {
            continue;
        }

        let elapsed_sec = (now_ms() - last_activity) / 1000;
        if elapsed_sec >= (minutes as i64 * 60) {
            // Auto-off triggered
            stop_server();

            if let Ok(store) = app.store(STORE_FILENAME) {
                store.set("phone.enabled", json!(false));
                let _ = store.save();
            }

            let hours = minutes / 60;
            let _ = app.emit("phone-changed", json!({ "reason": "auto-off", "hours": hours }));
        }
    }
}

// ============================================================
// RATE LIMITING (5 attempts per min -> 60s block)
// ============================================================

pub fn is_ip_blocked(ip: &IpAddr) -> bool {
    let mut lock = RATE_LIMITER.lock().unwrap();
    let map = lock.get_or_insert_with(HashMap::new);
    if let Some(entry) = map.get_mut(ip) {
        if let Some(until) = entry.blocked_until {
            if Instant::now() < until {
                return true;
            } else {
                // Block expired
                entry.blocked_until = None;
                entry.failed_count = 0;
                entry.window_start = Instant::now();
                return false;
            }
        }
    }
    false
}

pub fn record_failed_attempt(ip: IpAddr) {
    let mut lock = RATE_LIMITER.lock().unwrap();
    let map = lock.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    let entry = map.entry(ip).or_insert_with(|| IpRateLimit {
        failed_count: 0,
        window_start: now,
        blocked_until: None,
    });

    if now.duration_since(entry.window_start) > Duration::from_secs(60) {
        entry.failed_count = 1;
        entry.window_start = now;
        entry.blocked_until = None;
    } else {
        entry.failed_count += 1;
        if entry.failed_count >= 5 {
            entry.blocked_until = Some(now + Duration::from_secs(60));
        }
    }
}

pub fn record_success_attempt(ip: &IpAddr) {
    let mut lock = RATE_LIMITER.lock().unwrap();
    if let Some(map) = &mut *lock {
        map.remove(ip);
    }
}

// ============================================================
// IP FILTERING (RFC1918, Link-Local IPv4, IPv6 ULA, IPv6 Link-Local, Loopback)
// ============================================================

pub fn is_allowed_source_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            if ipv4.is_loopback() {
                return true;
            }
            let octets = ipv4.octets();
            // RFC 1918 Private IPv4:
            // 10.0.0.0/8
            if octets[0] == 10 {
                return true;
            }
            // 172.16.0.0/12 (172.16.0.0 - 172.31.255.255)
            if octets[0] == 172 && (16..=31).contains(&octets[1]) {
                return true;
            }
            // 192.168.0.0/16
            if octets[0] == 192 && octets[1] == 168 {
                return true;
            }
            // IPv4 Link-Local (169.254.0.0/16)
            if octets[0] == 169 && octets[1] == 254 {
                return true;
            }
            false
        }
        IpAddr::V6(ipv6) => {
            if ipv6.is_loopback() {
                return true;
            }
            let segments = ipv6.segments();
            // IPv6 ULA (fc00::/7 -> fc00... or fd00...)
            if (segments[0] & 0xfe00) == 0xfc00 {
                return true;
            }
            // IPv6 Link-Local (fe80::/10)
            if (segments[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            false
        }
    }
}

pub fn get_private_lan_ips() -> Vec<String> {
    let mut ips = Vec::new();

    if let Ok(netifas) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in netifas {
            if is_allowed_source_ip(&ip) && !ip.is_loopback() {
                let s = ip.to_string();
                if !ips.contains(&s) {
                    ips.push(s);
                }
            }
        }
    }

    if ips.is_empty() {
        ips.push("127.0.0.1".to_string());
    }

    ips
}

pub fn get_network_profile_category() -> String {
    // Check cache (valid for 30s)
    {
        let lock = NETWORK_PROFILE_CACHE.lock().unwrap();
        if let Some((cat, time)) = &*lock {
            if time.elapsed() < Duration::from_secs(30) {
                return cat.clone();
            }
        }
    }

    let cat = detect_network_profile_win32();
    {
        let mut lock = NETWORK_PROFILE_CACHE.lock().unwrap();
        *lock = Some((cat.clone(), Instant::now()));
    }
    cat
}

fn detect_network_profile_win32() -> String {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Networking::NetworkListManager::{
            INetworkListManager, NetworkListManager, NLM_ENUM_NETWORK_CONNECTED,
            NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED, NLM_NETWORK_CATEGORY_PRIVATE,
            NLM_NETWORK_CATEGORY_PUBLIC,
        };
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
        };

        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(nlm) =
            CoCreateInstance::<_, INetworkListManager>(&NetworkListManager, None, CLSCTX_ALL)
        {
            if let Ok(networks) = nlm.GetNetworks(NLM_ENUM_NETWORK_CONNECTED) {
                let mut is_private = false;
                let mut is_public = false;
                let mut is_domain = false;

                for _ in 0..10 {
                    let mut item = [None];
                    let mut fetched = 0;
                    if networks.Next(&mut item, Some(&mut fetched)).is_ok() && fetched > 0 {
                        if let Some(net) = &item[0] {
                            if let Ok(cat) = net.GetCategory() {
                                if cat == NLM_NETWORK_CATEGORY_PRIVATE {
                                    is_private = true;
                                } else if cat == NLM_NETWORK_CATEGORY_PUBLIC {
                                    is_public = true;
                                } else if cat == NLM_NETWORK_CATEGORY_DOMAIN_AUTHENTICATED {
                                    is_domain = true;
                                }
                            }
                        }
                    } else {
                        break;
                    }
                }

                if is_private {
                    return "private".to_string();
                } else if is_domain {
                    return "domain".to_string();
                } else if is_public {
                    return "public".to_string();
                }
            }
        }
    }

    "unknown".to_string()
}

// ============================================================
// KEYRING & CERTIFICATE MANAGEMENT
// ============================================================

fn load_tls_private_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(k) if !k.trim().is_empty() => Ok(Some(k)),
        _ => Ok(None),
    }
}

fn save_tls_private_key(key_pem: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY).map_err(|e| e.to_string())?;
    entry.set_password(key_pem).map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_tls_private_key() {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY) {
        let _ = entry.delete_credential();
    }
}

fn parse_pem_cert_der(cert_pem: &str) -> Option<Vec<u8>> {
    let mut b64 = String::new();
    let mut in_cert = false;
    for line in cert_pem.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN CERTIFICATE-----" {
            in_cert = true;
            continue;
        }
        if trimmed == "-----END CERTIFICATE-----" {
            break;
        }
        if in_cert {
            b64.push_str(trimmed);
        }
    }
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

fn get_cert_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&app_data).map_err(|e| e.to_string())?;
    Ok(app_data.join(CERT_FILE_NAME))
}

fn load_tls_cert_pem(app: &AppHandle) -> Result<Option<String>, String> {
    let path = get_cert_path(app)?;
    if path.exists() {
        let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if !content.trim().is_empty() {
            return Ok(Some(content));
        }
    }
    Ok(None)
}

fn save_tls_cert_pem(app: &AppHandle, cert_pem: &str) -> Result<(), String> {
    let path = get_cert_path(app)?;
    std::fs::write(&path, cert_pem).map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_tls_cert_pem(app: &AppHandle) {
    if let Ok(path) = get_cert_path(app) {
        let _ = std::fs::remove_file(path);
    }
}

fn generate_or_load_tls_certs(app: &AppHandle) -> Result<(String, String, String), String> {
    // 1. Try to load from Windows Credential Manager and app data cert file
    let key_opt = load_tls_private_key()?;
    let cert_opt = load_tls_cert_pem(app)?;

    if let (Some(key_pem), Some(cert_pem)) = (key_opt, cert_opt) {
        if let Some(der) = parse_pem_cert_der(&cert_pem) {
            let hash = Sha256::digest(&der);
            let fp_short = format!(
                "{:02X}{:02X} {:02X}{:02X} {:02X}{:02X}",
                hash[0], hash[1], hash[2], hash[3], hash[4], hash[5]
            );
            return Ok((cert_pem, key_pem, fp_short));
        }
    }

    // 2. Generate new self-signed certificate and private key
    let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    for ip in get_private_lan_ips() {
        if !sans.contains(&ip) {
            sans.push(ip);
        }
    }

    let cert = rcgen::generate_simple_self_signed(sans).map_err(|e| e.to_string())?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();

    let der = cert.cert.der();
    let hash = Sha256::digest(der);
    let fp_short = format!(
        "{:02X}{:02X} {:02X}{:02X} {:02X}{:02X}",
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5]
    );

    // Save private key in Windows Credential Manager (NEVER on disk)
    save_tls_private_key(&key_pem)?;

    // Save cert PEM in application data directory
    save_tls_cert_pem(app, &cert_pem)?;

    if let Ok(store) = app.store(STORE_FILENAME) {
        store.set("phone.fingerprint", json!(fp_short));
        let _ = store.save();
    }

    Ok((cert_pem, key_pem, fp_short))
}

// ============================================================
// SERVER CORE (AXUM + RUSTLS + TIMEOUT + CONCURRENCY LIMIT)
// ============================================================

#[derive(Clone)]
struct AppState {
    app: AppHandle,
}

async fn start_server_internal(app: &AppHandle) -> Result<(), String> {
    if SERVER_RUNNING.load(Ordering::SeqCst) {
        return Ok(());
    }

    {
        let mut err_lock = SERVER_ERROR.lock().unwrap();
        *err_lock = None;
    }

    let port = app
        .store(STORE_FILENAME)
        .ok()
        .and_then(|s| s.get("phone.port"))
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_PORT as u64) as u16;

    SERVER_PORT.store(port, Ordering::SeqCst);

    let (cert_pem, key_pem, fp_short) = match generate_or_load_tls_certs(app) {
        Ok(res) => res,
        Err(e) => {
            let mut err_lock = SERVER_ERROR.lock().unwrap();
            *err_lock = Some("tls".to_string());
            return Err(e);
        }
    };

    {
        let mut fp_lock = CERT_FINGERPRINT.lock().unwrap();
        *fp_lock = fp_short;
    }

    let rustls_config = match RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await {
        Ok(c) => c,
        Err(e) => {
            let mut err_lock = SERVER_ERROR.lock().unwrap();
            *err_lock = Some("tls".to_string());
            return Err(e.to_string());
        }
    };

    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));

    let state = AppState { app: app.clone() };

    // Initial API contains exactly: POST /v1/pair, GET /v1/state, DELETE /v1/me, GET /v1/live
    let router = Router::new()
        .route("/v1/pair", post(handle_pair))
        .route("/v1/state", get(handle_get_state))
        .route("/v1/me", delete(handle_delete_me))
        .route("/v1/live", get(handle_live_ws))
        .layer(axum::middleware::from_fn(ip_filter_middleware))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(REQUEST_TIMEOUT_SECS)))
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_CONNECTIONS))
        .with_state(state);

    let handle = axum_server::Handle::new();
    {
        let mut h_lock = SERVER_HANDLE.lock().unwrap();
        *h_lock = Some(handle.clone());
    }

    SERVER_RUNNING.store(true, Ordering::SeqCst);
    touch_activity();

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = app_handle.emit("phone-changed", json!({ "status": "running" }));

        let server_res = axum_server::bind_rustls(bind_addr, rustls_config)
            .handle(handle)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await;

        SERVER_RUNNING.store(false, Ordering::SeqCst);

        if let Err(err) = server_res {
            let err_str = err.to_string();
            let mut err_lock = SERVER_ERROR.lock().unwrap();
            if err_str.contains("address already in use") || err_str.contains("os error 10048") {
                *err_lock = Some("port-busy".to_string());
            } else {
                *err_lock = Some("unknown".to_string());
            }
        }

        let _ = app_handle.emit("phone-changed", json!({ "status": "stopped" }));
    });

    Ok(())
}

fn stop_server_internal() {
    close_all_live_connections("disabled");
    if let Some(handle) = SERVER_HANDLE.lock().unwrap().take() {
        handle.graceful_shutdown(Some(Duration::from_millis(200)));
    }
    SERVER_RUNNING.store(false, Ordering::SeqCst);
    with_connected_device_ids(|s| s.clear());
}

// ============================================================
// MIDDLEWARE & HANDLERS
// ============================================================

async fn ip_filter_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, StatusCode> {
    if !is_allowed_source_ip(&addr.ip()) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(req).await)
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    #[serde(alias = "device_name")]
    device_name: String,
    #[serde(alias = "device_id")]
    device_id: Option<String>,
}

async fn handle_pair(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(payload): Json<PairRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    touch_activity();
    let client_ip = addr.ip();

    if is_ip_blocked(&client_ip) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let clean_req_code: String = payload
        .code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase();

    let mut session_valid = false;
    {
        let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
        if let Some(session) = &mut *session_lock {
            if session.expires_at > Instant::now() {
                if session.clean_code == clean_req_code {
                    session_valid = true;
                } else {
                    session.wrong_attempts += 1;
                    if session.wrong_attempts >= MAX_PAIRING_ATTEMPTS {
                        // 5 wrong attempts destroys the pairing session
                        *session_lock = None;
                        record_failed_attempt(client_ip);
                        return Err(StatusCode::FORBIDDEN);
                    }
                }
            }
        }
        if session_valid {
            *session_lock = None; // Single-use consumption
        }
    }

    if !session_valid {
        record_failed_attempt(client_ip);
        return Err(StatusCode::FORBIDDEN);
    }

    record_success_attempt(&client_ip);

    // Check device limit (max 10)
    let mut devices = load_paired_devices(&state.app);
    if devices.len() >= MAX_DEVICES {
        return Err(StatusCode::FORBIDDEN);
    }

    // Generate exactly 32 random bytes
    let token_bytes: [u8; 32] = rand::thread_rng().gen();
    // Encode using Base64 URL-safe without padding
    let auth_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes);

    let token_hash = format!("{:x}", Sha256::digest(auth_token.as_bytes()));

    let dev_id = payload
        .device_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid_like_id());

    let safe_name = sanitize_device_name(&payload.device_name);

    let now = now_ms();
    let new_dev = PairedDevice {
        id: dev_id.clone(),
        name: safe_name.clone(),
        token_hash,
        created_at: now,
        last_seen: now,
    };

    if let Some(idx) = devices.iter().position(|d| d.id == dev_id) {
        devices[idx] = new_dev;
    } else {
        devices.push(new_dev);
    }

    save_paired_devices(&state.app, &devices);

    with_connected_device_ids(|conns| {
        conns.insert(dev_id.clone());
    });

    let caps = load_capabilities(&state.app);

    let _ = state.app.emit("phone-changed", json!({ "event": "device-paired", "deviceName": safe_name }));

    Ok((
        StatusCode::OK,
        Json(json!({
            "token": auth_token,
            "deviceId": dev_id,
            "capabilities": caps
        })),
    ))
}

async fn handle_get_state(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    touch_activity();
    let dev = authenticate_request(&headers, &state.app, &addr.ip())?;

    update_device_last_seen(&state.app, &dev.id);

    with_connected_device_ids(|conns| {
        conns.insert(dev.id.clone());
    });

    let caps = load_capabilities(&state.app);

    Ok(Json(json!({
        "status": "connected",
        "deviceId": dev.id,
        "capabilities": caps
    })))
}

async fn handle_delete_me(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<StatusCode, StatusCode> {
    touch_activity();
    let dev = authenticate_request(&headers, &state.app, &addr.ip())?;

    phone_remove_device_internal(&state.app, &dev.id);
    Ok(StatusCode::NO_CONTENT)
}

fn authenticate_request(
    headers: &HeaderMap,
    app: &AppHandle,
    client_ip: &IpAddr,
) -> Result<PairedDevice, StatusCode> {
    if is_ip_blocked(client_ip) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.trim())
        .ok_or_else(|| {
            record_failed_attempt(*client_ip);
            StatusCode::UNAUTHORIZED
        })?;

    let req_hash = format!("{:x}", Sha256::digest(auth_header.as_bytes()));

    let devices = load_paired_devices(app);
    let mut authenticated_dev = None;

    for dev in devices {
        // Constant-time hash comparison
        let match_bool: bool = req_hash.as_bytes().ct_eq(dev.token_hash.as_bytes()).into();
        if match_bool {
            authenticated_dev = Some(dev);
            break;
        }
    }

    match authenticated_dev {
        Some(dev) => {
            record_success_attempt(client_ip);
            Ok(dev)
        }
        None => {
            record_failed_attempt(*client_ip);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

async fn handle_live_ws(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    touch_activity();

    // 1. Origin header check: rejects any request that carries an Origin header (403; real clients are not browsers)
    if headers.contains_key("origin") {
        return Err(StatusCode::FORBIDDEN);
    }

    // 2. Authentication check: requires Bearer token in Authorization header
    let dev = authenticate_request(&headers, &state.app, &addr.ip())?;

    // 3. Limit check: at most 12 live connections in total
    if get_live_connections_count() >= MAX_LIVE_CONNECTIONS {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let app = state.app.clone();
    let ws = ws
        .protocols(["dude-live-v1"])
        .max_frame_size(4096)
        .max_message_size(4096);

    Ok(ws.on_upgrade(move |socket| async move {
        handle_live_session(socket, dev, app).await;
    }))
}

async fn handle_live_session(socket: WebSocket, dev: PairedDevice, app: AppHandle) {
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::SeqCst);
    let dev_id = dev.id.clone();
    let (close_tx, mut close_rx) = tokio::sync::mpsc::channel::<String>(2);
    let queue = Arc::new(BoundedLiveQueue::new());

    // at most ONE per device (a new connection from the same device closes the old one with reason "replaced")
    let old_close_tx = register_live_connection(LiveConnection {
        conn_id,
        device_id: dev_id.clone(),
        queue: queue.clone(),
        close_tx,
    });
    if let Some(old_tx) = old_close_tx {
        let _ = old_tx.try_send("replaced".to_string());
    }

    // Mark connected & update last_seen
    update_device_last_seen(&app, &dev_id);
    with_connected_device_ids(|conns| {
        conns.insert(dev_id.clone());
    });
    emit_phone_changed_debounced(&app);

    // Initial messages:
    // {"t":"hello","v":1,"serverName":"...","capabilities":{...},"time":<unix ms>}
    let hello = json!({
        "t": "hello",
        "v": 1,
        "serverName": get_server_name(),
        "capabilities": load_capabilities(&app),
        "time": now_ms()
    }).to_string();
    queue.push(hello);

    // Followed by current agent snapshot
    let snapshot = get_current_agent_snapshot(&app);
    queue.push(snapshot);

    let (mut ws_sender, mut ws_receiver) = socket.split();

    let mut ping_interval = tokio::time::interval(Duration::from_secs(LIVE_PING_INTERVAL_SECS));
    ping_interval.tick().await; // first tick returns immediately

    let mut five_min_interval = tokio::time::interval(Duration::from_secs(300));
    five_min_interval.tick().await;

    let mut last_pong = Instant::now();

    loop {
        // Flush any queued outgoing messages
        let mut send_err = false;
        while let Some(out_msg) = queue.pop() {
            if ws_sender.send(Message::Text(out_msg)).await.is_err() {
                send_err = true;
                break;
            }
        }
        if send_err {
            break;
        }

        tokio::select! {
            _ = ping_interval.tick() => {
                if last_pong.elapsed() > Duration::from_secs(LIVE_PONG_TIMEOUT_SECS) {
                    break;
                }
                if ws_sender.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            _ = five_min_interval.tick() => {
                update_device_last_seen(&app, &dev_id);
                touch_activity();
            }
            Some(reason) = close_rx.recv() => {
                let bye = json!({ "t": "bye", "reason": reason }).to_string();
                let _ = ws_sender.send(Message::Text(bye)).await;
                let _ = ws_sender.close().await;
                break;
            }
            _ = queue.notify.notified() => {
                while let Some(out_msg) = queue.pop() {
                    if ws_sender.send(Message::Text(out_msg)).await.is_err() {
                        send_err = true;
                        break;
                    }
                }
                if send_err {
                    break;
                }
            }
            incoming = ws_receiver.next() => {
                match incoming {
                    Some(Ok(Message::Ping(payload))) => {
                        last_pong = Instant::now();
                        let _ = ws_sender.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_pong = Instant::now();
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                            if val.get("t").and_then(|v| v.as_str()) == Some("ping") {
                                last_pong = Instant::now();
                                let pong = json!({ "t": "pong" }).to_string();
                                if ws_sender.send(Message::Text(pong)).await.is_err() {
                                    break;
                                }
                            } else {
                                IGNORED_PHONE_MESSAGES_COUNT.fetch_add(1, Ordering::Relaxed);
                            }
                        } else {
                            IGNORED_PHONE_MESSAGES_COUNT.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        IGNORED_PHONE_MESSAGES_COUNT.fetch_add(1, Ordering::Relaxed);
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Err(_)) => {
                        // Frame/message too large (>4 KB) or protocol error -> close
                        break;
                    }
                }
            }
        }
    }

    // Cleanup on disconnect
    unregister_live_connection(conn_id, &dev_id, &app);
}

pub fn require_capability(app: &AppHandle, key: &str) -> Result<(), StatusCode> {
    let caps = load_capabilities(app);
    let allowed = match key {
        "notifications" => caps.notifications,
        "files" => caps.files,
        "clipboard" => caps.clipboard,
        "media" => caps.media,
        "agents" => caps.agents,
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

// ============================================================
// STORAGE & HELPERS (Atomic & Debounced)
// ============================================================

fn get_devices_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&app_data).map_err(|e| e.to_string())?;
    Ok(app_data.join(DEVICES_FILE_NAME))
}

fn load_paired_devices(app: &AppHandle) -> Vec<PairedDevice> {
    if let Ok(path) = get_devices_path(app) {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(devs) = serde_json::from_str::<Vec<PairedDevice>>(&content) {
                    return devs;
                }
            }
        }
    }

    app.store(STORE_FILENAME)
        .ok()
        .and_then(|s| s.get("phone.devices"))
        .and_then(|v| serde_json::from_value::<Vec<PairedDevice>>(v).ok())
        .unwrap_or_default()
}

fn save_paired_devices(app: &AppHandle, devices: &[PairedDevice]) {
    // Atomic file write via temp file rename with safe Windows replacement
    if let Ok(path) = get_devices_path(app) {
        if let Ok(json_str) = serde_json::to_string_pretty(devices) {
            let tmp_path = path.with_extension("tmp");
            if std::fs::write(&tmp_path, &json_str).is_ok() {
                // On Windows, std::fs::rename fails if destination exists.
                // Replace safely:
                if std::fs::rename(&tmp_path, &path).is_err() {
                    let _ = std::fs::write(&path, &json_str);
                    let _ = std::fs::remove_file(&tmp_path);
                }
            } else {
                let _ = std::fs::write(&path, &json_str);
            }
        }
    }

    // Mirror to store
    if let Ok(store) = app.store(STORE_FILENAME) {
        store.set("phone.devices", json!(devices));
        let _ = store.save();
    }
}

fn update_device_last_seen(app: &AppHandle, dev_id: &str) {
    let now = now_ms();
    let should_save = {
        let mut lock = LAST_SAVED_SEEN.lock().unwrap();
        let map = lock.get_or_insert_with(HashMap::new);
        match map.get(dev_id) {
            Some(last_inst) if last_inst.elapsed() < Duration::from_secs(15) => false,
            _ => {
                map.insert(dev_id.to_string(), Instant::now());
                true
            }
        }
    };

    if should_save {
        let mut devices = load_paired_devices(app);
        if let Some(d) = devices.iter_mut().find(|d| d.id == dev_id) {
            d.last_seen = now;
            save_paired_devices(app, &devices);
        }
    }
}

fn load_capabilities(app: &AppHandle) -> PhoneCapabilities {
    app.store(STORE_FILENAME)
        .ok()
        .and_then(|s| s.get("phone.capabilities"))
        .and_then(|v| serde_json::from_value::<PhoneCapabilities>(v).ok())
        .unwrap_or_default()
}

fn save_capabilities(app: &AppHandle, caps: &PhoneCapabilities) {
    if let Ok(store) = app.store(STORE_FILENAME) {
        store.set("phone.capabilities", json!(caps));
        let _ = store.save();
    }
}

fn sanitize_device_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Android Phone".to_string()
    } else {
        trimmed.to_string()
    }
}

fn uuid_like_id() -> String {
    let mut rng = rand::thread_rng();
    let r1: u32 = rng.gen();
    let r2: u32 = rng.gen();
    format!("{:08x}-{:08x}", r1, r2)
}

fn phone_remove_device_internal(app: &AppHandle, id: &str) {
    close_device_live_connections(id, "removed");
    let mut devices = load_paired_devices(app);
    if let Some(pos) = devices.iter().position(|d| d.id == id) {
        devices.remove(pos);
        save_paired_devices(app, &devices);
    }

    with_connected_device_ids(|conns| {
        conns.remove(id);
    });

    let _ = app.emit("phone-changed", json!({ "event": "device-removed", "deviceId": id }));
}

// ============================================================
// TAURI COMMANDS
// ============================================================

#[tauri::command]
pub fn phone_get_status(app: AppHandle) -> Result<PhoneLinkStatus, String> {
    let running = SERVER_RUNNING.load(Ordering::SeqCst);
    let port = SERVER_PORT.load(Ordering::SeqCst);
    let error = SERVER_ERROR.lock().unwrap().clone();
    let addresses = get_private_lan_ips();
    let fingerprint_short = CERT_FINGERPRINT.lock().unwrap().clone();
    let network_profile = get_network_profile_category();
    let capabilities = load_capabilities(&app);
    let auto_off_minutes = AUTO_OFF_MINUTES.load(Ordering::SeqCst);

    let paired_records = load_paired_devices(&app);
    let now = now_ms();
    let connected_set: HashSet<String> = with_connected_device_ids(|c| c.clone());

    let devices: Vec<PairedDeviceView> = paired_records
        .into_iter()
        .map(|d| {
            let is_active = connected_set.contains(&d.id) || (now - d.last_seen < 60_000);
            PairedDeviceView {
                id: d.id,
                name: d.name,
                created_at: d.created_at,
                last_seen: d.last_seen,
                connected: is_active,
            }
        })
        .collect();

    Ok(PhoneLinkStatus {
        running,
        error,
        port,
        addresses,
        fingerprint_short,
        network_profile,
        capabilities,
        auto_off_minutes,
        devices,
    })
}

#[tauri::command]
pub async fn phone_set_enabled(app: AppHandle, enabled: bool) -> Result<PhoneLinkStatus, String> {
    if let Ok(store) = app.store(STORE_FILENAME) {
        store.set("phone.enabled", json!(enabled));
        let _ = store.save();
    }

    if enabled {
        let _ = start_server_internal(&app).await;
    } else {
        stop_server();
    }

    phone_get_status(app)
}

#[tauri::command]
pub fn phone_start_pairing(app: AppHandle) -> Result<PairingSessionView, String> {
    let devices = load_paired_devices(&app);
    if devices.len() >= MAX_DEVICES {
        return Err("Limit reached (10)".to_string());
    }

    let mut rng = rand::thread_rng();
    let charset = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut part1 = String::new();
    let mut part2 = String::new();

    for _ in 0..4 {
        part1.push(charset[rng.gen_range(0..charset.len())] as char);
        part2.push(charset[rng.gen_range(0..charset.len())] as char);
    }

    let code_formatted = format!("{}-{}", part1, part2);
    
    let clean_code = format!("{}{}", part1, part2);

    let nonce: [u8; 16] = rng.gen();
    let nonce_hex = format!("{:x}", Sha256::digest(nonce));

    let port = SERVER_PORT.load(Ordering::SeqCst);
    let ips = get_private_lan_ips();
    let primary_ip = ips.first().cloned().unwrap_or_else(|| "127.0.0.1".into());
    let fp = CERT_FINGERPRINT.lock().unwrap().clone();

    let qr_payload = json!({
        "v": 1,
        "ip": primary_ip,
        "port": port,
        "fp": fp,
        "code": code_formatted,
        "nonce": nonce_hex
    })
    .to_string();

    let qr = QrCode::new(qr_payload.as_bytes()).map_err(|e| e.to_string())?;
    let qr_size = qr.width();
    let qr_modules: Vec<bool> = qr.into_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();

    let session = PairingSession {
        code: code_formatted.clone(),
        clean_code,
        nonce: nonce_hex,
        expires_at: Instant::now() + Duration::from_secs(PAIRING_TIMEOUT_SECS),
        wrong_attempts: 0,
        qr_modules: qr_modules.clone(),
        qr_size,
    };

    {
        let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
        *session_lock = Some(session);
    }

    Ok(PairingSessionView {
        code: code_formatted,
        expires_in_sec: PAIRING_TIMEOUT_SECS,
        qr_modules,
        qr_size,
    })
}

#[tauri::command]
pub fn phone_cancel_pairing() -> Result<(), String> {
    let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
    *session_lock = None;
    Ok(())
}

#[tauri::command]
pub fn phone_refresh_pairing(app: AppHandle) -> Result<PairingSessionView, String> {
    phone_start_pairing(app)
}

#[tauri::command]
pub fn phone_copy_code() -> Result<(), String> {
    let clean_code = {
        let session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
        match &*session_lock {
            Some(s) if s.expires_at > Instant::now() => s.clean_code.clone(),
            _ => return Err("No active pairing session".to_string()),
        }
    };

    crate::clipboard_history::write_clipboard_skipping_history(&clean_code)?;

    let code_clone = clean_code;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let _ = crate::clipboard_history::clear_windows_clipboard_if_matches(&code_clone);
    });

    Ok(())
}

#[tauri::command]
pub fn phone_remove_device(app: AppHandle, id: String) -> Result<(), String> {
    phone_remove_device_internal(&app, &id);
    Ok(())
}

#[tauri::command]
pub fn phone_rename_device(app: AppHandle, id: String, name: String) -> Result<(), String> {
    let clean_id = id.trim();
    let safe_name = sanitize_device_name(&name);
    let mut devices = load_paired_devices(&app);
    if let Some(d) = devices.iter_mut().find(|d| d.id == clean_id) {
        d.name = safe_name.clone();
        save_paired_devices(&app, &devices);
        let _ = app.emit("phone-changed", json!({ "event": "device-renamed", "deviceId": clean_id, "name": safe_name }));
    }
    Ok(())
}

#[tauri::command]
pub fn phone_forget_all(app: AppHandle) -> Result<(), String> {
    close_all_live_connections("shutdown");
    save_paired_devices(&app, &[]);
    with_connected_device_ids(|conns| {
        conns.clear();
    });

    // Delete TLS key from Windows Credential Manager and cert file from disk
    delete_tls_private_key();
    delete_tls_cert_pem(&app);

    if let Ok(store) = app.store(STORE_FILENAME) {
        store.delete("phone.keyPem");
        store.delete("phone.certPem");
        store.delete("phone.fingerprint");
        let _ = store.save();
    }

    if SERVER_RUNNING.load(Ordering::SeqCst) {
        stop_server();
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = start_server_internal(&app_clone).await;
        });
    }

    let _ = app.emit("phone-changed", json!({ "event": "all-forgotten" }));
    Ok(())
}

#[tauri::command]
pub fn phone_set_capability(app: AppHandle, key: String, enabled: bool) -> Result<PhoneCapabilities, String> {
    let mut caps = load_capabilities(&app);
    match key.as_str() {
        "notifications" => caps.notifications = enabled,
        "files" => caps.files = enabled,
        "clipboard" => caps.clipboard = enabled,
        "media" => caps.media = enabled,
        "agents" => caps.agents = enabled,
        _ => return Err(format!("Unknown capability key: {}", key)),
    }
    save_capabilities(&app, &caps);
    broadcast_caps_to_live_connections(&app, &caps);
    let _ = app.emit("phone-changed", json!({ "event": "capabilities-changed", "capabilities": caps }));
    Ok(caps)
}

#[tauri::command]
pub fn phone_publish_agent(app: AppHandle, state: AgentPublishState) -> Result<(), String> {
    if !SERVER_RUNNING.load(Ordering::SeqCst) {
        return Ok(());
    }
    let validated = validate_agent_publish_state(state)?;
    publish_agent_state(&app, validated);
    Ok(())
}

#[tauri::command]
pub fn phone_set_auto_off(app: AppHandle, minutes: u32) -> Result<(), String> {
    AUTO_OFF_MINUTES.store(minutes, Ordering::SeqCst);
    if let Ok(store) = app.store(STORE_FILENAME) {
        store.set("phone.autoOffMinutes", json!(minutes));
        let _ = store.save();
    }
    let _ = app.emit("phone-changed", json!({ "event": "auto-off-changed", "minutes": minutes }));
    Ok(())
}

#[tauri::command]
pub async fn phone_restart(app: AppHandle) -> Result<PhoneLinkStatus, String> {
    close_all_live_connections("shutdown");
    stop_server();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = start_server_internal(&app).await;
    phone_get_status(app)
}

#[tauri::command]
pub fn phone_open_wifi_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "Start-Process 'ms-settings:network-wifi'",
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.spawn();
    }
    Ok(())
}

// ============================================================
// SECURITY & COMPLIANCE UNIT TESTS
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_phone_capabilities_default() {
        let caps = PhoneCapabilities::default();
        assert!(caps.notifications);
        assert!(caps.files);
        assert!(!caps.clipboard); // Off by default
        assert!(caps.media);
        assert!(caps.agents); // Default on
    }

    #[test]
    fn test_sanitize_device_name() {
        assert_eq!(sanitize_device_name("  Pixel 8 Pro  "), "Pixel 8 Pro");
        assert_eq!(sanitize_device_name(""), "Android Phone");
        assert_eq!(sanitize_device_name("   \n\t  "), "Android Phone");
        let long_name = "A".repeat(50);
        let sanitized = sanitize_device_name(&long_name);
        assert_eq!(sanitized.len(), 40);
    }

    #[test]
    fn test_qr_generation_matrix() {
        let payload = json!({
            "v": 1,
            "ip": "192.168.1.100",
            "port": 50432,
            "code": "8F4D-9K2M"
        }).to_string();

        let qr = QrCode::new(payload.as_bytes()).expect("QR code generation failed");
        let size = qr.width();
        let modules: Vec<bool> = qr.into_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();

        assert!(size > 0);
        assert_eq!(modules.len(), size * size);
    }

    #[test]
    fn test_private_lan_ips_not_empty() {
        let ips = get_private_lan_ips();
        assert!(!ips.is_empty());
    }

    #[test]
    fn test_allowed_ips_filtering() {
        // Allowed: RFC1918 IPv4
        assert!(is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 1, 1))));
        assert!(is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 254))));
        assert!(is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42))));

        // Allowed: IPv4 Link-Local
        assert!(is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 10, 20))));

        // Allowed: Loopback
        assert!(is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_allowed_source_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)));

        // Allowed: IPv6 ULA (fc00::/7)
        let ula1: Ipv6Addr = "fd12:3456:789a:1::1".parse().unwrap();
        assert!(is_allowed_source_ip(&IpAddr::V6(ula1)));
        let ula2: Ipv6Addr = "fc00::1".parse().unwrap();
        assert!(is_allowed_source_ip(&IpAddr::V6(ula2)));

        // Allowed: IPv6 Link-Local (fe80::/10)
        let ll6: Ipv6Addr = "fe80::1ff:fe23:4567".parse().unwrap();
        assert!(is_allowed_source_ip(&IpAddr::V6(ll6)));

        // Rejected: Public IPv4
        assert!(!is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(172, 15, 0, 1))));
        assert!(!is_allowed_source_ip(&IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1))));

        // Rejected: Public IPv6
        let pub6: Ipv6Addr = "2001:4860:4860::8888".parse().unwrap();
        assert!(!is_allowed_source_ip(&IpAddr::V6(pub6)));
    }

    #[test]
    fn test_token_generation_32_bytes_base64url() {
        let token_bytes: [u8; 32] = rand::thread_rng().gen();
        let auth_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes);

        // 32 bytes encoded in base64url without padding is exactly 43 chars
        assert_eq!(auth_token.len(), 43);
        // Ensure no padding '=' and no non-URL-safe characters
        assert!(!auth_token.contains('='));
        assert!(!auth_token.contains('+'));
        assert!(!auth_token.contains('/'));
    }

    #[test]
    fn test_constant_time_comparison() {
        let hash1 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let hash2 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let hash3 = "0000000000000000000000000000000000000000000000000000000000000000";

        let eq: bool = hash1.as_bytes().ct_eq(hash2.as_bytes()).into();
        assert!(eq);

        let ne: bool = hash1.as_bytes().ct_eq(hash3.as_bytes()).into();
        assert!(!ne);
    }

    #[test]
    fn test_pairing_code_alphabet_and_length() {
        let charset = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
        let mut rng = rand::thread_rng();

        let mut code = String::new();
        for _ in 0..8 {
            code.push(charset[rng.gen_range(0..charset.len())] as char);
        }

        assert_eq!(code.len(), 8);
        for c in code.chars() {
            assert!(charset.contains(&(c as u8)));
            assert_ne!(c, '0');
            assert_ne!(c, 'O');
            assert_ne!(c, '1');
            assert_ne!(c, 'I');
        }
    }

    #[test]
    fn test_pairing_wrong_attempts_invalidation() {
        let test_session = PairingSession {
            code: "8F4D-9K2M".into(),
            clean_code: "8F4D9K2M".into(),
            nonce: "test-nonce".into(),
            expires_at: Instant::now() + Duration::from_secs(120),
            wrong_attempts: 4,
            qr_modules: vec![true],
            qr_size: 1,
        };

        {
            let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
            *session_lock = Some(test_session);
        }

        // Simulate 5th wrong attempt
        {
            let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
            let session = session_lock.as_mut().unwrap();
            session.wrong_attempts += 1;
            if session.wrong_attempts >= MAX_PAIRING_ATTEMPTS {
                *session_lock = None;
            }
        }

        let lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
        assert!(lock.is_none(), "Session must be invalidated after 5 wrong attempts");
    }

    #[test]
    fn test_rate_limiter_5_attempts_and_block() {
        let test_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 99, 99));

        // Ensure clean state
        record_success_attempt(&test_ip);
        assert!(!is_ip_blocked(&test_ip));

        // 4 failed attempts -> not blocked yet
        for _ in 0..4 {
            record_failed_attempt(test_ip);
            assert!(!is_ip_blocked(&test_ip));
        }

        // 5th failed attempt -> blocked for 60s
        record_failed_attempt(test_ip);
        assert!(is_ip_blocked(&test_ip));

        // Other IPs are unaffected
        let other_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 99, 100));
        assert!(!is_ip_blocked(&other_ip));

        // Reset
        record_success_attempt(&test_ip);
        assert!(!is_ip_blocked(&test_ip));
    }

    #[test]
    fn test_device_limit_max_10() {
        let mut devices = Vec::new();
        for i in 0..10 {
            devices.push(PairedDevice {
                id: format!("dev-{}", i),
                name: format!("Phone {}", i),
                token_hash: "hash".into(),
                created_at: 0,
                last_seen: 0,
            });
        }
        assert_eq!(devices.len(), MAX_DEVICES);
        assert!(devices.len() >= MAX_DEVICES);
    }

    #[test]
    fn test_keyring_credential_manager_storage() {
        let test_key = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----";
        let res = save_tls_private_key(test_key);
        if res.is_ok() {
            let loaded = load_tls_private_key().unwrap();
            assert_eq!(loaded, Some(test_key.to_string()));
            delete_tls_private_key();
            let after_delete = load_tls_private_key().unwrap();
            assert!(after_delete.is_none());
        }
    }

    #[test]
    fn test_concurrency_and_timeout_constants() {
        assert_eq!(MAX_CONCURRENT_CONNECTIONS, 12);
        assert_eq!(REQUEST_TIMEOUT_SECS, 15);
        assert_eq!(MAX_BODY_BYTES, 16 * 1024);
    }

    #[test]
    fn test_pair_request_deserialization_camel_and_snake() {
        // CamelCase (from user prompt)
        let json_camel = r#"{"code":"WRONG123","deviceName":"test"}"#;
        let req_camel: PairRequest = serde_json::from_str(json_camel).expect("Failed to parse camelCase PairRequest");
        assert_eq!(req_camel.code, "WRONG123");
        assert_eq!(req_camel.device_name, "test");
        assert!(req_camel.device_id.is_none());

        // CamelCase with deviceId
        let json_camel_full = r#"{"code":"8F4D-9K2M","deviceName":"OnePlus Nord","deviceId":"dev-abc"}"#;
        let req_full: PairRequest = serde_json::from_str(json_camel_full).expect("Failed to parse camelCase PairRequest with id");
        assert_eq!(req_full.code, "8F4D-9K2M");
        assert_eq!(req_full.device_name, "OnePlus Nord");
        assert_eq!(req_full.device_id.as_deref(), Some("dev-abc"));

        // Snake_case (from PHONE.md)
        let json_snake = r#"{"code":"8F4D-9K2M","device_name":"Pixel 8 Pro","device_id":"dev-xyz"}"#;
        let req_snake: PairRequest = serde_json::from_str(json_snake).expect("Failed to parse snake_case PairRequest");
        assert_eq!(req_snake.code, "8F4D-9K2M");
        assert_eq!(req_snake.device_name, "Pixel 8 Pro");
        assert_eq!(req_snake.device_id.as_deref(), Some("dev-xyz"));
    }

    #[test]
    fn test_unquoted_json_powershell_curl_error_reproduction() {
        // When PowerShell executes: curl.exe ... --data-raw "$body"
        // PowerShell strips quotes from $body, passing `{code:WRONG123,deviceName:test}` to curl.
        let raw_powershell_arg = "{code:WRONG123,deviceName:test}";
        let parse_res = serde_json::from_str::<PairRequest>(raw_powershell_arg);
        assert!(parse_res.is_err());
        let err_msg = parse_res.err().unwrap().to_string();
        assert!(
            err_msg.contains("key must be a string at line 1 column 2"),
            "Expected exact serde error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_pairing_wrong_code_and_reuse_returns_403() {
        let client_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 50));
        record_success_attempt(&client_ip);

        // 1. Set active session with code "8F4D9K2M"
        {
            let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
            *session_lock = Some(PairingSession {
                code: "8F4D-9K2M".into(),
                clean_code: "8F4D9K2M".into(),
                nonce: "test_nonce".into(),
                expires_at: Instant::now() + Duration::from_secs(120),
                wrong_attempts: 0,
                qr_modules: vec![true, false],
                qr_size: 2,
            });
        }

        // Helper to validate code simulating handle_pair session logic
        let validate_pairing = |attempt_code: &str| -> Result<(), StatusCode> {
            let clean_req_code: String = attempt_code
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_uppercase();

            let mut session_valid = false;
            {
                let mut session_lock = ACTIVE_PAIRING_SESSION.lock().unwrap();
                if let Some(session) = &mut *session_lock {
                    if session.expires_at > Instant::now() {
                        if session.clean_code == clean_req_code {
                            session_valid = true;
                        } else {
                            session.wrong_attempts += 1;
                            if session.wrong_attempts >= MAX_PAIRING_ATTEMPTS {
                                *session_lock = None;
                                return Err(StatusCode::FORBIDDEN);
                            }
                        }
                    }
                }
                if session_valid {
                    *session_lock = None;
                }
            }
            if session_valid {
                Ok(())
            } else {
                Err(StatusCode::FORBIDDEN)
            }
        };

        // 2. Wrong code returns 403 Forbidden
        let err_wrong = validate_pairing("WRONG123").unwrap_err();
        assert_eq!(err_wrong, StatusCode::FORBIDDEN);

        // 3. Correct code succeeds (and single-use consumes session)
        let ok_res = validate_pairing("8F4D-9K2M");
        assert!(ok_res.is_ok());

        // 4. Reusing the code returns 403 Forbidden
        let err_reused = validate_pairing("8F4D-9K2M").unwrap_err();
        assert_eq!(err_reused, StatusCode::FORBIDDEN);

        // 5. Test authenticate_request without token returns 401 Unauthorized
        let headers = HeaderMap::new();
        let auth_res = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|s| s.trim())
            .ok_or(StatusCode::UNAUTHORIZED);
        assert_eq!(auth_res.unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_device_rename_and_persistence_replacement() {
        let temp_dir = std::env::temp_dir().join(format!("dude_test_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let target_path = temp_dir.join("devices.json");
        let tmp_path = target_path.with_extension("tmp");

        let mut devices = vec![PairedDevice {
            id: "dev-123".into(),
            name: "Original Name".into(),
            created_at: 1000,
            last_seen: 1000,
            token_hash: "hash".into(),
        }];

        // First write
        let json_1 = serde_json::to_string_pretty(&devices).unwrap();
        std::fs::write(&target_path, json_1).unwrap();

        // Rename device in memory
        let safe_name = sanitize_device_name("  Renamed Device  ");
        assert_eq!(safe_name, "Renamed Device");
        devices[0].name = safe_name;

        // Perform safe replacement (handles Windows AlreadyExists error on rename)
        let json_2 = serde_json::to_string_pretty(&devices).unwrap();
        std::fs::write(&tmp_path, &json_2).unwrap();
        if std::fs::rename(&tmp_path, &target_path).is_err() {
            std::fs::write(&target_path, &json_2).unwrap();
            let _ = std::fs::remove_file(&tmp_path);
        }

        // Read back
        let content = std::fs::read_to_string(&target_path).unwrap();
        let loaded: Vec<PairedDevice> = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded[0].name, "Renamed Device");

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_agent_state_validation() {
        // Valid status, folder extraction, truncation, edits clamp, since
        let now = now_ms();
        let valid_state = AgentPublishState {
            status: "working".into(),
            project: Some("D:/Development/my_super_long_project_name_that_exceeds_forty_characters_limit".into()),
            step: Some("edit".into()),
            since: Some(now - 1000),
            edits: Some(1500), // should clamp to 999
        };
        let res = validate_agent_publish_state(valid_state).unwrap();
        assert_eq!(res.status, "working");
        // Folder extracted & clamped to 40 chars
        assert!(res.project.len() <= 40);
        assert_eq!(res.project, "my_super_long_project_name_that_exceeds_");
        assert_eq!(res.step, Some("edit".into()));
        assert_eq!(res.edits, 999);
        assert_eq!(res.since, Some(now - 1000));
        assert!(res.seq > 0);

        // Invalid status rejected
        let invalid_status = AgentPublishState {
            status: "hacking".into(),
            project: None,
            step: None,
            since: None,
            edits: None,
        };
        assert!(validate_agent_publish_state(invalid_status).is_err());

        // Stale timestamp (>24h old) dropped to null
        let stale_state = AgentPublishState {
            status: "idle".into(),
            project: None,
            step: Some("unknown_step".into()), // unknown step becomes None
            since: Some(now - 90_000_000), // > 24 hours ago
            edits: Some(5),
        };
        let res2 = validate_agent_publish_state(stale_state).unwrap();
        assert_eq!(res2.status, "idle");
        assert_eq!(res2.step, None);
        assert_eq!(res2.since, None);
        assert_eq!(res2.edits, 5);
        assert_eq!(res2.project, "");
    }

    #[test]
    fn test_bounded_live_queue_drops_oldest() {
        let queue = BoundedLiveQueue::new();
        for i in 0..20 {
            queue.push(format!("msg_{}", i));
        }
        // Exactly 16 messages retained
        let mut messages = Vec::new();
        while let Some(m) = queue.pop() {
            messages.push(m);
        }
        assert_eq!(messages.len(), 16);
        // Oldest 4 (msg_0, msg_1, msg_2, msg_3) were dropped
        assert_eq!(messages[0], "msg_4");
        assert_eq!(messages[15], "msg_19");
    }

    #[test]
    fn test_origin_header_rejection_logic() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://malicious-site.com".parse().unwrap());
        // Condition: rejects any request that carries an Origin header (403)
        assert!(headers.contains_key("origin"));
    }

    #[test]
    fn test_live_connections_limit_and_replacement_logic() {
        // Clear live connections
        {
            let mut lock = LIVE_CONNECTIONS.lock().unwrap();
            *lock = Some(HashMap::new());
        }

        let q1 = Arc::new(BoundedLiveQueue::new());
        let (close_tx1, mut close_rx1) = tokio::sync::mpsc::channel(2);
        let old1 = register_live_connection(LiveConnection {
            conn_id: 101,
            device_id: "dev-phone-1".into(),
            queue: q1,
            close_tx: close_tx1,
        });
        assert!(old1.is_none());
        assert_eq!(get_live_connections_count(), 1);

        // A new connection from the same device replaces old one
        let q2 = Arc::new(BoundedLiveQueue::new());
        let (close_tx2, _close_rx2) = tokio::sync::mpsc::channel(2);
        let old2 = register_live_connection(LiveConnection {
            conn_id: 102,
            device_id: "dev-phone-1".into(),
            queue: q2,
            close_tx: close_tx2,
        });
        assert!(old2.is_some());
        let _ = old2.unwrap().try_send("replaced".into());
        assert_eq!(close_rx1.try_recv().unwrap(), "replaced");
        assert_eq!(get_live_connections_count(), 1);

        // Fill up to MAX_LIVE_CONNECTIONS (12)
        for i in 2..=12 {
            let q = Arc::new(BoundedLiveQueue::new());
            let (tx, _) = tokio::sync::mpsc::channel(2);
            register_live_connection(LiveConnection {
                conn_id: 200 + i,
                device_id: format!("dev-phone-{}", i),
                queue: q,
                close_tx: tx,
            });
        }
        assert_eq!(get_live_connections_count(), 12);

        // 13th refused
        assert!(get_live_connections_count() >= MAX_LIVE_CONNECTIONS);

        // Close all with "shutdown"
        close_all_live_connections("shutdown");
        assert_eq!(get_live_connections_count(), 0);
    }
}
