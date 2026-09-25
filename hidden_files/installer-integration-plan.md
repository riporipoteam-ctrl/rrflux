# Flux Rec installer integration plan — Goldberg emulator (v0.1.7)

**Status:** PLAN ONLY. Do not build, do not publish, do not ship to Armin until the
runtime proof (PLAY or CREATE ACCOUNT screenshot from the Windows cloud run) exists
**and** Armin explicitly approves a release.

**Baseline:** the last known-good installer is tag `recflare-installer-v0.1.3`
(in `riporipoteam-ctrl/rrflux`). Current tree is 0.1.6. The working source of truth
is `~/workspace/fluxrec-work/game/installer/`.

---

## 1. What changed between v0.1.3 (known-good) and current (0.1.6)

Verified by `git diff recflare-installer-v0.1.3` against the working tree
(`game/installer/src/main.rs`). Three behavioral changes plus one addition:

| # | Change | Introduced | Action for v0.1.7 |
|---|--------|------------|-------------------|
| 1 | `write_bepinex_config()` **removed** (the `[Logging.Console] Enabled = false` config that hides the BepInEx console window) and its call at step 4b removed | v0.1.4+ | **RESTORE exactly as in v0.1.3** |
| 2 | Shortcut args changed from `"+forcemode:screen -screen-fullscreen 1"` to `"+forcemode:screen"` (both Start Menu and desktop) | v0.1.4+ | **RESTORE the fullscreen args** |
| 3 | Logo bundle integration **added** (step 1b): `LOGO_BUNDLE_*` consts, `install_logo_bundle()`, `apply_logo_bundle()`, 3 logo unit tests, `flate2` gunzip logic, ~39MB HF download per install | v0.1.6 | **REMOVE entirely** (unproven loading-screen patch is excluded from the recovery baseline by standing order) |
| 4 | Steam bypass hardened but still wrong: writes root `steam_appid.txt = "480"` in two fail-soft steps (1a + 5) | v0.1.5/0.1.6 | **REPLACE with Goldberg emulator** (see §2) |

**Unchanged since v0.1.3 (keep as-is):**
- `fluxrec.ico` — MD5 `e0a9ce7282c6e748c3f688e31e0f8526` at both v0.1.3 and current. Do not touch.
- `fluxrec.nsi` — legacy file from the old launcher pipeline; not used by the RecFlare pipeline (which ships `FluxRec-Setup.exe` straight from `cargo build --release` via `.github/workflows/build-recflare-installer.yml`). Leave untouched.
- No-wipe behavior: `run()` uses `create_dir_all` + idempotent `download()` (skips when `file_ok`, re-downloads only on hash/size mismatch). There is **no** directory-wipe code in this installer. Keep it that way — never add one.
- Client/plugin/BepInEx download URLs, MD5s, sizes, plugin config layout, `--dir`/`--ns-host`/Photon CLI args, fail-soft plugin/config/shortcut steps, final `[verify]` report. Keep.

---

## 2. Goldberg emulator integration (replaces root `steam_appid.txt = 480`)

### 2a. Validated Goldberg source (pinned — do not float to latest)

- Release: `https://github.com/Detanup01/gbe_fork/releases/download/release-2026_09_16_2/emu-win-release-vs22.7z`
- 7z: size `13,465,728` bytes, MD5 `e8478032313f69d843f906d7705afced`,
  SHA256 `d311deadc2a8a8aed620fe66976646059388123587aa22d408f723c592fc9688`
- Inner file: `release/regular/x64/steam_api64.dll`
  - size `11,429,800` bytes, MD5 `e8462e013a5caea09c8abc6c3f532ede`,
    SHA256 `e853944da54eecb379db29a8b71015233e05dc1e8f310a90b57e839f16482c94`
- This exact DLL + `steam_settings/steam_appid.txt = 471710` + the pinned
  `steam_interfaces.txt` passed `SteamAPI_Init → 1` in the harness and booted the
  game on the real Windows runner (the "Login failed" seen there was the auth-worker
  ticket issue, now fixed server-side — unrelated to Steam init).

### 2b. Delivery mechanism — embed the DLL in the setup binary

Recommended: commit the extracted DLL to `game/installer/assets/steam_api64.dll`
and embed it with `include_bytes!`. Rationale: zero-touch install must not depend
on 7-Zip being present on the user's PC (the 7z cannot be extracted with std
tooling), and downloading an 11MB DLL from GitHub at install time adds a failure
mode the v0.1.3 design deliberately avoided for critical files. +11.4MB on
`FluxRec-Setup.exe` is acceptable.

