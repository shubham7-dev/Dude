# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
