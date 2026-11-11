# Dude Link (Desktop Server) — Architecture & Protocol Specification

## 1. Overview & Security Philosophy
**Dude Link** enables encrypted local communication between your Android smartphone running the Dude Companion App and the Dude Desktop application.

### Key Security Principles
1. **Default-Off**:
   - Dude Link is disabled by default upon initial installation.
   - When disabled (`phone.enabled = false`), **zero ports are bound**, no network sockets are opened, no background listener runs, and no TLS certificates or keys are active in memory.
2. **Local Area Network (LAN) Only & Source IP Filtering**:
   - All communication happens directly across the user's local network.
   - Source IP filtering restricts access to:
     - RFC 1918 private IPv4 (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`)
     - IPv4 Link-Local (`169.254.0.0/16`)
     - IPv6 Unique Local Addresses (ULA, `fc00::/7`)
     - IPv6 Link-Local (`fe80::/10`)
     - Loopback (`127.0.0.0/8`, `::1`) for testing.
   - Any external or public IP connection is rejected immediately with HTTP `403 Forbidden`.
   - No external cloud servers, relay servers, or telemetry endpoints exist. Everything stays on your local network.
3. **Pure-Rust Memory-Safe TLS 1.2 & 1.3**:
   - The desktop server runs an embedded `axum` + `axum-server` instance using `rustls` (pure-Rust memory-safe TLS).
   - An X.509 self-signed certificate is dynamically generated via `rcgen` bound to the machine's local LAN IPs and localhost.
   - **Private TLS Key Storage**: Stored exclusively in native **Windows Credential Manager** (`service: dude-app`, `entry: phone-tls-key`). The private key is never written to disk or `dude_store.json`.
   - **Certificate PEM Storage**: Stored as `phone-cert.pem` in Dude's application data directory.
   - The desktop computes an uppercase hex SHA-256 fingerprint (e.g. `A1B2 C3D4 E5F6...`), which is pinned directly by the mobile client via the pairing QR code.
4. **Connection, Timeout & Request Limits**:
   - **Maximum 4 concurrent connections** enforced via `tower::limit::ConcurrencyLimitLayer`.
   - **15-second request timeout** enforced via `tower_http::timeout::TimeoutLayer`.
   - **Maximum 16 KB request body** enforced via `axum::extract::DefaultBodyLimit::max(16 * 1024)`.
5. **Rate Limiting**:
   - Maximum 5 failed pairing or authentication attempts per source IP per minute.
   - Exceeding this threshold blocks that source IP for 60 seconds with HTTP `429 Too Many Requests`.
6. **Zero Sensitive Logging**:
   - The desktop server never logs bearer tokens, pairing codes, device identifiers, or request payloads. Debug builds log only event types and HTTP status codes.

---

## 2. Pairing Protocol & Handshake

```
+-----------------------------------------------------------------------------------+
|                               PAIRING FLOW DIAGRAM                                |
+-----------------------------------------------------------------------------------+
|  Desktop (Dude)                                          Mobile (Dude Companion)  |
|         |                                                           |             |
|         |-- 1. Generate Ephemeral Session (120s TTL)                |             |
|         |      (Code: 8-char, Alphabet: 23456789ABCDEFGHJKLMNPQRSTUVWXYZ)         |
|         |                                                           |             |
|         |-- 2. Display QR Code & Monospace Pill in Settings ------->|             |
|         |                                                           |             |
|         |                                    3. Scan QR Code or ----|             |
|         |                                       Enter 8-char Code   |             |
|         |                                                           |             |
|         |<-- 4. POST /v1/pair (code, device_name, device_id) -------|             |
|         |                                                           |             |
|         |-- 5. Validate Code & Session (single-use)                 |             |
|         |      Destroy session after 5 wrong attempts               |             |
|         |      Generate 32 random bytes -> URL-safe Base64 token    |             |
|         |      Store SHA-256 Token Hash atomically in               |             |
|         |      phone-devices.json (max 10 devices)                  |             |
|         |      Emit "phone-changed" to Desktop UI                   |             |
|         |                                                           |             |
|         |-- 6. Response 200 OK (token, deviceId, capabilities) ---->|             |
+-----------------------------------------------------------------------------------+
```

### Pairing Rules
- **Alphabet**: Exactly 32 unambiguous characters: `23456789ABCDEFGHJKLMNPQRSTUVWXYZ` (no `0`, `O`, `1`, `I`).
- **Session Lifespan**: Exactly 120 seconds. Generated using a cryptographic RNG (`rand::rngs::OsRng`).
- **Wrong Attempts Limit**: Maximum 5 wrong attempts before the pairing session is destroyed.
- **Single-Use Invalidation**: Upon successful pairing, the active session is immediately consumed and discarded.
- **Device Limit**: Maximum of 10 paired devices. If limit is reached, pairing requests return `403 Forbidden` (`Limit reached (10)`).
- **Clipboard Isolation**: When `phone_copy_code()` is invoked, the code is copied without spaces/dashes, flagged to be ignored by Dude's Clipboard History, and automatically wiped from the Windows clipboard after 60 seconds if unchanged.

---

## 3. Desktop IPC Commands (Tauri API)

All commands are exposed to the Settings UI via `tauri::generate_handler!`:

| Command | Arguments | Return Type | Description |
| :--- | :--- | :--- | :--- |
| `phone_get_status` | None | `PhoneLinkStatus` | Returns current server state, port, LAN IPs, cert fingerprint, and paired devices. |
| `phone_set_enabled` | `enabled: bool` | `PhoneLinkStatus` | Toggles the desktop Link Server on or off. Starts or stops the Axum TLS server. |
| `phone_start_pairing` | None | `PairingSessionView` | Initiates a 120-second pairing session and returns QR code module matrix + formatted code. |
| `phone_cancel_pairing`| None | `()` | Cancels and clears the current active pairing session. |
| `phone_remove_device` | `id: String` | `()` | Unpairs a device and invalidates its token hash. |
| `phone_forget_all` | None | `()` | Removes all paired devices, stops server, and regenerates TLS certificate and keys. |

---

## 4. Initial HTTP Server Endpoints

The initial desktop Phone Link server exposes strictly three HTTP endpoints:

### 4.1 `POST /v1/pair`
- **Auth**: None (Validates active pairing session code)
- **Request Body** (Max 16 KB):
  ```json
  {
    "code": "8F4D-9K2M",
    "device_name": "OnePlus Nord CE4 Lite 5G",
    "device_id": "optional-uuid"
  }
  ```
- **Response**: `200 OK`
  ```json
  {
    "token": "43-character-url-safe-base64-token-no-padding",
    "deviceId": "assigned-device-id",
    "capabilities": {
      "notifications": true,
      "files": true,
      "clipboard": false,
      "media": true
    }
  }
  ```
- **Error Codes**:
  - `403 Forbidden`: Invalid or expired pairing code, or maximum 10 devices limit reached.
  - `429 Too Many Requests`: Exceeded 5 failed attempts/minute from this IP.

### 4.2 `GET /v1/state`
- **Auth**: `Authorization: Bearer <token>`
- **Response**: `200 OK`
  ```json
  {
    "status": "connected",
    "deviceId": "assigned-device-id",
    "capabilities": {
      "notifications": true,
      "files": true,
      "clipboard": false,
      "media": true
    }
  }
  ```
- **Error Codes**:
  - `401 Unauthorized`: Missing, malformed, or invalid bearer token.
  - `429 Too Many Requests`: Exceeded 5 failed authentication attempts/minute from this IP.

### 4.3 `DELETE /v1/me`
- **Auth**: `Authorization: Bearer <token>`
- **Action**: Authenticated paired device unregisters itself. Its token hash and record are removed from `phone-devices.json`.
- **Response**: `204 No Content`
- **Error Codes**:
  - `401 Unauthorized`: Missing or invalid bearer token.

### 4.4 `GET /v1/live` (WebSocket Upgrade)
- **Protocol**: WebSocket upgrade over HTTPS (`wss://`) using Axum's `ws` support.
- **Subprotocol**: Accepts `"dude-live-v1"` if offered by the client.
- **Auth**: Requires Bearer token in the `Authorization: Bearer <token>` header (constant-time SHA-256 hash comparison).
- **Origin Rejection**: Any request carrying an `Origin` header is rejected immediately with `403 Forbidden` (real clients are not web browsers).
- **Connection & Resource Limits**:
  - Global concurrent live connections limit: at most **12** connections in total (13th connection refused).
  - Maximum **1** live connection per device (a new connection from the same device closes the previous one with reason `"replaced"`).
  - Maximum frame size and message size: **4 KB** (4096 bytes).
  - Server ping every **20 seconds**; connection closed if no pong received after **45 seconds**.
  - 10-second timeout for the WebSocket upgrade handshake.
  - Outgoing messages from a bounded queue of **16** per connection (drops oldest state messages, never grows unbounded).
  - Closing triggers: Removing a device closes its connection with `{"t":"bye","reason":"removed"}`. Disabling the link closes connections with `{"t":"bye","reason":"disabled"}`. Server shutdown or forgetting all pairings closes with `{"t":"bye","reason":"shutdown"}`.
- **Messages** (JSON text frames, server to phone):
  - Handshake: `{"t":"hello","v":1,"serverName":"...","capabilities":{...},"time":<unix ms>}` sent immediately after upgrade, followed by current agent snapshot.
  - Agent status feed: `{"t":"agent","seq":N,"status":"idle"|"thinking"|"working"|"needs_you"|"done","project":"<folder name or empty>","step":"read"|"edit"|"bash"|null,"since":<unix ms or null>,"edits":N}`.
  - Capabilities update: `{"t":"caps","capabilities":{...}}` when capabilities change.
  - Termination: `{"t":"bye","reason":"removed"|"disabled"|"shutdown"}`.
- **Client to Server**:
  - Only WebSocket pings, pongs, and text frames `{"t":"ping"}` are accepted (server answers `{"t":"pong"}`).
  - Any other message is ignored and counted. No commands can be sent from the phone in this version.
- **Capability `agents`**:
  - If capability `"agents"` is off (`phone.capabilities.agents = false`), the live connection stays active but receives no agent activity broadcasts (only `hello`, `caps`, `bye`, and pongs; initial snapshot is sent as `idle` with an empty project).
- **Test Example (wscat)**:
  ```bash
  npx wscat -c wss://127.0.0.1:<port>/v1/live -n -H "Authorization: Bearer <token>"
  ```

---

## 5. Storage Architecture

1. **Private TLS Key**:
   - Native Windows Credential Manager under target `dude-app` with username `phone-tls-key`.
   - Never stored in plain text or written to disk.
2. **Certificate PEM**:
   - Saved as `phone-cert.pem` inside Dude's application data directory (`%APPDATA%\com.dude.app\phone-cert.pem`).
3. **Paired Devices Storage**:
   - Saved as `phone-devices.json` inside Dude's application data directory.
   - Written atomically via temporary file rename (`.tmp` -> `.json`).
   - `last_seen` timestamp updates are debounced (written at most once every 15 seconds) to eliminate disk churn.
   - Authentication tokens are never stored; only their constant-time verifiable SHA-256 hashes are persisted.
