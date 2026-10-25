# Third-Party Notices & Licenses

Dude bundles or loads the following third-party fonts, icon sets, and libraries. This document provides attribution, source links, and corresponding license terms for each component.

---

## 1. Fonts

The following typefaces are utilized by the application interface:

### Inter
- **Source**: [Google Fonts - Inter](https://fonts.google.com/specimen/Inter) | [GitHub Repository](https://github.com/rsms/inter)
- **Author**: Rasmus Andersson
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

### Montserrat
- **Source**: [Google Fonts - Montserrat](https://fonts.google.com/specimen/Montserrat) | [GitHub Repository](https://github.com/JulietaUla/Montserrat)
- **Author**: Julieta Ulanovsky, Sol Matas, Juan Pablo del Peral, Jacques Le Bailly
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

### JetBrains Mono
- **Source**: [Google Fonts - JetBrains Mono](https://fonts.google.com/specimen/JetBrains+Mono) | [GitHub Repository](https://github.com/JetBrains/JetBrainsMono)
- **Author**: JetBrains
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

### Noto Sans Devanagari
- **Source**: [Google Fonts - Noto Sans Devanagari](https://fonts.google.com/specimen/Noto+Sans+Devanagari) | [GitHub Repository](https://github.com/notofonts/devanagari)
- **Author**: Google
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

### Fira Sans & Fira Sans Condensed
- **Source**: [Google Fonts - Fira Sans](https://fonts.google.com/specimen/Fira+Sans) | [GitHub Repository](https://github.com/bBoxType/FiraSans)
- **Author**: The Mozilla Foundation, Carrois Apostrophe
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

### Rubik
- **Source**: [Google Fonts - Rubik](https://fonts.google.com/specimen/Rubik) | [GitHub Repository](https://github.com/googlefonts/rubik)
- **Author**: Hubert & Fischer, Meir Sadan, Cyreal
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

### IBM Plex Mono
- **Source**: [Google Fonts - IBM Plex Mono](https://fonts.google.com/specimen/IBM+Plex+Mono) | [GitHub Repository](https://github.com/IBM/plex)
- **Author**: Mike Abbink, Bold Monday
- **License**: [SIL Open Font License 1.1 (OFL)](https://openfontlicense.org/)

---

## 2. Icon Sets & Artwork

### Agents Section Backgrounds (`assets/agents-banner-light.webp`, `assets/agents-banner-dark.webp`, `assets/agents-hooks-light.webp`, `assets/agents-hooks-dark.webp`)
- **source and license: to be filled by the owner**

### Startup Section Backgrounds (`assets/startup-banner-light.webp`, `assets/startup-banner-dark.webp`, `assets/startup-card-light.webp`, `assets/startup-card-dark.webp`)
- **source and license: to be filled by the owner**

### Behavior Section Backgrounds (`assets/behavior-banner-light.png`, `assets/behavior-banner-dark.png`)
- **source and license: to be filled by the owner**

### Appearance Section Backgrounds (`assets/appearance-banner-both.png`, `assets/appearance-tree.webp`, `assets/appearance-tree.png`)
- **source and license: to be filled by the owner**

### AI Section Backgrounds (`assets/ai-banner-both.png`)
- **source and license: to be filled by the owner**

### About Section Background (`assets/about-banner.webp`)
- **source and license: to be filled by the owner**

### Character Artwork & Brand Assets ("Dude")
- **Source**: `assets/` and `src-tauri/icons/`
- **Copyright**: Copyright (c) 2026. All Rights Reserved.
- **Notice**: The mascot character design, character name ("Dude"), logos, and application icons are proprietary and are **not** covered by the open-source MIT license. See [LICENSE-ASSETS.md](../LICENSE-ASSETS.md).

### Application UI Icons
- **Source**: Hand-crafted inline SVG paths created specifically for Dude's top pill, control center sliders, media transport, and settings views.
- **License**: Covered under the project's [MIT License](../LICENSE).

---

## 3. Frontend Libraries & Dependencies

| Package | Version | Source | License |
| :--- | :--- | :--- | :--- |
| `@tauri-apps/api` | `^2` | [tauri-apps/tauri](https://github.com/tauri-apps/tauri) | MIT OR Apache-2.0 |
| `@tauri-apps/plugin-autostart` | `^2.7.0` | [tauri-apps/plugins-workspace](https://github.com/tauri-apps/plugins-workspace) | MIT OR Apache-2.0 |
| `@tauri-apps/plugin-global-shortcut` | `^2.4.1` | [tauri-apps/plugins-workspace](https://github.com/tauri-apps/plugins-workspace) | MIT OR Apache-2.0 |
| `@tauri-apps/plugin-opener` | `^2` | [tauri-apps/plugins-workspace](https://github.com/tauri-apps/plugins-workspace) | MIT OR Apache-2.0 |
| `@tauri-apps/plugin-store` | `^2.5.0` | [tauri-apps/plugins-workspace](https://github.com/tauri-apps/plugins-workspace) | MIT OR Apache-2.0 |

---

## 4. Rust Backend Dependencies

| Crate | Version | Source | License |
| :--- | :--- | :--- | :--- |
| `tauri` | `2` | [crates.io/crates/tauri](https://crates.io/crates/tauri) | MIT OR Apache-2.0 |
| `tauri-plugin-opener` | `2` | [crates.io/crates/tauri-plugin-opener](https://crates.io/crates/tauri-plugin-opener) | MIT OR Apache-2.0 |
| `tauri-plugin-store` | `2.5.0` | [crates.io/crates/tauri-plugin-store](https://crates.io/crates/tauri-plugin-store) | MIT OR Apache-2.0 |
| `tauri-plugin-autostart` | `2` | [crates.io/crates/tauri-plugin-autostart](https://crates.io/crates/tauri-plugin-autostart) | MIT OR Apache-2.0 |
| `tauri-plugin-global-shortcut` | `2` | [crates.io/crates/tauri-plugin-global-shortcut](https://crates.io/crates/tauri-plugin-global-shortcut) | MIT OR Apache-2.0 |
| `serde` | `1` | [crates.io/crates/serde](https://crates.io/crates/serde) | MIT OR Apache-2.0 |
| `serde_json` | `1` | [crates.io/crates/serde_json](https://crates.io/crates/serde_json) | MIT OR Apache-2.0 |
| `reqwest` | `0.12` | [crates.io/crates/reqwest](https://crates.io/crates/reqwest) | MIT OR Apache-2.0 |
| `tokio` | `1` | [crates.io/crates/tokio](https://crates.io/crates/tokio) | MIT |
| `windows` | `0.58` | [crates.io/crates/windows](https://crates.io/crates/windows) | MIT OR Apache-2.0 |
| `wmi` | `0.14` | [crates.io/crates/wmi](https://crates.io/crates/wmi) | MIT |
| `base64` | `0.22` | [crates.io/crates/base64](https://crates.io/crates/base64) | MIT OR Apache-2.0 |
| `keyring` | `4.2.0` | [crates.io/crates/keyring](https://crates.io/crates/keyring) | MIT OR Apache-2.0 |
| `log` | `0.4` | [crates.io/crates/log](https://crates.io/crates/log) | MIT OR Apache-2.0 |

---

## 5. Build & Development Tools

| Tool | Version | Source | License |
| :--- | :--- | :--- | :--- |
| `vite` | `^8.0.16` | [vitejs/vite](https://github.com/vitejs/vite) | MIT |
| `typescript` | `~6.0.3` | [microsoft/TypeScript](https://github.com/microsoft/TypeScript) | Apache-2.0 |
| `@tauri-apps/cli` | `^2` | [tauri-apps/tauri](https://github.com/tauri-apps/tauri) | MIT OR Apache-2.0 |
| `tauri-build` | `2` | [crates.io/crates/tauri-build](https://crates.io/crates/tauri-build) | MIT OR Apache-2.0 |
