# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.3.1] - 2026-10-25

### Added
- Standardized `.gitattributes` and `.editorconfig` for cross-platform repository cleanliness.
- Technical documentation suite: [CONTRIBUTING.md](CONTRIBUTING.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), and [docs/RELEASE_GUIDE.md](docs/RELEASE_GUIDE.md).
- Automated Windows release workflow `.github/workflows/release.yml`.
- Inline visual API test badge in Settings for instant connection status.

### Changed
- **Gemini AI Engine**:
  - Upgraded default model from deprecated/high-load `gemini-flash-latest` to high-speed `gemini-3.5-flash`.
  - Re-ordered model preference list prioritizing modern Gemini 3.5 and Gemini 3 models (`gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-3-flash-preview`).
  - Added automatic fallback to `gemini-3.5-flash` on HTTP 404/503 errors.
  - Optimized connection test payload (`maxOutputTokens: 5`) with 8-second timeout for ~1.4s response times.
- **Application Icons & Visual Assets**:
  - Completely updated high-resolution squircle mascot icon with clean anti-aliased transparency.
  - Rebuilt Windows ICO, macOS ICNS, tray icons, Windows Store tiles, iOS, and Android mipmap icon bundles.
- **Settings UI**:
  - Added 350ms debounced auto-save on API key input, plus Enter and blur save triggers.
  - Non-blocking background model list refresh upon API key save.

### Fixed
- **Agent Hook Protocol**: Updated `dude-hook.rs` to format `hookSpecificOutput` with `hookEventName`, `permissionDecision`, and `permissionOverrides` for compatibility with modern CLI agents (such as Claude Code v0.2.x+).
- **Gemini Test Latency**: Eliminated 10s+ timeouts and 503 errors when saving and verifying Gemini API keys.

---

## [0.3.0] - 2026-10-21

### Added
- Support for Claude Code and Gemini Code Assist hook integrations in `agent_hooks.rs` and `dude-hook.rs`.
- Interactive sidebar mascot animations and appearance asset preloading in Settings.
- Compact approval card layout and action states in the floating widget.

### Changed
- Refined agent receiver Named Pipe IPC handler for robust payload parsing.
- Updated project branding and mascot imagery in documentation.

---

## [0.2.0] - 2026-10-18

### Added
- Standalone CLI agent hook binary (`dude-hook.exe`) for capturing agent lifecycle events.
- Windows Named Pipe IPC communication (`\\.\pipe\dude-agent-events`).
- Interactive tool approval UI in the floating desktop widget.
- Backend Tauri command execution hooks for allow/reject tool decisions.
- Initial multi-agent hook configuration manager.

---

## [0.1.0] - 2026-10-17

Initial public release of **Dude** for Windows.

### Added

#### Mascot & Desktop Companion
- Minimalist floating pill widget anchored to the top edge of your Windows screen.
- Expressive mascot character with smooth animations and states: idle, smiling, blinking, thinking, speaking, sleeping, and eye tracking.
- Four distinct mascot body color options: Cream (default), Pink, Blue, and Black.
- Custom placement presets (Top Center, Top Left, Top Right) with custom X/Y coordinate positioning.
- Right-click desktop context menu window with quick controls (Open Settings, Hide / Show Widget, Quit).
- System tray integration with left-click toggle and right-click context menu.

#### Laptop Control Center
- **Media Controller**: Real-time integration with Windows System Media Transport Controls displaying track title, artist, album art, play/pause, next/previous controls, and interactive seek bar.
- **Audio & Mute**: System volume slider and instant master mute toggle.
- **Display Brightness**: Smooth brightness slider powered by Windows WMI CIM.
- **Battery Hub**: Live battery percentage, AC power connection badge, and Windows Battery Saver toggle.
- **Connectivity Switches**: Instant toggles for Wi-Fi (displaying active SSID), Bluetooth, Airplane Mode, and Mobile Hotspot.
- **System Utilities**: Fast triggers to Lock Workstation, put the PC to Sleep, and launch the Windows Snipping Tool (`ms-screenclip:`).
- **Windows Theme Switcher**: One-click toggle between Windows Dark Mode and Light Mode.

#### Integrated Gemini AI Assistant
- Slide-out Chat drawer accessible directly from the floating pill.
- Powered by official Google Gemini models (including `gemini-flash-latest`) using your personal API key.
- Dynamic model listing based on your Google AI Studio account permissions.
- Multi-turn conversation history management with automatic token trimming.
- Custom AI persona configuration and personality instructions in Settings.
- Key security: API key stored exclusively in Windows Credential Manager.

#### Real-Time Agents Integration
- Live Agents tab monitoring AI coding tasks in your development environment.
- Step progression tracker displaying active tools (Read, Edit, Bash, Done) with status badges (running, done, pending).
- Live code component displaying file names, line numbers, and tool command execution snippets.
- Dedicated Agents hook manager in Settings:
  - One-click Install, Reinstall, and Uninstall for Google Antigravity.
  - Safe automated editing of `~/.gemini/config/hooks.json` with timestamped backups.
  - Bundled release binary `dude-hook.exe` deploying to `C:\ProgramData\Dude\hooks\dude-hook.exe` with local Named Pipe communication (`\\.\pipe\dude-agent-events`).

#### Settings & Desktop Features
- Multi-section Settings panel covering AI, Appearance, Behavior, Startup, Agents, and About.
- Built-in Dark Mode and Light Mode for the Settings interface.
- Appearance tuning: Pill width/height, background color presets, corner roundness, and opacity sliders.
- Behavior toggles: Eye tracking, blinking, blush on hover, auto-collapse, and animation speeds.
- Startup configuration: Windows login autostart and mascot visibility on boot.
- Global keyboard shortcuts: `Ctrl+Shift+M` to toggle mascot visibility and `Ctrl+Shift+Space` to summon chat.
- Packaging: MSI and NSIS installers with bundled release resources.