The asset must be hash-verified at packaging/CI time against §2a before commit.
`steam_interfaces.txt` (already at `game/installer/assets/steam_interfaces.txt`,
MD5 `8662f8adf8bb70a2a9817ca51da62617`, 29 lines) is also embedded.

### 2c. New installer step — "Steam bypass (Goldberg emulator)"

In `run()`, replace both `steam_appid.txt = "480"` writes (old steps 1a and 5) with:

1. `plugin_dir = dir.join("RecRoom_Data").join("Plugins").join("x86_64")`
   (matches the client zip layout; the screenshot-test workflow used
   `D:\FluxRec\RecRoom_Data\Plugins\x86_64`).
2. If `plugin_dir/steam_api64.dll` exists **and** its MD5 is not the Goldberg hash
   (§2a), copy it to `plugin_dir/steam_api64.dll.orig` (never overwrite an existing
   `.orig`). If its MD5 already equals the Goldberg hash, skip the backup and the
   rewrite (idempotent re-runs).
3. Write the embedded Goldberg DLL to `plugin_dir/steam_api64.dll`; re-hash after
   write and fail the step (fail-soft WARNING, see §4) on mismatch.
4. `create_dir_all(plugin_dir/steam_settings)`; write exactly `471710` with **no
   trailing newline** to `steam_settings/steam_appid.txt`; write the embedded
   `steam_interfaces.txt` to `steam_settings/steam_interfaces.txt` and verify its
   MD5 (`8662f8adf8bb70a2a9817ca51da62617`).
5. Delete `dir/steam_appid.txt` if present (the misleading root file from older
   installs — the real Steam client never reads it, Goldberg never reads it).
6. Add all of the above to the final `[verify]` checklist (see §5).

Fail-soft (WARNING + continue), consistent with the 0.1.5/0.1.6 philosophy that a
transient filesystem hiccup must never leave the game half-installed — but the
`[verify]` step must report Steam files as missing so a broken bypass is never silent.

### 2d. Expected installed file layout (relative to install dir, default `C:\Games\FluxRec`)

| Path | Action | Expected content/hash |
|------|--------|----------------------|
| `RecRoom_Data/Plugins/x86_64/steam_api64.dll` | **replaced** with Goldberg | MD5 `e8462e013a5caea09c8abc6c3f532ede`, 11,429,800 bytes |
| `RecRoom_Data/Plugins/x86_64/steam_api64.dll.orig` | **added** (stock backup, once) | original client DLL, hash differs from Goldberg |
| `RecRoom_Data/Plugins/x86_64/steam_settings/steam_appid.txt` | **added** | exactly `471710`, no trailing newline |
| `RecRoom_Data/Plugins/x86_64/steam_settings/steam_interfaces.txt` | **added** | pinned file, MD5 `8662f8adf8bb70a2a9817ca51da62617` |
| `steam_appid.txt` (install root) | **removed** if present | must not exist after install |
| `BepInEx/config/BepInEx.cfg` | **restored** (v0.1.3) | contains `[Logging.Console]` / `Enabled = false` |

---

## 3. Exact code edits for v0.1.7 (`game/installer/src/main.rs`)

Base the edits on the current `main.rs`; net effect must equal
"v0.1.3 semantics + Goldberg, minus logo bundle":

1. **Restore** `write_bepinex_config()` verbatim from v0.1.3 (writes
   `BepInEx/config/BepInEx.cfg` with the header comment and
   `[Logging.Console]\nEnabled = false`), and **restore** its call as step 4b in
   `run()` between `write_plugin_config` and the Steam step.
2. **Restore** shortcut args to `"+forcemode:screen -screen-fullscreen 1"` in
   `create_shortcuts()` (both Start Menu and desktop calls; keep the non-Windows
   stub call consistent).
3. **Remove** the entire logo integration: `LOGO_BUNDLE_URL/MD5/SIZE/NAME`,
   `LOGO_BUNDLE_UNZIPPED_SIZE/MD5`, `install_logo_bundle()`,
   `apply_logo_bundle()`, the step-1b call in `run()`, the `1b.` header comment
   lines, and the three `logo_install_*` unit tests. (`flate2` becomes unused —
   drop it from `Cargo.toml` dependencies to keep the build clean.)
4. **Replace** the two `steam_appid.txt = "480"` blocks (old steps 1a and 5) with
   the Goldberg step from §2c. Update the header comment block at the top of the
   file (flow steps 5–6) to describe the Goldberg step.
