# Dude Release Guide

This guide details the standard release process for **Dude** on Windows, including version increments, local installer packaging, checksum generation, and release artifact verification.

---

## 1. Release Checklist

Before releasing a new version:

- [ ] All tests, type checks, and linting pass (`npm run build`, `cargo check`).
- [ ] No `.env`, secret keys, or test credentials exist in tracked files or staging areas.
- [ ] `CHANGELOG.md` is updated with verified changes under `[X.Y.Z] - YYYY-MM-DD`.
- [ ] Version numbers are synchronized across:
  - `package.json` (`"version": "X.Y.Z"`)
  - `src-tauri/Cargo.toml` (`version = "X.Y.Z"`)
  - `src-tauri/tauri.conf.json` (`"version": "X.Y.Z"`)
- [ ] `LICENSE` and `LICENSE-ASSETS.md` remain intact.

---

## 2. Building Installers Locally

To produce the production Windows binaries and installers:

```bash
# 1. Typecheck and build frontend assets
npm run build

# 2. Build Tauri release binaries and bundle installers
npm run tauri build
```

Generated installer packages will be placed in:
- NSIS Installer: `src-tauri/target/release/bundle/nsis/Dude_<version>_x64-setup.exe`
- MSI Installer: `src-tauri/target/release/bundle/msi/Dude_<version>_x64_en-US.msi`
- Hook CLI Binary: `src-tauri/target/release/dude-hook.exe`

---

## 3. Checksums & Verification

Always generate SHA-256 hashes for release installer binaries. For each release, create a dedicated `SHA256SUMS` file inside the release artifact folder:

```powershell
Get-FileHash Dude_<version>_x64-setup.exe -Algorithm SHA256
Get-FileHash Dude_<version>_x64_en-US.msi -Algorithm SHA256
```

Format of `SHA256SUMS`:
```
<UPPERCASE_SHA256_HASH>  Dude_<version>_x64-setup.exe
<UPPERCASE_SHA256_HASH>  Dude_<version>_x64_en-US.msi
```

> **Note**: Do not overwrite the root `SHA256SUMS` file that tracks previous milestone releases. Store release-specific checksums in `release-<version>/SHA256SUMS`.

---

## 4. Release Packaging & Tagging

1. **Create Release Directory**:
   Assemble all installers and checksum files in `release-<version>/`:
   ```
   release-<version>/
   ├── Dude_<version>_x64-setup.exe
   ├── Dude_<version>_x64_en-US.msi
   ├── SHA256SUMS
   └── RELEASE_NOTES.md
   ```

2. **Git Commit & Tag**:
   Once verified and approved:
   ```bash
   git add package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json CHANGELOG.md README.md
   git commit -m "chore(release): v<version>"
   git tag -a "v<version>" -m "Release v<version>"
   ```

3. **Publishing**:
   Push the tag to trigger the GitHub Actions release pipeline, or manually create a GitHub Release attaching the `.exe`, `.msi`, and `SHA256SUMS` files.
