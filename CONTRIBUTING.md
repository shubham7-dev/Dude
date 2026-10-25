# Contributing to Dude

Thank you for your interest in contributing to **Dude**! We welcome community contributions, bug reports, and suggestions to make Dude the best floating companion and laptop control widget for Windows.

---

## Code of Conduct & Asset Notice

- **Code License**: Dude's software source code is open source under the [MIT License](LICENSE).
- **Brand & Character Assets**: The mascot character design, artwork, icons, and name ("Dude") are proprietary and protected under [LICENSE-ASSETS.md](LICENSE-ASSETS.md). Please do not submit PRs replacing or modifying character assets without prior discussion.

---

## Development Environment Setup

### Prerequisites

1. **Node.js**: v18.0.0 or later (v20+ or v22 LTS recommended)
2. **Rust & Cargo**: Latest stable channel (`rustup update stable`)
3. **C++ Build Tools**: Visual Studio Build Tools 2022 with the "Desktop development with C++" workload
4. **Platform**: Windows 10/11 (64-bit)

### Installation

Clone the repository and install frontend dependencies:

```bash
git clone https://github.com/shubham7-dev/Dude.git
cd Dude
npm install
```

### Running in Development

To start the Vite dev server alongside the Tauri desktop runtime:

```bash
npm run tauri dev
```

This launches the floating mascot pill on your screen. You can right-click the mascot to open Settings or exit.

---

## Project Architecture

Dude uses a multi-page Tauri 2 architecture:

- `index.html`: Main transparent desktop floating pill widget, laptop control center, agent monitoring drawer, and chat assistant.
- `settings.html`: Dedicated multi-section configuration window (AI, Appearance, Behavior, Startup, Agents, About).
- `menu.html`: Fast lightweight desktop context menu popup.
- `src-tauri/src/lib.rs`: Tauri command handlers, Windows API integrations (brightness, media, volume, power, Wi-Fi), and Gemini AI client.
- `src-tauri/src/agent_hooks.rs`: Named pipe IPC listener and agent event routing.
- `src-tauri/src/bin/dude-hook.rs`: Lightweight CLI binary deployed to handle agent hook execution events.

---

## Code Style & Guidelines

- **HTML / CSS / JavaScript**:
  - Keep styling consistent with the existing theme tokens in `index.html` and `settings.html`.
  - Use Vanilla CSS and standard Web APIs where possible.
  - Follow the 2-space indentation rule configured in `.editorconfig`.
- **Rust**:
  - Format Rust code with `cargo fmt`.
  - Run `cargo clippy` and ensure zero warnings or errors.
  - Keep command handlers modular and asynchronous where network or I/O is involved.

---

## Submitting Pull Requests

1. **Fork the repository** and create your branch from `main`:
   ```bash
   git checkout -b feature/your-feature-name
   ```
2. **Commit your changes**:
   - Write clear, concise commit messages following conventional commits (`feat: ...`, `fix: ...`, `docs: ...`).
   - Do NOT commit sensitive data, `.env` files, or API keys.
3. **Verify the build locally**:
   ```bash
   npm run build
   cd src-tauri && cargo check && cd ..
   ```
4. **Open a Pull Request**:
   - Describe the problem solved or feature added.
   - Attach screenshots or recordings if UI changes are included.

Thank you for helping make Dude awesome!
