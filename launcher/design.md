# Launcher design

Two pieces: the **installer** (`setup.exe`) and the **launcher app**.
The installer is one-shot; the launcher is the player's daily driver.

## Decision (2026-09-19): Tauri

Picked **Tauri** over C#/WPF:
- **Tiny installer** (~5–15 MB vs 60–150 MB self-contained .NET) — first impressions matter.
- **Modern UI** with web tech (gamer aesthetic is easy; XAML is not).
- **Rust backend** — fast streaming downloads, SHA-256 verify, process launch.
- **Built-in NSIS bundler** → `setup.exe` for free (replaces the hand-written `setup.nsi` below, kept for reference).
- **Built-in updater** — game + launcher updates without reinstalls.
- WebView2 is preinstalled on Win10/11 — non-issue in 2026.

Scaffold: `launcher/src-tauri/` (Rust backend: manifest fetch, resumable
download w/ progress events, SHA-256 verify, game launch) + `launcher/src/`
(frontend). Build on Windows with `tauri build`.

## setup.exe (NSIS) — installer reference

- Built with NSIS (`makensis`), scripted in `launcher/setup.nsi`.
- Installs to `%LOCALAPPDATA%\RRFlux` (user can change dir).
- Installs the **launcher only** (~small). The game payload is fetched
  by the launcher on first run — keeps the installer tiny and lets us
  update the game without rebuilding setup.exe.
- Creates Start Menu + Desktop shortcuts, writes uninstaller.

## RRFlux-Launcher — the app

### Responsibilities
1. **Sign-in** — Firebase Auth (email/password). Stores a refresh token
   securely (Windows Credential Manager), never the password.
2. **Update check** — `GET /v1/manifest/version`; if behind, downloads
   changed files from `/v1/manifest` file list.
3. **Download** — resumable, per-file sha256 verification, progress UI.
4. **Launch** — starts patched `RecRoom.exe` with args:
   `--rrflux-api=https://api.rrflux.example --session=<firebase-id-token>`
5. **Repair** — "Verify files" button re-checks hashes, redownloads bad files.

### Tech choice
| Option | Pros | Cons |
|---|---|---|
| **Tauri** (Rust + web UI) | Small binary, modern UI, single codebase | New toolchain for the team |
| **C# WinForms/WPF** | Native, easy Windows APIs, huge docs | Windows-only dev loop |
| Electron | Familiar web stack | Heavy (~150MB for a launcher) |

**Recommendation:** Tauri — tiny footprint matters for an installer-stage
app, and the UI is just HTML/CSS/JS.

### Manifest format (`GET /v1/manifest`)
```json
{
  "version": "0.1.0",
  "baseUrl": "https://dist.rrflux.example/builds/0.1.0",
  "files": [
    { "path": "RecRoom.exe", "sha256": "…", "size": 123456 },
    { "path": "GameAssembly.dll", "sha256": "…", "size": 987654321 }
  ]
}
```
Launcher diffs `files` against local hashes → downloads only what's new.

## Game payload distribution
- Patched builds are zipped per version at `dist.rrflux.example` (or any
  static host / Cloud Storage bucket).
- Only application-approved players get launcher accounts, so the payload
  URLs stay effectively private during the private phase.

## Open questions
- [ ] Final dist host (Cloud Storage vs. cheap VPS)
- [ ] Tauri vs. C# — Armin/Ripo Team call
- [ ] Auto-update the launcher itself (Tauri updater vs. manual)
