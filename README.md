

<p align="center">
  <img src="mascot.png" alt="Dude mascot" width="160">
</p>

<h1 align="center">Dude</h1>

<p align="center">
  <strong>Your tiny AI companion for Windows.</strong>
  <br>
  An animated desktop mascot, Gemini AI assistant, laptop controls, and coding-agent activity in one compact interface.
</p>

<p align="center">
  <a href="https://github.com/shubham7-dev/Dude/releases/latest"><img src="https://img.shields.io/github/v/release/shubham7-dev/Dude?label=latest%20release" alt="Latest release"></a>
  <a href="https://github.com/shubham7-dev/Dude/blob/main/LICENSE"><img src="https://img.shields.io/badge/source-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://github.com/shubham7-dev/Dude/actions/workflows/build.yml"><img src="https://github.com/shubham7-dev/Dude/actions/workflows/build.yml/badge.svg" alt="Build Check"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2011-0078D4" alt="Windows 11">
</p>

<p align="center">
  <a href="https://github.com/shubham7-dev/Dude/releases">Download</a> ·
  <a href="#features">Features</a> ·
  <a href="#installation">Installation</a> ·
  <a href="#build-from-source">Build from source</a> ·
  <a href="https://github.com/shubham7-dev/Dude/issues">Report a bug</a>
</p>

---

## Overview

Dude is a Windows desktop companion that combines an animated mascot, an AI chat assistant, quick laptop controls, and live coding-agent activity in a compact interface at the top of your screen.

Built with **Tauri 2, Rust, TypeScript, and Vite**, Dude is designed to make everyday laptop controls and AI assistance accessible without opening a full-sized application.

## Features

### Animated desktop companion

- Interactive mascot with idle, smiling, thinking, speaking, sleeping, blinking, and eye-tracking states.
- Customizable mascot appearance and pill settings.
- Compact interface designed to stay accessible while you work.

### Three quick-access views

Switch between three views from the top pill:

- **Chat** — talk to your AI assistant.
- **Controls** — access common laptop controls.
- **Agents** — view coding-agent activity.

### Gemini AI assistant

- Connect to Google's Gemini models using your own API key.
- Conversation memory and context trimming.
- Custom personality instructions.
- Configure the provider, model, API key, and persona in Settings.

### Laptop control center

- **Media:** track information, artist, album artwork, playback controls, and seek bar.
- **Volume and brightness:** adjust volume, mute audio, and control display brightness.
- **Power and battery:** battery percentage, AC charging status, and Battery Saver toggle.
- **Quick radios:** Wi-Fi, Bluetooth, Airplane Mode, and Mobile Hotspot controls.

Hardware and Windows support can vary by device.

### Coding-agent integration

- Live visualization of Antigravity agent activity.
- Track tool steps such as Read, Edit, Bash, and Done.
- View active code snippets and events received through a local named pipe.
- Manage Antigravity hook integration from Settings.

### Customization and system integration

- Cream, Pink, Blue, and Black mascot color options.
- Adjust pill size, roundness, background opacity, and color swatches.
- Configure eye tracking, blinking, hover blush, and auto-collapse behavior.
- Configure Windows startup and launch visibility.
- Windows system tray icon and context menu.
- Global shortcuts:
  - `Ctrl + Shift + M` — toggle the mascot.
  - `Ctrl + Shift + Space` — open chat.
- Dark and light themes in Settings.

## Screenshots and demo

Screenshots and a demo video will be added here in a future update.

## Installation

**Requirements:** Windows 11. The installer requires the Microsoft Edge WebView2 runtime, which is normally available on Windows 11.