5. **Embed assets**: `const GBE_DLL: &[u8] = include_bytes!("../assets/steam_api64.dll");`
   and `const STEAM_INTERFACES: &str = include_str!("../assets/steam_interfaces.txt");`
   Build-time assert the DLL hash in a unit test (fail the build if the pinned
   asset drifts from §2a).
6. Update the `user_agent` string (`FluxRec-Setup/0.1.0` → match the new version).
7. Bump `game/installer/Cargo.toml`: `version = "0.1.6"` → `"0.1.7"`.

**Do not** change: client/BepInEx/plugin URLs and hashes, plugin config format,
CLI args, no-wipe/idempotent download logic, `fluxrec.ico`, `fluxrec.nsi`.

---

## 4. Licensing — Goldberg (gbe_fork) is LGPL-3.0

GitHub reports the `Detanup01/gbe_fork` repository license as
**LGPL-3.0** (verified 2026-09-23 via the repo license API). Redistributing
`steam_api64.dll` inside the installer therefore requires, before any release:

- Attribution to the Goldberg emulator authors in the repo (e.g.
  `game/installer/assets/ATTRIBUTION.md` + full LGPL-3.0 text).
- A note in the release notes linking the pinned upstream release and its source.
- Confirmation that dynamic replacement (DLL swap, not static linking) keeps the
  installer itself outside LGPL's copyleft scope — get this reviewed, do not
  assume. If in doubt, fall back to download-at-install-time from the pinned
  upstream URL instead of embedding.

Do not ship v0.1.7 until the attribution files exist and this review is recorded.

---

## 5. Verification checklist (clean Windows environment, real or cloud runner)

Run the built `FluxRec-Setup.exe` on a clean machine/VM (no prior FluxRec dir),
then check:

- [ ] Install completes with exit 0 and `[verify] all critical files present`.
- [ ] `RecRoom_Data/Plugins/x86_64/steam_api64.dll` MD5 = `e8462e013a5caea09c8abc6c3f532ede`
- [ ] `steam_api64.dll.orig` exists and is the stock DLL (hash ≠ Goldberg).
- [ ] `steam_settings/steam_appid.txt` is exactly `471710` (no trailing newline).
- [ ] `steam_settings/steam_interfaces.txt` MD5 = `8662f8adf8bb70a2a9817ca51da62617`.
- [ ] No `steam_appid.txt` in the install root.
- [ ] `BepInEx/config/BepInEx.cfg` contains `[Logging.Console]` / `Enabled = false`.
- [ ] Both shortcuts (Start Menu + desktop) carry `+forcemode:screen -screen-fullscreen 1`.
- [ ] No logo-bundle files: no `logo-bundle.gz`, no `*.bundle.stock` in
      `RecRoom_Data/StreamingAssets/aa/StandaloneWindows64/`, and the UI bundle
      `682ba40059cd6c037bace975e7aea07f.bundle` is the stock client hash.
- [ ] The old game directory is untouched apart from additions above (no wipe:
      drop a sentinel file in the install dir before re-running setup, confirm it
      survives).
- [ ] Steam harness: `SteamAPI_Init()` returns `1` against the installed
      `steam_api64.dll` (same check that validated the fix on 2026-09-22).
- [ ] Full `game-screenshot-test` workflow against a v0.1.7-installed client
      reaches the account menu (PLAY or CREATE ACCOUNT) — the same bar as the
      current runtime proof.

---

## 6. Release mechanics (only after proof + Armin's explicit go-ahead)

1. Commit the §3 edits + new assets to a branch, push via the API helper
   (`~/workspace/goals/private-rec-room-revival-build/git_push_api.py`).
2. Merge to `main` — CI (`.github/workflows/build-recflare-installer.yml`,
   path-triggered on `game/installer/**`) builds on `windows-latest` and creates
   the `recflare-installer-v0.1.7` prerelease with `FluxRec-Setup.exe`.
3. Verify the release asset exists and its version string reads 0.1.7 before
   telling Armin anything.
4. Rollback: keep the `recflare-installer-v0.1.6` prerelease published; a revert
   is a version bump to 0.1.8 restoring the 0.1.6 tree (never delete tags).

## 7. Open questions for the build step (not blockers for this plan)

- Whether to keep the 0.1.5/0.1.6 fail-soft WARNING style on the new Goldberg
  step (recommended: yes, with `[verify]` coverage) or hard-fail like v0.1.3 did
  on the steam write. Either is defensible; decide at implementation and note it.
- The game-screenshot-test workflow's "Apply Goldberg Steam fix" step becomes
  redundant once the installer does it — update that workflow to *verify* the
  Steam files instead of applying them, so the test exercises the real installer
  path.
