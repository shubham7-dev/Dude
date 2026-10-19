<p align="center">
  <img src="mascot.png" alt="Dude Mascot" width="180">
</p>

<h1 align="center">Dude</h1>

<p align="center">
  Your tiny AI companion for Windows.
</p>
# Dude

A mascot AI assistant and laptop controls at the top of your Windows screen.

<!-- Demo GIF Placeholder -->
![Dude Demo](docs/images/demo.gif)

---

## Features

- **Mascot & Animations**: An expressive desktop companion that reacts in real time with interactive expressions (idle, smiling, thinking, speaking, sleeping, blinking, and eye tracking).
- **Three Quick-Access Drawers**: Click the top pill to switch between **Chat**, **Controls**, and **Agents** views without cluttering your desktop.
- **Gemini AI Chat**: Converse with Google's Gemini models (like `gemini-flash-latest`) using your personal API key. Features conversation memory, context trimming, and custom personality instructions.
- **Laptop Control Center**:
  - **Media Controls**: Live Now Playing track info, artist name, album art, play/pause, next/previous buttons, and an interactive seek bar.
  - **Volume & Brightness**: Precision sliders with an instant mute toggle and smooth WMI display brightness control.
  - **Power & Battery**: Live battery percentage, AC charging indicator, and Windows Battery Saver toggle.
  - **Quick Radios**: Instant switches for Wi-Fi (showing active network SSID), Bluetooth, Airplane Mode, and Mobile Hotspot.
- **Live Antigravity Agent View**: Real-time visualization of coding tasks in progress, tracking tool steps (Read, Edit, Bash, Done) and active code snippets across a local named pipe.
- **Comprehensive Settings**:
  - **AI**: Manage provider, model selection, API key, and persona.
  - **Appearance**: Choose from 4 Dude mascot colors (**Cream**, **Pink**, **Blue**, **Black**), adjust pill size, roundness, background opacity, and color swatches.
  - **Behavior**: Fine-tune eye tracking, blinking frequency, hover blush, and auto-collapse behavior.
  - **Startup**: Configure Windows boot startup and launch visibility.
  - **Agents**: Automated hook management for Antigravity.
  - **About**: Diagnostic info, release version, and documentation links.
- **System Integration**: Dark mode and light mode support in Settings, Windows system tray icon, custom right-click context menu, and global hotkeys (`Ctrl+Shift+M` to toggle the mascot, `Ctrl+Shift+Space` to open chat).

---

## Download

Get the latest release for Windows:

- **[Dude-Windows-setup.exe](https://github.com/shubham7-dev/Dude/releases/latest/download/Dude-Windows-setup.exe)** (NSIS Installer)
- **[Dude-Windows.msi](https://github.com/shubham7-dev/Dude/releases/latest/download/Dude-Windows.msi)** (MSI Package)

You can also browse all version assets on the [Releases](https://github.com/shubham7-dev/Dude/releases) page.

### Windows SmartScreen Note
When running the installer for the first time, Windows SmartScreen may show a prompt saying **"Windows protected your PC"**:
1. Click **More info**.
2. Click **Run anyway**.

**Why does this appear?**  
This warning appears on new releases because the executable is not signed with an expensive commercial code-signing certificate yet. Dude is completely open source—all code is inspectable in this repository.

---

## Requirements

- **Operating System**: Windows 11 (tested and supported).
- **WebView2**: Requires the Microsoft Edge WebView2 runtime (pre-installed by default on Windows 11; automatically downloaded by the installer if missing).

---

## Setup

Dude uses the Google Gemini API to power the AI assistant.

1. **Create an API key**:
   - Go to [Google AI Studio](https://aistudio.google.com/apikey).
   - Sign in with your Google account and generate a free API key.
2. **Add key to Dude**:
   - Paste the key in the initial Welcome walkthrough or navigate to **Settings > AI**.
   - Click **Test & Save**. Your key is securely stored in Windows Credential Manager.

---

## Agents Integration

To connect Dude to your local development agent:

1. Open **Settings > Agents**.
2. Click **Install** under Antigravity.
3. Dude creates a timestamped backup of `~/.gemini/config/hooks.json` and inserts only a `"dude"` hook entry without touching any other configuration.
4. Restart your Antigravity IDE so it loads the updated hooks.
5. *Before uninstalling or deleting Dude from your PC, open Settings > Agents and click **Uninstall** to cleanly remove the hook configuration.*

---

## Privacy

Dude respects your privacy:
- Zero telemetry, analytics, or background tracking scripts.
- No user accounts or registrations required.
- API keys are stored in native Windows Credential Manager.
- All laptop controls and agent event data remain entirely local on your machine.

Read the full [Privacy Policy](docs/PRIVACY.md) for details.

---

## Build from Source

### Prerequisites
- **Node.js** (v20 or higher) & **npm**
- **Rust** (stable toolchain with `x86_64-pc-windows-msvc` target)
- **C++ Build Tools**: Visual Studio Build Tools with the *Desktop development with C++* workload.

### Build Steps

1. Clone the repository:
   ```bash
   git clone https://github.com/shubham7-dev/Dude.git
   cd Dude
   ```

2. Install frontend dependencies:
   ```bash
   npm install
   ```

3. Run in development mode:
   ```bash
   npm run tauri dev
   ```

4. Compile the release installer:
   ```bash
   npm run tauri build
   ```
   The generated installers will be located in `src-tauri/target/release/bundle/`.

> [!TIP]
> If you encounter PowerShell script execution errors (such as `about_Execution_Policies`), invoke commands directly via `npm.cmd run tauri dev` or `npx.cmd tauri ...`.

---

## Credits

- **Made by**: Shubham Kale.

---

## License

- **Source Code**: Released under the [MIT License](LICENSE).
- **Brand & Mascot Artwork**: The app name "Dude", character illustrations, mascot renders, icons, and media files are **Copyright (c) 2026 Shubham Kale. All Rights Reserved.** and are **not** covered by the MIT license. See [LICENSE-ASSETS.md](LICENSE-ASSETS.md) for restrictions.