1. Open the [Dude Releases page](https://github.com/shubham7-dev/Dude/releases).
2. Select the release you want.
3. Download the Windows installer:
   - **NSIS installer:** `Dude_0.3.0_x64-setup.exe`
   - **MSI package:** `Dude_0.3.0_x64_en-US.msi`
4. Run the installer and follow the on-screen instructions.

For the latest available version, use [Latest Release](https://github.com/shubham7-dev/Dude/releases/latest).

### Windows SmartScreen

Windows SmartScreen may display a warning when you first run an installer that is not signed with a recognized code-signing certificate.

Only proceed if you obtained the installer from the official [Dude Releases page](https://github.com/shubham7-dev/Dude/releases) and trust the source. You can review the source code in this repository.

### Verify installer checksums

The repository includes a [`SHA256SUMS`](SHA256SUMS) file containing SHA-256 hashes for the v0.3.0 installers.

In PowerShell, you can calculate an installer's hash with:

```powershell
Get-FileHash .\Dude_0.3.0_x64-setup.exe -Algorithm SHA256
```

Compare the resulting hash with the matching entry in `SHA256SUMS`. The file and checksum should come from a trusted release source.

## Initial setup

Dude uses the Google Gemini API for its AI assistant.

1. Open [Google AI Studio](https://aistudio.google.com/apikey).
2. Sign in and create an API key.
3. Open Dude's welcome walkthrough or go to **Settings → AI**.
4. Enter your API key and click **Test & Save**.

According to the project configuration, the API key is stored in Windows Credential Manager.

## Antigravity agent integration

To connect Dude to your local development agent:

1. Open **Settings → Agents**.
2. Click **Install** under Antigravity.
3. Dude creates a timestamped backup of `~/.gemini/config/hooks.json` and adds its hook entry.
4. Restart Antigravity so it loads the updated hooks.
5. Before uninstalling Dude, open **Settings → Agents** and click **Uninstall** to remove the hook configuration.

Keep a backup of important configuration files before changing integrations.

## Privacy

Dude's project documentation describes the following privacy goals:

- No telemetry or analytics.
- No user account or registration required.
- API keys stored in Windows Credential Manager.
- Laptop controls and agent event data kept locally on your machine.

Read the [Privacy Policy](docs/PRIVACY.md) for more information.

## Build from source

### Prerequisites

- Node.js 20 or newer and npm.
- Rust stable toolchain.
- `x86_64-pc-windows-msvc` Rust target.
- Visual Studio Build Tools with the **Desktop development with C++** workload.
- Microsoft Edge WebView2 runtime.

### Clone the repository

```powershell
git clone https://github.com/shubham7-dev/Dude.git
cd Dude
```

### Install dependencies

```powershell
npm install
```

If PowerShell blocks `npm.ps1`, use the Windows command wrapper:

```powershell
npm.cmd install
```

### Run in development mode

```powershell
npm run tauri dev
```

Or, if required by your PowerShell setup:

```powershell
npm.cmd run tauri dev
```

### Build the application

```powershell
npm run tauri build
```

Or:

```powershell
npm.cmd run tauri build
```

Generated installer bundles are placed under:

```text
src-tauri/target/release/bundle/
```

### Frontend build check

To run the frontend TypeScript and Vite production build:

```powershell
npm run build
```

Dude also has a GitHub Actions workflow that runs the frontend build check on pushes and pull requests targeting `main`.

## Contributing

Contributions, bug reports, and feature ideas are welcome.

- [Report a bug](https://github.com/shubham7-dev/Dude/issues/new?template=bug_report.md)
- [Request a feature](https://github.com/shubham7-dev/Dude/issues/new?template=feature_request.md)
- Open a [pull request](https://github.com/shubham7-dev/Dude/compare)

Please describe the problem, the proposed change, and any relevant testing. For UI changes, include screenshots when possible.

## Project structure

```text
Dude/
├── .github/          # Issue templates and GitHub Actions
├── assets/           # Project assets
├── docs/             # Documentation
├── src/              # TypeScript frontend
├── src-tauri/        # Rust backend and Tauri configuration
├── index.html        # Main application entry
├── menu.html         # Menu entry
├── settings.html     # Settings entry
├── package.json      # Scripts and dependencies
├── vite.config.ts    # Vite configuration
└── README.md
```

## Credits

Made by **Shubham Kale**.

## License

- **Source code:** [MIT License](LICENSE).
- **Brand and mascot artwork:** The app name "Dude", character illustrations, mascot renders, icons, and media files are Copyright (c) 2026 Shubham Kale. All Rights Reserved. These assets are not covered by the MIT license. See [LICENSE-ASSETS.md](LICENSE-ASSETS.md) for restrictions.

---

<p align="center">
  Made with care for a more helpful Windows desktop.
</p>