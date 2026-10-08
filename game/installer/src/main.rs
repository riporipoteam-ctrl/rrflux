// FluxRec-Setup — zero-touch installer for the RecFlare-pipeline Flux Rec client.
//
// Flow:
//   0. (Upgrade mode) If RecRoom.exe is already present in the install dir,
//      skip step 1 (the ~3.8GB client.zip download + extract) and only
//      refresh the Steam bypass, logo bundle, BepInEx, plugin, config, and
//      shortcuts in place. The game dir is never wiped. The dir of every
//      successful install is remembered in
//      %LOCALAPPDATA%\FluxRec\install_dir.txt so later runs (and the
//      launcher) find it without needing --dir again.
//   1. Download the 2023 Rec Room client zip from the public mirror, verify MD5,
//      extract into the install dir. (We host no game binaries ourselves.
//      Skipped entirely in upgrade mode, see step 0.)
//   1b. Download the Flux Rec logo bundle (gzipped patched Addressables UI
//      bundle, hosted on our Hugging Face dataset), verify MD5, gunzip, and
//      overwrite the stock bundle so the loading screen shows Flux Rec
//      branding. The stock bundle is backed up as *.bundle.stock once.
//   2. Download BepInEx 6.0.0-pre.2 (Unity IL2CPP win-x64), extract into the dir.
//   3. Write the embedded RecNetPlugin.dll (RecFlare redirect plugin,
//      BepInEx build) into BepInEx/plugins/.
//   4. Write BepInEx/config/net.rec.plugin.cfg — ns host and Photon App IDs
//      baked in at packaging time from FLUXREC_NS_HOST / FLUXREC_PHOTON_RT /
//      FLUXREC_PHOTON_VOICE / FLUXREC_PHOTON_CHAT env vars (or pass --ns-host /
//      --photon-rt / --photon-voice / --photon-chat at install time).
//   5. Steam bypass pipeline (see bypass.rs): VC++ 2022 x64 runtime first
//      (the vs22-built emulator DLL crashes without it), then the Goldberg
//      emulator swap (stock DLL backed up once), steam_settings/
//      steam_appid.txt = 471710 + steam_interfaces.txt. If Goldberg fails,
//      a minimal Flux Rec stub DLL (no VC++ dependency) is used instead.
//      No Steam client installed, none needed.
//   6. Create Start Menu + desktop shortcuts (Windows only), using the blue
//      Flux Rec .ico.
//
// Exit 0 on success, 1 with an ERROR line on failure.
// Usage: FluxRec-Setup [--dir <path>] [--ns-host <url>] [--photon-rt <id>] [--photon-voice <id>] [--photon-chat <id>]
//        FluxRec-Setup --play [--dir <path>]      (launcher mode: update check, then launch the game)
//        FluxRec-Setup --updated [--dir <path>]  (spawned by the launcher: install, then launch the game)

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod assets;
mod bypass;
mod defender; // AV hardening (2026-09-24): Defender exclusions, quarantine self-heal, Unblock-File
// v0.3.0: this module now manages 2025Patch (native DLL injection) instead
// of BepInEx. Same public interface (verify/repair/sync) for launcher+defender.
mod bepinex;
mod transaction; // v0.2.0: transactional staging, atomic swap, rollback
mod guide; // one-time guided Windows Security exclusion setup (2026-09-25)
mod gui;
mod launcher;
mod patches; // Flux Rec client patches: welcome text, YouTube IDs (2026-09-24)
mod progress;
mod segmented;
mod stealth;
mod updater;
mod vcredist;

/// Client mirrors, fastest first. All serve the byte-identical client.zip
/// (same MD5) — the downloader falls through to the next on any failure.
const CLIENT_ZIP_MIRRORS: &[&str] = &[
    "https://s3.g.megas4.com/2koayuyiwxv4groxzwdbbxg43cwustavrkvfb/recflare/2025/client.zip",
];
const CLIENT_ZIP_MD5: &str = "6820e89bff41906ded7f5c066027f1d6";
// 2025 client uses 2025Patch (native DLL injection) instead of BepInEx.
// v0.3.0: 2025 client migration — BepInEx removed.
const PATCH2025_URL: &str = "https://github.com/recflare/patch-2025/releases/download/v0.0.8/2025Patch-v0.0.8-x64.zip";
// (v0.3.0: 2025Patch files are embedded — see patch2025.rs.)
/// Flux Rec logo bundles: 5 patched Addressables bundles with the blue Flux
/// logo replacing Rec Room branding (loading screens, splash, UI icons).
/// Hosted on our Hugging Face dataset; each verified by MD5 before use,
/// then copied over the stock bundle. Backups are kept as .stock files.
const LOGO_BUNDLES: &[(&str, &str, u64)] = &[
    // (filename, md5, size)
    ("063b6b6653aa642616fd21d3d4701899.bundle", "95624fde710651e930d9f93b74438477", 27_981_907),
    ("2b731967c40860818dbeadf308b851f5.bundle", "9432291269dc067ace03f76305638467", 11_326_577),
    ("6d3223da354de646ab79d5660dcac9d2.bundle", "2a94ec3a870aca582cb7d2666fc2f81a", 19_822_299),
    ("91aca73acb86d6607f0efa1f803348be.bundle", "c82afb3cf4106e2ff1e52770adce8b8a", 62_928_907),
    ("94e46740de5686a7dc4491ef7d58c517.bundle", "59bacad169acb6eb3786ea1b2f46455f", 986_601),
];
const LOGO_BUNDLE_BASE_URL: &str = "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/logo-patch-v2/";

/// Goldberg Steam emulator (gbe_fork) release — pinned. This is the exact
/// archive our cloud test runs use to make the 2023 client boot with no
/// Steam client installed.
const GBE_URL: &str =
    "https://github.com/Detanup01/gbe_fork/releases/download/release-2026_09_16_2/emu-win-release-vs22.7z";
/// Path of the win-x64 emulator DLL inside the archive.
const GBE_DLL_INNER: &str = "release/regular/x64/steam_api64.dll";
/// Rec Room's real Steam app ID (matches our cloud test setup).
pub(crate) const STEAM_APP_ID: &str = "471710";
/// steam_interfaces.txt, embedded from assets/ (same file the cloud tests copy).
pub(crate) const STEAM_INTERFACES: &str = include_str!("../assets/steam_interfaces.txt");

const NS_PLACEHOLDER: &str = "fluxrec-api.ripo-ripoteam.workers.dev";
/// Compile-time wiring (GitHub Secrets -> CI env -> baked in at build).
/// Keeps the IDs out of the repo; runtime args/env still override.
const NS_HOST_DEFAULT: &str = match option_env!("FLUXREC_NS_HOST") {
    Some(v) => v,
    None => NS_PLACEHOLDER,
};
const PHOTON_RT_DEFAULT: &str = match option_env!("FLUXREC_PHOTON_RT") {
    Some(v) => v,
    None => "%%FLUXREC_PHOTON_RT%%",
};
const PHOTON_VOICE_DEFAULT: &str = match option_env!("FLUXREC_PHOTON_VOICE") {
    Some(v) => v,
    None => "%%FLUXREC_PHOTON_VOICE%%",
};
const PHOTON_CHAT_DEFAULT: &str = match option_env!("FLUXREC_PHOTON_CHAT") {
    Some(v) => v,
    None => "%%FLUXREC_PHOTON_CHAT%%",
};
/// Stall detection: any stream silent this long is aborted and retried.
const STALL_SECS: u64 = 60;
const MAX_ATTEMPTS: u32 = 5;
/// Time to wait for the server to start responding (headers) before
/// aborting the attempt and retrying. The client archive host can be
/// very slow to first byte; without this the request hangs indefinitely.
const FIRST_BYTE_TIMEOUT_SECS: u64 = 180;

fn default_install_dir() -> PathBuf {
    let drive = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".to_string());
    PathBuf::from(format!("{drive}\\Games\\FluxRec"))
}

/// File that records the install directory of the last successful install.
/// Lets later runs (and the launcher) find the game dir without --dir.
fn persisted_install_dir_file() -> PathBuf {
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
    install_dir_file_in(&local_appdata)
}

/// Pure part of the above, kept separate so tests can point it at a scratch
/// dir instead of the real %LOCALAPPDATA%.
fn install_dir_file_in(local_appdata: &str) -> PathBuf {
    PathBuf::from(local_appdata)
        .join("FluxRec")
        .join("install_dir.txt")
}

/// Read a previously persisted install dir, trimming whitespace. Returns
/// None when the file is missing, unreadable, or empty.
fn read_install_dir_file(file: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(file).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Persist the install dir after a successful install. Fail-soft by design:
/// this is bookkeeping, so a missing %LOCALAPPDATA%, a locked file, or any
/// other error just means the dir won't be remembered — it never fails the
/// install.
fn write_install_dir_file(file: &Path, dir: &Path) {
    if let Some(parent) = file.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    // A trailing newline is harmless — the reader trims.
    let _ = std::fs::write(file, format!("{}\n", dir.display()));
}

fn read_persisted_install_dir() -> Option<String> {
    read_install_dir_file(&persisted_install_dir_file())
}

fn persist_install_dir(dir: &Path) {
    write_install_dir_file(&persisted_install_dir_file(), dir);
}

/// Resolve the effective install dir for this run.
/// Priority: an explicitly passed --dir always wins; otherwise the dir
/// persisted by a previous successful install; otherwise the default.
pub(crate) fn resolve_install_dir(
    dir_arg: Option<&Path>,
    persisted: Option<&str>,
    default: &Path,
) -> PathBuf {
    if let Some(d) = dir_arg {
        return d.to_path_buf();
    }
    if let Some(p) = persisted {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    default.to_path_buf()
}

pub(crate) fn md5_of_file(path: &Path) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, f);
    let mut ctx = md5::Context::new();
    std::io::copy(&mut reader, &mut ctx).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", ctx.compute()))
}

pub(crate) fn file_ok(path: &Path, expected_md5: Option<&str>, expected_size: Option<u64>) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if let Some(s) = expected_size {
        if meta.len() != s {
            return false;
        }
    }
    if let Some(h) = expected_md5 {
        match md5_of_file(path) {
            Ok(got) => {
                if !got.eq_ignore_ascii_case(h) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

async fn probe_range(client: &reqwest::Client, url: &str) -> bool {
    let r = client
        .get(url)
        .header("Range", "bytes=0-0")
        .send()
        .await;
    matches!(r, Ok(resp) if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT)
}

/// Download `url` to `dest` (resume-capable when the host honors Range),
/// verifying MD5/size when given. Writes to `<dest>.part` and renames only
/// after verification, so a half-written file never looks valid.
///
/// `progress` optionally carries `(&Progress, stage_name)`; the stage's
/// percent range is filled as bytes arrive. Never blocks, never panics.
pub(crate) async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_md5: Option<&str>,
    expected_size: Option<u64>,
    label: &str,
    progress: Option<(&progress::Progress, &'static str)>,
) -> Result<(), String> {
    if file_ok(dest, expected_md5, expected_size) {
        println!("[{label}] already present and verified, skipping.");
        if let Some((p, stage)) = progress {
            p.set_stage(stage);
            p.set_fraction(1.0);
        }
        return Ok(());
    }
    if dest.exists() {
        println!("[{label}] existing file failed verification, re-downloading.");
        std::fs::remove_file(dest).map_err(|e| e.to_string())?;
    }
    // Fast path: parallel segmented download (16 Range segments) for anything
    // not known-small. Any failure falls through to the classic single
    // stream below — speed can never break a download.
    if expected_size.map(|s| s >= segmented::SEGMENT_MIN_SIZE).unwrap_or(true) {
        match segmented::download_segmented(
            client,
            url,
            dest,
            expected_md5,
            expected_size,
            label,
            progress,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("[{label}] segmented download failed ({e}); single-stream fallback.");
            }
        }
    }
    let part: PathBuf = {
        let mut p = dest.as_os_str().to_owned();
        p.push(".part");
        PathBuf::from(p)
    };
    let range_ok = probe_range(client, url).await;
    if range_ok {
        println!("[{label}] host honors Range — resume enabled.");
    }

    let mut attempt = 0;
    loop {
        attempt += 1;
        if let Some((p, stage)) = progress {
            p.set_stage(stage);
        }
        let resume_from = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut req = client.get(url);
        if range_ok && resume_from > 0 {
            req = req.header("Range", format!("bytes={resume_from}-"));
            println!("[{label}] resuming at byte {resume_from}.");
        }
        let resp = tokio::time::timeout(
            Duration::from_secs(FIRST_BYTE_TIMEOUT_SECS),
            req.send(),
        )
        .await
        .map_err(|_| "timed out waiting for server response (slow host)".to_string())?
        .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !(status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT) {
            let msg = format!("HTTP {status}");
            if attempt >= MAX_ATTEMPTS {
                return Err(msg);
            }
            println!("[{label}] attempt {attempt}: {msg}, retrying.");
            continue;
        }
        let total = expected_size.unwrap_or_else(|| {
            resume_from + resp.content_length().unwrap_or(0)
        });

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(resume_from > 0)
            .write(true)
            .truncate(resume_from == 0)
            .open(&part)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = resp.bytes_stream();
        let mut done = resume_from;
        let mut last_print = Instant::now();
        let started = Instant::now();
        let mut failed: Option<String> = None;
        loop {
            match tokio::time::timeout(Duration::from_secs(STALL_SECS), stream.next()).await {
                Err(_) => {
                    failed = Some(format!("stalled {STALL_SECS}s without data"));
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    failed = Some(e.to_string());
                    break;
                }
                Ok(Some(Ok(chunk))) => {
                    use tokio::io::AsyncWriteExt as _;
                    file.write_all(&chunk).await.map_err(|e| e.to_string())?;
                    done += chunk.len() as u64;
                    if let Some((p, _)) = progress {
                        if total > 0 {
                            p.set_fraction(done as f64 / total as f64);
                        }
                    }
                    if last_print.elapsed() >= Duration::from_secs(2) {
                        last_print = Instant::now();
                        let pct = if total > 0 {
                            done as f64 / total as f64 * 100.0
                        } else {
                            0.0
                        };
                        println!(
                            "[{label}] {done}/{total} bytes ({pct:.1}%) elapsed={}s",
                            started.elapsed().as_secs()
                        );
                    }
                }
            }
        }
        drop(file);
        if let Some(f) = failed {
            if attempt >= MAX_ATTEMPTS {
                return Err(format!("{label}: {f} (attempt {attempt})"));
            }
            println!("[{label}] attempt {attempt}: {f}, retrying.");
            continue;
        }
        // Verify before promoting the .part file.
        if !file_ok(&part, expected_md5, expected_size) {
            let _ = std::fs::remove_file(&part);
            let msg = "hash/size mismatch after download".to_string();
            if attempt >= MAX_ATTEMPTS {
                return Err(format!("{label}: {msg}"));
            }
            println!("[{label}] attempt {attempt}: {msg}, retrying.");
            continue;
        }
        std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
        println!("[{label}] done ({done} bytes).");
        return Ok(());
    }
}

/// Download trying each mirror URL in order until one verifies.
///
/// Mirrors must serve the byte-identical file (same MD5): a corrupt mirror
/// just fails verification and the next mirror is tried. Segment part-files
/// are even resumable across mirrors since the bytes are identical.
pub(crate) async fn download_mirrored(
    client: &reqwest::Client,
    urls: &[&str],
    dest: &Path,
    expected_md5: Option<&str>,
    expected_size: Option<u64>,
    label: &str,
    progress: Option<(&progress::Progress, &'static str)>,
) -> Result<(), String> {
    let mut last_err = format!("{label}: no mirrors configured");
    for (i, url) in urls.iter().enumerate() {
        if i > 0 {
            println!("[{label}] mirror {} failed ({last_err}); trying mirror {} ...", i, i + 1);
            if let Some((p, stage)) = progress {
                p.set_stage(stage);
            }
        }
        match download(client, url, dest, expected_md5, expected_size, label, progress).await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
            }
        }
    }
    Err(format!("{label}: all {} mirrors failed (last: {last_err})", urls.len()))
}

pub(crate) fn extract_zip(zip_path: &Path, dest_dir: &Path, label: &str) -> Result<(), String> {
    println!("[{label}] extracting...");
    let f = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let n = archive.len();
    archive.extract(dest_dir).map_err(|e| e.to_string())?;
    println!("[{label}] extracted {n} entries.");
    Ok(())
}

/// Ensure the Windows hosts file maps ns.rec.net to the backend IP.
/// The 2023 client resolves ns.rec.net itself via System.Net.Dns, bypassing
/// the plugin's HTTP-level redirect. On machines where that dead hostname
/// doesn't resolve (e.g., certain ISPs), the game hangs at "Connecting to
/// server..." forever. This maps it to the backend so DNS succeeds.
/// Idempotent, fail-soft (a hosts write failure never breaks the install).
#[cfg(windows)]
pub(crate) fn ensure_ns_hosts_entry(ns_host: &str) {
    // Resolve ns_host to an IP (shared helper: IP literals pass through,
    // schemes/paths are stripped). None when offline/unresolvable.
    let ip = match crate::defender::resolve_backend_ip(ns_host) {
        Some(ip) => ip,
        None => {
            eprintln!(
                "[hosts] WARNING: could not resolve {}; skipping hosts entry.",
                ns_host
            );
            return;
        }
    };

    let hosts_path = std::path::Path::new("C:\\Windows\\System32\\drivers\\etc\\hosts");
    let content = match std::fs::read_to_string(hosts_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[hosts] WARNING: cannot read hosts file ({}); skipping.", e);
            return;
        }
    };

    // 2026-09-24 (defender.rs hardening): don't BE the malware — writing to
    // the hosts file is itself AV-suspicious, so only touch it when the
    // entry is actually absent. Checked against the file content, not a live
    // DNS comparison: the backend is anycast and a fresh resolution can
    // return a different IP than the stored entry, which used to send the
    // launcher into bogus "repair" loops ending in a blocking error dialog.
    if crate::defender::ns_hosts_entry_present(&content) {
        println!("[hosts] ns.rec.net entry already present; nothing to do.");
        return;
    }

    let entry = format!("{} ns.rec.net", ip);
    // Check if the correct entry already exists
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // If ns.rec.net is already mapped (to any IP), we need to update or skip
        if trimmed.contains("ns.rec.net") {
            if trimmed == entry {
                println!("[hosts] ns.rec.net already mapped to {}; nothing to do.", ip);
                return;
            } else {
                // Entry exists but with different IP — remove old lines and add new
                let new_content: Vec<&str> = content
                    .lines()
                    .filter(|l| !l.trim().contains("ns.rec.net"))
                    .collect();
                let mut updated = new_content.join("\n");
                updated.push_str(&format!("\n{} # Flux Rec backend\n", entry));
                if let Err(e) = std::fs::write(hosts_path, updated) {
                    eprintln!("[hosts] WARNING: cannot update hosts file ({}); skipping.", e);
                } else {
                    println!("[hosts] updated ns.rec.net -> {} in hosts file.", ip);
                }
                return;
            }
        }
    }

    // No existing entry — append
    let mut updated = content;
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&format!("{} # Flux Rec backend\n", entry));
    match std::fs::write(hosts_path, updated) {
        Ok(_) => println!("[hosts] added {} to hosts file.", entry),
        Err(e) => eprintln!("[hosts] WARNING: cannot write hosts file ({}); game may hang at 'Connecting to server' if DNS fails.", e),
    }
}

#[cfg(not(windows))]
pub(crate) fn ensure_ns_hosts_entry(_ns_host: &str) {
    // No-op on non-Windows (only used for local testing)
}

// NOTE (2026-09-24): `unblock_file` moved to defender.rs
// (`defender::unblock_file`) — same behavior, but via the hidden
// PowerShell helper (no console flash) and applied to every
// internet-sourced file we write, not just the plugin DLL.

#[cfg(windows)]
fn ps_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Windows-only: create a .lnk via WScript.Shell. Skipped elsewhere.
fn create_shortcut(
    lnk: &Path,
    target: &Path,
    args: &str,
    workdir: &Path,
    icon: Option<&Path>,
) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (lnk, target, args, workdir, icon);
        println!("[shortcut] skipped (not Windows): {}", lnk.display());
        return Ok(());
    }
    #[cfg(windows)]
    {
        let icon_line = match icon {
            Some(p) => format!(
                "$sc.IconLocation = '{ico}';",
                ico = ps_escape(&p.to_string_lossy())
            ),
            None => String::new(),
        };
        let cmd = format!(
            "$ws = New-Object -ComObject WScript.Shell; \
             $sc = $ws.CreateShortcut('{lnk}'); \
             $sc.TargetPath = '{tgt}'; $sc.Arguments = '{args}'; \
             $sc.WorkingDirectory = '{wd}'; $sc.Description = 'Flux Rec'; {icon_line} $sc.Save()",
            lnk = ps_escape(&lnk.to_string_lossy()),
            tgt = ps_escape(&target.to_string_lossy()),
            args = ps_escape(args),
            wd = ps_escape(&workdir.to_string_lossy()),
        );
        let out = stealth::hidden_command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "shortcut failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        println!("[shortcut] {}", lnk.display());
        Ok(())
    }
}

/// Game exe: v0.3.1+ ships ONLY the 2025 client (`Recroom_Release.exe`).
/// The 2023 name (`RecRoom.exe`) is deliberately NOT matched anymore:
/// v0.3.0 treated a stale 2023 install as a valid game, skipped the 2025
/// client download, overlaid 2025Patch onto the 2023 tree, and launched the
/// OLD client with its BepInEx stack (black screen on Armin's PC, no
/// 2025patch.log). A 2023 tree is never a valid 2025 install.
pub(crate) fn find_game_exe(dir: &Path) -> Option<PathBuf> {
    for name in [
        // 2025: Recroom_Release.exe IS the game.
        // 2026: Recroom_Release.exe is the Referee Client Launcher — it
        //   starts the Referee service and then launches RecRoom.exe (the
        //   actual Unity game). Launching through it is the supported flow.
        "Recroom_Release.exe",
        "recroom_release.exe",
        "RecRoom.exe",
        "recroom.exe",
    ] {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Detect a leftover 2023-era install in `dir`: the old exe name, the
/// BepInEx loader, or the Doorstop proxy DLL. Used to trigger the clean
/// migration path (quarantine the old tree, fresh-install 2025) instead of
/// the upgrade path, which must never run against a 2023 tree.
pub(crate) fn is_2023_install(dir: &Path) -> bool {
    // 2026 client also uses RecRoom.exe, so don't use exe name alone.
    // 2023 is identified by BepInEx/Doorstop loader files.
    dir.join("BepInEx").is_dir()
        || dir.join("winhttp.dll").exists()
        || dir.join("doorstop_config.ini").exists()
}

/// Detect a March 2026 client install: it has the Referee service installer
/// or the 2026 data dir name. v0.6.0 migrates 2026 -> 2025 (quarantine + fresh
/// 2025 download) because the 2026 Referee chain cannot launch.
pub(crate) fn is_2026_install(dir: &Path) -> bool {
    dir.join("RefereeClientInstaller.exe").exists()
        || dir.join("RefereeClientApp.exe").exists()
        || dir.join("RecRoom_Data").is_dir()
}

/// Upgrade-mode decision (pure): skip the ~4.2GB client.zip download +
/// extract only when a real 2025 install (`Recroom_Release.exe`) is already
/// present in the dir. A 2023 tree (old exe / BepInEx / Doorstop files)
/// NEVER counts — it takes the migration path instead.
/// v0.4.1: Force re-download to upgrade from 2022 client to 2026 client.
pub(crate) fn should_skip_client_download(_dir: &Path) -> bool {
    false
}

/// Quarantine a stale 2023 install before a clean 2025 migration: rename
/// the whole tree to `<dir>.2023-backup` (kept, never deleted) so the fresh
/// 2025 install lands in a guaranteed-clean directory. Returns the backup
/// path on success.
fn quarantine_2023_tree(dir: &Path) -> Result<PathBuf, String> {
    let base = dir.as_os_str().to_owned();
    for i in 0..100 {
        let mut name = base.clone();
        if i == 0 {
            name.push(".2023-backup");
        } else {
            name.push(format!(".2023-backup-{i}"));
        }
        let backup = PathBuf::from(name);
        if backup.exists() {
            continue;
        }
        std::fs::rename(dir, &backup)
            .map_err(|e| format!("could not quarantine old 2023 install: {e}"))?;
        println!(
            "[migrate] quarantined 2023 tree -> {} (kept, not deleted).",
            backup.display()
        );
        return Ok(backup);
    }
    Err("could not find a free .2023-backup name".to_string())
}

fn create_shortcuts(dir: &Path) -> Result<(), String> {
    let exe = match find_game_exe(dir) {
        Some(p) => p,
        None => {
            // Don't fail the install: the archive layout is verified at packaging
            // time; shortcuts just can't be made without the exe.
            println!("[shortcut] WARNING: RecRoom.exe not found under {}, skipping shortcuts.", dir.display());
            return Ok(());
        }
    };
    // Install the launcher entry point next to the game: a copy of this
    // setup binary. The shortcuts point at it with `--play`, so every
    // launch goes through the update check + pretty window first.
    // Fail-soft: if the copy fails we fall back to the old direct target.
    let launcher_exe = dir.join("FluxRecLauncher.exe");
    let launcher_ok = match std::env::current_exe() {
        Ok(me) => {
            // remove-first: Windows cannot overwrite a running exe, and the
            // launcher always exits before spawning a new setup, so the old
            // copy is never running here.
            #[cfg(windows)]
            let _ = std::fs::remove_file(&launcher_exe);
            match std::fs::copy(&me, &launcher_exe) {
                Ok(_) => {
                    println!("[shortcut] installed launcher: {}", launcher_exe.display());
                    // AV hardening (2026-09-24): a copied exe keeps its
                    // Mark-of-the-Web; clear it so Defender/SmartScreen treat
                    // the launcher like a local file.
                    defender::unblock_file(&launcher_exe);
                    true
                }
                Err(e) => {
                    eprintln!("[shortcut] WARNING: launcher copy failed ({}); using direct target.", e);
                    false
                }
            }
        }
        Err(e) => {
            eprintln!("[shortcut] WARNING: current_exe unavailable ({}); using direct target.", e);
            false
        }
    };
    // New behavior: shortcut -> launcher --play. Fallback: game exe directly.
    let (target, args): (PathBuf, &str) = if launcher_ok {
        (launcher_exe, "--play")
    } else {
        (exe, "+forcemode:screen")
    };
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA").map_err(|e| e.to_string())?;
        let start_menu = PathBuf::from(format!(
            "{appdata}\\Microsoft\\Windows\\Start Menu\\Programs\\Flux Rec.lnk"
        ));
        let desktop = stealth::hidden_command("powershell")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "[Environment]::GetFolderPath('Desktop')",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        let desktop_dir = String::from_utf8_lossy(&desktop.stdout).trim().to_string();
        let desktop_lnk = PathBuf::from(format!("{desktop_dir}\\Flux Rec.lnk"));
        // Blue logo icon for both shortcuts (fail-soft: generic icon if the
        // write fails).
        let icon_path = dir.join("fluxrec.ico");
        if let Err(e) = assets::write_icon_ico(&icon_path) {
            eprintln!("[shortcut] WARNING: icon write failed ({}).", e);
        }
        create_shortcut(&start_menu, &target, args, dir, Some(&icon_path))?;
        create_shortcut(&desktop_lnk, &target, args, dir, Some(&icon_path))?;
    }
    #[cfg(not(windows))]
    {
        create_shortcut(&PathBuf::from("Flux Rec.lnk"), &target, args, dir, None)?;
    }
    Ok(())
}

/// Install the logo bundle from a local .gz file: gunzip into a temp file,
/// verify the uncompressed bytes, back up the stock bundle once (never
/// overwriting an existing backup), then atomically replace the target.
/// The stock/current bundle is preserved on every failure path.
fn install_logo_bundle(
    gz_path: &Path,
    target: &Path,
    expected_md5: &str,
    expected_size: u64,
) -> Result<(), String> {
    // Gunzip into a temp file next to the target, then verify before touching it.
    let tmp_path = target.with_extension("bundle.logo-new");
    {
        let gz_file = std::fs::File::open(gz_path).map_err(|e| e.to_string())?;
        let mut decoder = flate2::read::GzDecoder::new(gz_file);
        let mut tmp = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
        std::io::copy(&mut decoder, &mut tmp).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            e.to_string()
        })?;
    }

    if !file_ok(&tmp_path, Some(expected_md5), Some(expected_size)) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err("logo bundle failed verification after gunzip".to_string());
    }

    // Back up the stock bundle once — never overwrite an existing backup.
    let backup_path = target.with_extension("bundle.stock");
    if !backup_path.exists() {
        std::fs::copy(target, &backup_path).map_err(|e| e.to_string())?;
        println!("[logo] backed up stock bundle.");
    }

    // Atomic replace: rename the verified temp file over the target.
    // NOTE: on Windows, rename() fails if the target already exists, so
    // remove it first. The .stock backup above guarantees we can restore
    // the original bundle if anything goes wrong here.
    #[cfg(windows)]
    if target.exists() {
        std::fs::remove_file(target).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&tmp_path, target) {
        let _ = std::fs::remove_file(&tmp_path);
        // Never leave the game without this bundle: restore the stock backup.
        if backup_path.exists() {
            let _ = std::fs::copy(&backup_path, target);
        }
        return Err(e.to_string());
    }
    println!("[logo] Flux Rec logo bundle applied.");
    Ok(())
}

pub(crate) async fn apply_logo_bundle(
    client: &reqwest::Client,
    dir: &Path,
    progress: &progress::Progress,
) -> Result<(), String> {
    // v0.6.6: 2025 client uses Recroom_Release_Data, not RecRoom_Data.
    // Check both to support 2025 and 2026 layouts.
    let aa_dir = {
        let path2025 = dir
            .join("Recroom_Release_Data")
            .join("StreamingAssets")
            .join("aa")
            .join("StandaloneWindows64");
        if path2025.exists() {
            path2025
        } else {
            dir.join("RecRoom_Data")
                .join("StreamingAssets")
                .join("aa")
                .join("StandaloneWindows64")
        }
    };

    if !aa_dir.exists() {
        println!("[logo] Addressables dir not found (fresh layout?), skipping.");
        return Ok(());
    }

    // Apply each of the 5 patched logo bundles.
    for (idx, (filename, expected_md5, expected_size)) in LOGO_BUNDLES.iter().enumerate() {
        let target = aa_dir.join(filename);

        if !target.exists() {
            println!("[logo] target {} not found, skipping.", filename);
            continue;
        }

        // Idempotency: if the installed bundle already has our patched hash, skip.
        if let Ok(h) = md5_of_file(&target) {
            if h.eq_ignore_ascii_case(expected_md5) {
                println!("[logo] {} already patched, skipping.", filename);
                continue;
            }
        }

        // Download the patched bundle (verified by MD5 + size).
        let url = format!("{}{}", LOGO_BUNDLE_BASE_URL, filename);
        let tmp_path = dir.join(format!("logo-patch-{}.tmp", idx));
        download(
            client,
            &url,
            &tmp_path,
            Some(expected_md5),
            Some(*expected_size),
            filename,
            Some((progress, "Applying Flux Rec branding…")),
        )
        .await?;
        // AV hardening (defender.rs): strip the Mark of the Web.
        defender::unblock_file(&tmp_path);

        if !file_ok(&tmp_path, Some(expected_md5), Some(*expected_size)) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("logo bundle {} failed verification", filename));
        }

        // Back up the stock bundle once — never overwrite an existing backup.
        let backup_path = target.with_extension("bundle.stock");
        if !backup_path.exists() {
            std::fs::copy(&target, &backup_path).map_err(|e| e.to_string())?;
            println!("[logo] backed up {}.", filename);
        }

        // Atomic replace.
        #[cfg(windows)]
        if target.exists() {
            std::fs::remove_file(&target).map_err(|e| e.to_string())?;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &target) {
            let _ = std::fs::remove_file(&tmp_path);
            if backup_path.exists() {
                let _ = std::fs::copy(&backup_path, &target);
            }
            return Err(e.to_string());
        }
        println!("[logo] Applied {} (Flux Rec branding).", filename);
    }
    Ok(())
}

async fn run_install(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &progress::Progress,
) -> Result<(), String> {
    progress.set_stage("Preparing…");
    // Clean staging/backup leftovers from crashed previous runs.
    if let Some(parent) = dir.parent() {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            transaction::cleanup_leftovers(parent, name);
        }
    }
    let client = reqwest::Client::builder()
        .user_agent("FluxRec-Setup/0.2.0")
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    // v0.2.0: TRANSACTIONAL install. Fresh installs build the complete tree
    // in a staging directory and atomically swap it into place; upgrades
    // refresh components in place with per-file backups and rollback.
    // The live directory is NEVER deleted first.
    //
    // v0.3.1: the upgrade path only ever runs on a REAL 2025 install.
    // A 2023-era tree (old exe / BepInEx / Doorstop) is quarantined aside
    // first, then 2025 is fresh-installed into the clean dir. Upgrading a
    // 2023 tree in place is what bricked v0.3.0 (black screen, old BepInEx
    // stack launching instead of the 2025 client).
    if find_game_exe(dir).is_none() {
        if is_2023_install(dir) {
            println!("[install] 2023-era install detected — migrating to a clean 2025 tree.");
            progress.set_stage("Migrating…");
            progress.set_detail("Moving the old 2023 install aside (kept as backup)…".to_string());
            quarantine_2023_tree(dir)?;
            progress.set_detail(String::new());
        }
        println!("[install] fresh install — building in staging.");
        fresh_install(
            dir,
            &client,
            ns_host,
            photon_rt,
            photon_voice,
            photon_chat,
            progress,
        )
        .await
    } else if is_2026_install(dir) {
        // v0.6.0: March 2026 client cannot launch (Referee service dead).
        // Quarantine the 2026 tree and fresh-install the 2025 client.
        println!("[install] 2026-era install detected — migrating to a clean 2025 tree.");
        progress.set_stage("Migrating…");
        progress.set_detail("Moving the old 2026 install aside (kept as backup)…".to_string());
        quarantine_2023_tree(dir)?;
        progress.set_detail(String::new());
        println!("[install] fresh install — building in staging.");
        fresh_install(
            dir,
            &client,
            ns_host,
            photon_rt,
            photon_voice,
            photon_chat,
            progress,
        )
        .await
    } else {
        println!("[install] upgrade — refreshing components in place with rollback.");
        upgrade_install(
            dir,
            &client,
            ns_host,
            photon_rt,
            photon_voice,
            photon_chat,
            progress,
        )
        .await
    }
}

/// Fresh install: build the complete tree inside a staging directory, verify
/// it strictly, then atomically swap it into place. Any failure aborts the
/// staging tree and leaves the machine untouched.
async fn fresh_install(
    dir: &Path,
    client: &reqwest::Client,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &progress::Progress,
) -> Result<(), String> {
    let staging = transaction::Staging::begin(dir).map_err(|e| e.to_string())?;
    let target = staging.path().to_path_buf();
    let build = build_full_install(
        &target,
        client,
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        progress,
    )
    .await;
    if let Err(e) = build {
        staging.abort();
        return Err(format!("fresh install failed (live directory untouched): {e}"));
    }
    progress.set_stage("Committing installation…");
    progress.set_detail("Swapping new installation into place…".to_string());
    staging.commit().map_err(|e| e.to_string())?;
    // Post-commit steps against the live dir — fail-soft, the game itself
    // is already installed and verified.
    finish_live_dir(dir, ns_host, progress);
    progress.set_stage("Final checks…");
    verify_install(dir)?;
    persist_install_dir(dir);
    println!("[install] fresh install committed and verified.");
    Ok(())
}

/// Upgrade: refresh managed components in place. Every overwritten file is
/// backed up first; any failure restores the backups so the previous
/// working install is never left half-upgraded.
async fn upgrade_install(
    dir: &Path,
    client: &reqwest::Client,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &progress::Progress,
) -> Result<(), String> {
    let mut backups = transaction::BackupSet::new();
    let refresh = refresh_components(
        dir,
        client,
        &mut backups,
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        progress,
    )
    .await;
    match refresh {
        Ok(()) => {
            backups.commit();
            // 2026: ensure the Referee service is installed and any v0.5.9
            // *.disabled renames are repaired (upgrade path skips the fresh
            // client download, so this wouldn't otherwise run).
            // v0.6.0: only for 2026 installs (2025 has no service files).
            if dir.join("RefereeClientInstaller.exe").exists() {
                install_referee_service(dir, progress);
            }
            finish_live_dir(dir, ns_host, progress);
            progress.set_stage("Final checks…");
            verify_install(dir)?;
            persist_install_dir(dir);
            println!("[install] upgrade completed and verified.");
            Ok(())
        }
        Err(e) => {
            backups.rollback();
            Err(format!("upgrade failed — rolled back to previous install: {e}"))
        }
    }
}

/// Build the complete installation tree inside `target` (staging dir or the
/// live dir for upgrades that need the client). Strict: any error aborts.
async fn build_full_install(
    target: &Path,
    client: &reqwest::Client,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &progress::Progress,
) -> Result<(), String> {
    // 1. Game client (~4.2GB). Create target dir first — the download fails
    // with "system cannot find the path specified" if it doesn't exist.
    std::fs::create_dir_all(target).map_err(|e| format!("create game dir: {e}"))?;
    let client_zip = target.join("client.zip");
    download_mirrored(
        client,
        CLIENT_ZIP_MIRRORS,
        &client_zip,
        Some(CLIENT_ZIP_MD5),
        None,
        "client",
        Some((progress, "Downloading game files…")),
    )
    .await?;
    // AV hardening (defender.rs): strip the Mark of the Web from the
    // archive before extracting it.
    defender::unblock_file(&client_zip);
    progress.set_stage("Extracting game files…");
    progress.set_detail("Extracting game files…".to_string());
    extract_zip(&client_zip, target, "client")?;
    let _ = std::fs::remove_file(&client_zip); // free ~3.8GB after extract
    // 2026 client only: install the Referee anti-cheat Windows service the way
    // Steam did (installscript.vdf runs `RefereeClientInstaller.exe -install`
    // on first install). The March 2026 RecRoom.exe cannot start without it.
    // The 2025 client has no service files (no RefereeClientInstaller.exe) —
    // its 2025Patch handles the anti-cheat in-memory, so skip the service.
    if target.join("RefereeClientInstaller.exe").exists() {
        install_referee_service(target, progress);
        progress.set_detail(String::new());
    }

    apply_common_components(
        target,
        client,
        &mut transaction::BackupSet::new(),
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        progress,
    )
    .await
}

/// 2026 client: install the Referee anti-cheat Windows service the way Steam
/// did (`installscript.vdf` runs `RefereeClientInstaller.exe -install` once on
/// first install). The March 2026 `RecRoom.exe` statically imports `TWpjzW`
/// from `Referee.dll`, and the anti-cheat terminates the game process when
/// its service isn't installed/running — so the service setup is part of a
/// working install, not optional.
///
/// Also repairs v0.5.9 installs: that version renamed the Referee files to
/// `*.disabled`, which broke the game load entirely. The files are restored
/// to their shipped names here.
///
/// Must run elevated (setup already relaunches as admin before installing).
/// Fail-soft: if the install fails, the Referee Client Launcher
/// (`Recroom_Release.exe`) retries the service start at play time, so a
/// failure here logs loudly but doesn't abort the install.
#[cfg(windows)]
fn install_referee_service(target: &Path, progress: &progress::Progress) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // v0.5.9 renamed these to *.disabled — restore the shipped names.
    for referee_file in [
        "Referee.dll",
        "RefereeClientApp.exe",
        "RefereeClientInstaller.exe",
    ] {
        let disabled = target.join(format!("{referee_file}.disabled"));
        let src = target.join(referee_file);
        if disabled.is_file() && !src.is_file() {
            match std::fs::rename(&disabled, &src) {
                Ok(()) => println!("[referee] restored {referee_file}"),
                Err(e) => eprintln!("[referee] WARNING: could not restore {referee_file}: {e}"),
            }
        }
    }
    let installer = target.join("RefereeClientInstaller.exe");
    if !installer.is_file() {
        return; // not a Referee-era client, nothing to do
    }
    println!("[referee] installing Referee anti-cheat service…");
    progress.set_stage("Installing anti-cheat service…");
    match std::process::Command::new(&installer)
        .arg("-install")
        .creation_flags(CREATE_NO_WINDOW)
        .status()
    {
        Ok(s) if s.success() => println!("[referee] service installed."),
        Ok(s) => eprintln!(
            "[referee] WARNING: service installer exited with {s}; the game may fail to start."
        ),
        Err(e) => eprintln!("[referee] WARNING: could not run service installer: {e}"),
    }
    // Start it now so it's already running at first launch. `sc` failing
    // (already running, service not registered) is fine — the game launcher
    // starts it on demand.
    //
    // v0.5.9-fix: set the service to AUTO-START. The game launcher
    // (Recroom_Release.exe) runs non-elevated and cannot start a stopped
    // service (access denied) — if the service isn't already running, the
    // launcher retries and gives up ("banner then close"). Auto-start
    // ensures it's running after boot without needing elevation at play time.
    let _ = std::process::Command::new("sc")
        .args(["config", "RefereeClientApp", "start=", "auto"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let started = std::process::Command::new("sc")
        .args(["start", "RefereeClientApp"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if started {
        println!("[referee] service started.");
    } else {
        eprintln!("[referee] note: service not started yet (the game launcher will start it).");
    }
}

#[cfg(not(windows))]
fn install_referee_service(_target: &Path, _progress: &progress::Progress) {}

/// Refresh managed components in an existing install. `backups` protects
/// every file that gets overwritten.
async fn refresh_components(
    dir: &Path,
    client: &reqwest::Client,
    backups: &mut transaction::BackupSet,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &progress::Progress,
) -> Result<(), String> {
    // Protect the Steam bypass files before the pipeline touches them.
    // v0.3.1: 2025 client layout (`Recroom_Release_Data`, not `RecRoom_Data`).
    let plug = dir
        .join("Recroom_Release_Data")
        .join("Plugins")
        .join("x86_64");
    for rel in [
        "steam_api64.dll",
        "steam_settings/steam_appid.txt",
        "steam_settings/steam_interfaces.txt",
    ] {
        // Best-effort: a missing file just means the bypass reinstalls it.
        let _ = backups.protect(&plug.join(rel));
    }
    apply_common_components(
        dir,
        client,
        backups,
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        progress,
    )
    .await
}

/// Steps shared by fresh installs and upgrades: Steam bypass, branding,
/// client patches, BepInEx. In fresh mode `target` is the staging dir
/// (the backups set is a throwaway — staging abort covers failures); in
/// upgrade mode it is the live dir and `backups` is the caller's set, so a
/// later failure rolls back these changes too.
async fn apply_common_components(
    target: &Path,
    client: &reqwest::Client,
    _backups: &mut transaction::BackupSet,
    ns_host: &str,
    _photon_rt: &str,
    _photon_voice: &str,
    _photon_chat: &str,
    progress: &progress::Progress,
) -> Result<(), String> {
    // 2. Steam bypass FIRST: the game cannot boot without this, and no
    // later step may ever prevent it from being in place.
    let bypass_method = bypass::apply_steam_bypass(client, target, progress).await?;
    println!("[install] Steam bypass in place: {bypass_method:?}.");

    // 3. Flux Rec logo bundle: patched loading-screen bundle over the stock one.
    // FAIL-SOFT: the logo is cosmetic. If it fails for any reason, warn and
    // continue — the game must always end up fully installed and playable.
    progress.set_stage("Applying Flux Rec branding…");
    if let Err(e) = apply_logo_bundle(client, target, progress).await {
        eprintln!("[logo] WARNING: logo bundle step failed ({}); continuing without it.", e);
    }

    // 4. Flux Rec client patches: welcome screen text, YouTube video IDs.
    // Same-length byte replacements, fail-soft.
    patches::apply_client_patches(target);

    // 5. BepInEx (v0.2.6: restored — the proven loader; FluxLoader's
    // v0.3.0: 2025 client uses 2025Patch (native DLL injection) instead of BepInEx.
    // The patch files (2025Patch.dll, Injector.exe, 2025patch.ini) go next to
    // Recroom_Release.exe. The injector launches the game with the patch.
    progress.set_stage("Installing 2025Patch…");
    let patch_zip = fetch_patch2025_zip(client, progress).await?;
    install_patch2025(
        target,
        &patch_zip,
        ns_host,
        progress,
    )?;
    // Clean up the temp patch zip.
    let _ = std::fs::remove_file(&patch_zip);

    // 6. Configuration lives in 2025patch.ini (written by install_patch2025).
    progress.set_stage("Writing configuration…");
    println!("[install] configuration written.");
    Ok(())
}

/// Download the 2025Patch zip to the temp dir.
/// Returns the zip path.
pub(crate) async fn fetch_patch2025_zip(
    client: &reqwest::Client,
    progress: &progress::Progress,
) -> Result<std::path::PathBuf, String> {
    let dest = std::env::temp_dir().join("fluxrec-patch2025.zip");
    // Reuse a valid cached copy when present (no size check — GitHub release).
    if dest.exists() {
        println!("[patch2025] reusing cached zip.");
        return Ok(dest);
    }
    download(
        client,
        PATCH2025_URL,
        &dest,
        None,
        None,
        "patch2025",
        Some((progress, "Installing 2025Patch…")),
    )
    .await?;
    defender::unblock_file(&dest);
    Ok(dest)
}

/// Install 2025Patch: extract DLL, injector, and write configured 2025patch.ini.
pub(crate) fn install_patch2025(
    target: &std::path::Path,
    patch_zip: &std::path::Path,
    ns_host: &str,
    _progress: &progress::Progress,
) -> Result<(), String> {
    use std::io::Read;
    
    let file = std::fs::File::open(patch_zip)
        .map_err(|e| format!("open patch zip: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("read patch zip: {}", e))?;
    
    // Extract 2025Patch.dll and Injector.exe next to Recroom_Release.exe
    for name in ["2025Patch.dll", "Injector.exe"] {
        let mut entry = archive.by_name(name)
            .map_err(|e| format!("patch zip missing {}: {}", name, e))?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)
            .map_err(|e| format!("read {}: {}", name, e))?;
        let dest = target.join(name);
        std::fs::write(&dest, &buf)
            .map_err(|e| format!("write {}: {}", name, e))?;
        println!("[patch2025] installed {}", name);
    }
    
    // Write configured 2025patch.ini with Flux Rec backend
    let ini_content = format!(
        "; Flux Rec 2025 patch configuration\n\
         ; Auto-generated by Flux Rec installer v0.3.2\n\
         \n\
         [config]\n\
         \n\
         ; Flux Rec backend — rewrites ns.rec.net\n\
         ApiHost={}\n\
         \n\
         ; Photon — leave empty to use server-provided hosts\n\
         PhotonHost=\n\
         PhotonPort=0\n\
         EnableConsole=false\n\
         BlockDeadHosts=true\n\
         SuppressDuidMismatch=true\n\
         EnableTracing=true\n\
         VoiceKeyXml=\n",
        ns_host
    );
    std::fs::write(target.join("2025patch.ini"), ini_content)
        .map_err(|e| format!("write 2025patch.ini: {}", e))?;
    println!("[patch2025] wrote 2025patch.ini (ApiHost={})", ns_host);
    
    // Write launcher batch files
    let screen_bat = "@echo off\ncd /d \"%~dp0\"\nstart \"\" \"Injector.exe\"\n\"Recroom_Release.exe\" +forcemode:screen\n";
    let vr_bat = "@echo off\ncd /d \"%~dp0\"\nstart \"\" \"Injector.exe\"\n\"Recroom_Release.exe\" +forcemode:vr\n";
    std::fs::write(target.join("RecRoomScreen.bat"), screen_bat)
        .map_err(|e| format!("write RecRoomScreen.bat: {}", e))?;
    std::fs::write(target.join("RecRoomVR.bat"), vr_bat)
        .map_err(|e| format!("write RecRoomVR.bat: {}", e))?;
    println!("[patch2025] wrote launcher batch files");
    
    Ok(())
}

/// Post-install steps against the live dir: hosts entry, shortcuts,
/// Defender exclusions. All fail-soft — the game is installed already.
fn finish_live_dir(dir: &Path, ns_host: &str, progress: &progress::Progress) {
    // Hosts entry for ns.rec.net: the client resolves this hostname itself
    // via System.Net.Dns, bypassing the plugin's HTTP redirect. Idempotent,
    // fail-soft.
    ensure_ns_hosts_entry(ns_host);
    // Shortcuts. Fail-soft: missing shortcuts never break the game.
    progress.set_stage("Creating shortcuts…");
    if let Err(e) = create_shortcuts(dir) {
        eprintln!("[shortcut] WARNING: shortcut step failed ({}); continuing.", e);
    }
    // Defender exclusion (defender.rs, 2026-09-24): Windows Security
    // quarantined a game file on Armin's PC, hanging the game at
    // "Connecting to server...". Exclude the game dir so it can't happen
    // again. Idempotent, fail-soft, silent.
    defender::ensure_defender_exclusions(dir);
}

/// Strict final verification: a broken install is NEVER silent.
/// Fails hard — the caller rolls back / aborts on error.
///
/// v0.3.1: verifies the 2025 install (2025Patch, native DLL injection).
/// The old BepInEx checks are gone — a 2025 tree must NOT contain them.
fn verify_install(dir: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    if find_game_exe(dir).is_none() {
        missing.push("game exe (RecRoom.exe / Recroom_Release.exe)");
    }
    // 2026 client uses RecRoom_Data, 2025 uses Recroom_Release_Data.
    let data_dir = if dir.join("RecRoom_Data").is_dir() {
        dir.join("RecRoom_Data")
    } else {
        dir.join("Recroom_Release_Data")
    };
    if !data_dir.is_dir() {
        missing.push("RecRoom_Data/ or Recroom_Release_Data/");
    }
    for rel in [
        "GameAssembly.dll",
        "Referee.dll",
        "Injector.exe",
        "2025Patch.dll",
        "2025patch.ini",
    ] {
        if !dir.join(rel).is_file() {
            missing.push(rel);
        }
    }
    let steam_settings = data_dir
        .join("Plugins")
        .join("x86_64")
        .join("steam_settings");
    if !steam_settings.join("steam_appid.txt").exists() {
        missing.push("steam_settings/steam_appid.txt (Steam bypass)");
    }
    if !steam_settings.join("steam_interfaces.txt").exists() {
        missing.push("steam_settings/steam_interfaces.txt (Steam bypass)");
    }
    if missing.is_empty() {
        println!("[verify] all critical files present: game is ready.");
        Ok(())
    } else {
        Err(format!(
            "installation verification FAILED, missing: {}.",
            missing.join(", ")
        ))
    }
}

/// Steam bypass via the Goldberg emulator (gbe_fork) — the exact approach
/// our cloud test runs use to boot the 2023 client with no Steam client
/// installed anywhere on the machine.
///
/// v0.3.1: retargeted to the 2025 client layout (`Recroom_Release_Data`,
/// not `RecRoom_Data`). Same pipeline otherwise.
///
/// What it does:
///   1. Downloads the pinned emulator release archive.
///   2. Extracts `release/regular/x64/steam_api64.dll` from it.
///   3. Backs up the stock DLL once (`steam_api64.dll.fluxrec-stock`), then
///      overwrites `Recroom_Release_Data/Plugins/x86_64/steam_api64.dll` with it.
///   4. Writes `steam_settings/steam_appid.txt` (= 471710, Rec Room's real
///      app ID, no trailing newline) + the embedded `steam_interfaces.txt`.
///   5. Deletes the legacy root `steam_appid.txt` (the old 480 trick) if a
///      previous install left one behind.
///
/// Hard-fails on download/extract errors: without the emulator the bypass
/// pipeline falls back to the minimal stub (see `bypass::apply_steam_bypass`),
/// so a broken download must surface loudly rather than silently shipping
/// the weaker fallback.
pub(crate) async fn apply_goldberg_steam_fix(
    client: &reqwest::Client,
    dir: &Path,
    progress: &progress::Progress,
) -> Result<(), String> {
    progress.set_stage("Applying Steam bypass…");
    // 2026 client uses RecRoom_Data, 2025 uses Recroom_Release_Data — support both.
    let plug_dir_2026 = dir.join("RecRoom_Data").join("Plugins").join("x86_64");
    let plug_dir_2025 = dir.join("Recroom_Release_Data").join("Plugins").join("x86_64");
    let plug_dir = if plug_dir_2026.join("steam_api64.dll").exists() {
        plug_dir_2026
    } else {
        plug_dir_2025
    };
    let stock_dll = plug_dir.join("steam_api64.dll");
    if !stock_dll.exists() {
        return Err(format!(
            "steam_api64.dll not found under {} — client extract broken?",
            plug_dir.display()
        ));
    }

    // 1. Download the emulator archive (progress feeds the GUI bar).
    let sevenz_path = dir.join("gbe.7z");
    download(
        client,
        GBE_URL,
        &sevenz_path,
        None,
        None,
        "gbe",
        Some((progress, "Applying Steam bypass…")),
    )
    .await?;
    // AV hardening (defender.rs): strip the Mark of the Web from the archive.
    defender::unblock_file(&sevenz_path);

    // 2. Extract; we only need the one win-x64 DLL.
    let extract_dir = dir.join(".gbe-extract");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
    sevenz_rust::decompress_file(&sevenz_path, &extract_dir)
        .map_err(|e| format!("steam emulator archive extract failed: {e}"))?;
    let mut gbe_dll = extract_dir.clone();
    for part in GBE_DLL_INNER.split('/') {
        gbe_dll.push(part);
    }
    if !gbe_dll.exists() {
        return Err(format!(
            "steam_api64.dll missing inside the emulator archive ({GBE_DLL_INNER})"
        ));
    }

    // 3. Back up the stock DLL once, then swap in the emulator.
    let backup_dll = plug_dir.join("steam_api64.dll.fluxrec-stock");
    if !backup_dll.exists() {
        std::fs::copy(&stock_dll, &backup_dll)
            .map_err(|e| format!("steam dll backup failed: {e}"))?;
    }
    std::fs::copy(&gbe_dll, &stock_dll)
        .map_err(|e| format!("steam emulator install failed: {e}"))?;
    // AV hardening (defender.rs): the emulator DLL is a classic heuristic
    // target and must also be loadable by the game without a MotW flag.
    defender::unblock_file(&stock_dll);

    // 4. steam_settings: app ID + interfaces list.
    let settings_dir = plug_dir.join("steam_settings");
    std::fs::create_dir_all(&settings_dir).map_err(|e| e.to_string())?;
    std::fs::write(settings_dir.join("steam_appid.txt"), STEAM_APP_ID)
        .map_err(|e| e.to_string())?;
    std::fs::write(
        settings_dir.join("steam_interfaces.txt"),
        STEAM_INTERFACES,
    )
    .map_err(|e| e.to_string())?;

    // 5. Remove the legacy root steam_appid.txt (old 480 trick) if present.
    let _ = std::fs::remove_file(dir.join("steam_appid.txt"));

    // 6. Cleanup.
    let _ = std::fs::remove_file(&sevenz_path);
    let _ = std::fs::remove_dir_all(&extract_dir);

    println!("[steam] Goldberg emulator installed (appid {STEAM_APP_ID}).");
    Ok(())
}

/// Priority: CLI arg > env var > value baked in at packaging time.
fn resolve_value(arg: Option<String>, env_name: &str, baked: &str) -> String {
    if let Some(v) = arg {
        if !v.is_empty() {
            return v;
        }
    }
    if let Ok(v) = std::env::var(env_name) {
        if !v.is_empty() {
            return v;
        }
    }
    baked.to_string()
}

/// Spawn the installer GUI on its own thread. The thread is infallible by
/// design (gui::run_gui swallows every failure); the extra catch_unwind is
/// belt-and-braces so a GUI panic can never take the install down.
pub(crate) fn spawn_gui(
    rx: std::sync::mpsc::Receiver<gui::GuiMsg>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gui::run_gui(rx)));
    })
}

/// Native Win32 message box. `error` selects the error vs warning icon.
/// A failed install must NEVER look successful — this is how v0.1.7's
/// problems went undiagnosed. Non-Windows: no-op (callers also eprintln).
#[cfg(windows)]
pub(crate) fn message_box(title: &str, text: &str, error: bool) {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONWARNING, MB_OK,
    };
    let title_h = HSTRING::from(title);
    let text_h = HSTRING::from(text);
    let style = MB_OK | if error { MB_ICONERROR } else { MB_ICONWARNING };
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_h.as_ptr()),
            PCWSTR(title_h.as_ptr()),
            style,
        );
    }
}

/// OK/Cancel variant. Returns true when the user pressed OK.
#[cfg(windows)]
pub(crate) fn message_box_ok_cancel(title: &str, text: &str) -> bool {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDOK, MB_ICONWARNING, MB_OKCANCEL};
    let title_h = HSTRING::from(title);
    let text_h = HSTRING::from(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_h.as_ptr()),
            PCWSTR(title_h.as_ptr()),
            MB_OKCANCEL | MB_ICONWARNING,
        ) == IDOK
    }
}

#[cfg(not(windows))]
pub(crate) fn message_box(_title: &str, _text: &str, _error: bool) {}

#[cfg(not(windows))]
pub(crate) fn message_box_ok_cancel(_title: &str, _text: &str) -> bool {
    true
}

/// YES/NO variant. Returns true when the user pressed YES.
/// Non-Windows: no-op returning true.
#[cfg(windows)]
pub(crate) fn message_box_yes_no(title: &str, text: &str) -> bool {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONQUESTION, MB_YESNO};
    let title_h = HSTRING::from(title);
    let text_h = HSTRING::from(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_h.as_ptr()),
            PCWSTR(title_h.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

#[cfg(not(windows))]
pub(crate) fn message_box_yes_no(_title: &str, _text: &str) -> bool {
    true
}

fn main() {
    stealth::hide_own_console(); // first: no console flash, ever
    let mut dir = default_install_dir();
    let mut dir_arg: Option<PathBuf> = None;
    let mut ns_arg: Option<String> = None;
    let mut rt_arg: Option<String> = None;
    let mut voice_arg: Option<String> = None;
    let mut chat_arg: Option<String> = None;
    let mut play_mode = false;
    let mut updated_mode = false;
    let mut silent_mode = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => {
                if let Some(v) = args.next() {
                    dir_arg = Some(PathBuf::from(v));
                }
            }
            "--ns-host" => ns_arg = args.next(),
            "--photon-rt" => rt_arg = args.next(),
            "--photon-voice" => voice_arg = args.next(),
            "--photon-chat" => chat_arg = args.next(),
            "--play" => play_mode = true,
            "--updated" => updated_mode = true,
            // v0.1.18: skip all interactive prompts (the Defender exclusion
            // guide). For scripted/headless installs where no one can click
            // a dialog — a modal prompt there hangs the install forever.
            "--silent" => silent_mode = true,
            _ => {}
        }
    }
    // --dir always wins when explicitly passed; otherwise fall back to the
    // dir persisted by the last successful install, then the default. This
    // resolution happens before --play/--updated dispatch so the launcher
    // and update flows benefit from it too.
    dir = resolve_install_dir(
        dir_arg.as_deref(),
        read_persisted_install_dir().as_deref(),
        &dir,
    );
    let ns_host = resolve_value(ns_arg, "FLUXREC_NS_HOST", NS_HOST_DEFAULT);
    let photon_rt = resolve_value(rt_arg, "FLUXREC_PHOTON_RT", PHOTON_RT_DEFAULT);
    let photon_voice = resolve_value(voice_arg, "FLUXREC_PHOTON_VOICE", PHOTON_VOICE_DEFAULT);
    let photon_chat = resolve_value(chat_arg, "FLUXREC_PHOTON_CHAT", PHOTON_CHAT_DEFAULT);

    // Launcher mode (the desktop shortcut target): update check, then game.
    // Never returns.
    if play_mode {
        launcher::run_launcher(&dir, &ns_host, &photon_rt, &photon_voice, &photon_chat);
    }

    // Install mode needs administrator rights: the VC++ runtime silent
    // install and the game-dir writes both require them. Re-launch elevated
    // once instead of failing halfway with access-denied errors.
    #[cfg(windows)]
    if !vcredist::is_admin() {
        match vcredist::relaunch_elevated() {
            Ok(()) => std::process::exit(0), // the elevated child continues
            Err(e) => {
                if updated_mode {
                    // Auto-update that can't elevate must not strand the
                    // player: launch the already-installed game instead of
                    // dying here. (UAC cancel also lands in this arm.)
                    eprintln!("[updated] elevation unavailable ({e}) — launching installed game.");
                    let (progress, rx) = progress::channel();
                    let gui_thread = spawn_gui(rx);
                    launcher::launch_game(&dir, &progress);
                    progress.done();
                    let _ = gui_thread.join();
                    std::process::exit(0);
                }
                message_box(
                    "Flux Rec Setup",
                    &format!(
                        "Could not request administrator rights: {e}\n\n\
                         Please right-click FluxRec-Setup.exe and choose \
                         \"Run as administrator\"."
                    ),
                    true,
                );
                std::process::exit(1);
            }
        }
    }

    // Install mode: pretty window + hidden console from here on.
    //
    // v0.1.17: one-time guided Windows Security exclusion setup, BEFORE any
    // download. Windows Security quarantined installer files on Armin's PC;
    // the installer never changes security settings itself, so it walks the
    // user through adding the folder exclusion manually instead. Pure UI,
    // fail-soft, shown once per install dir. Skipped with --silent (v0.1.18):
    // a modal dialog in a headless/scripted install would hang it forever.
    if !silent_mode {
        guide::maybe_show_defender_guide(&dir);
    }

    println!("Flux Rec setup — installing to {}", dir.display());
    let (progress, rx) = progress::channel();
    let gui_thread = spawn_gui(rx);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(run_install(
        &dir,
        &ns_host,
        &photon_rt,
        &photon_voice,
        &photon_chat,
        &progress,
    )) {
        eprintln!("ERROR: {e}");
        // Visible failure: a native message box with the actual error text.
        // A failed install can never again look successful.
        message_box(
            "Flux Rec Setup",
            &format!(
                "Flux Rec install failed:\n\n{e}\n\n\
                 Please take a screenshot of this message and send it to Ripo Team."
            ),
            true,
        );
        let msg = format!("Install failed: {e}");
        let short = msg.chars().take(90).collect::<String>();
        progress.set_status(&short, 100);
        std::thread::sleep(Duration::from_secs(8));
        progress.done();
        let _ = gui_thread.join();
        std::process::exit(1);
    }
    if updated_mode {
        // Spawned by the launcher for an update: the update is fully
        // installed now, so launch the game before exiting.
        launcher::launch_game(&dir, &progress);
    } else {
        progress.set_status("Install complete!", 100);
        std::thread::sleep(Duration::from_secs(2));
    }
    progress.done();
    let _ = gui_thread.join();
    println!("DONE — launch Flux Rec from the Start Menu or desktop shortcut.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fluxrec-logo-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_gz(dir: &Path, payload: &[u8], name: &str) -> PathBuf {
        let p = dir.join(name);
        let f = std::fs::File::create(&p).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        p
    }

    fn md5_of(bytes: &[u8]) -> String {
        format!("{:x}", md5::compute(bytes))
    }

    #[test]
    fn upgrade_skips_client_download_only_when_2025_exe_present() {
        // v0.4.1: Force re-download to upgrade from 2022 to 2026 client.
        // Never skip the download.
        let with_exe = tmp_dir("upgrade-with-exe");
        std::fs::write(with_exe.join("Recroom_Release.exe"), b"fake-exe").unwrap();
        assert!(!should_skip_client_download(&with_exe));

        // The lowercase variant counts as present too.
        let lower = tmp_dir("upgrade-lower");
        std::fs::write(lower.join("recroom_release.exe"), b"fake-exe").unwrap();
        assert!(!should_skip_client_download(&lower));

        // A STALE 2023 install must NEVER count as a valid game.
        // 2023 is identified by BepInEx/Doorstop files, NOT by RecRoom.exe
        // (the 2026 client also uses RecRoom.exe).
        let old = tmp_dir("upgrade-2023");
        std::fs::write(old.join("RecRoom.exe"), b"fake-exe").unwrap();
        std::fs::create_dir_all(old.join("BepInEx")).unwrap();
        assert!(!should_skip_client_download(&old));
        assert!(is_2023_install(&old));

        // BepInEx / Doorstop leftovers also mark a 2023 tree.
        let bep = tmp_dir("upgrade-2023-bepinex");
        std::fs::create_dir_all(bep.join("BepInEx")).unwrap();
        assert!(is_2023_install(&bep));
        assert!(!should_skip_client_download(&bep));
        let door = tmp_dir("upgrade-2023-doorstop");
        std::fs::write(door.join("winhttp.dll"), b"fake").unwrap();
        assert!(is_2023_install(&door));

        // A dir with other files but no game exe: no skip (fresh install).
        let no_exe = tmp_dir("upgrade-no-exe");
        std::fs::write(no_exe.join("some-other-file.txt"), b"junk").unwrap();
        assert!(!should_skip_client_download(&no_exe));
        assert!(!is_2023_install(&no_exe));

        // A clean 2025 tree is not a 2023 install.
        assert!(!is_2023_install(&with_exe));

        for d in [&with_exe, &lower, &old, &bep, &door, &no_exe] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn resolve_install_dir_priority_arg_persisted_default() {
        let default = PathBuf::from(r"C:\Games\FluxRec");
        let arg = Path::new(r"D:\MyGames\FluxRec");
        let persisted = r"C:\Games\FluxRec-Old";

        // Explicit --dir wins over everything.
        assert_eq!(
            resolve_install_dir(Some(arg), Some(persisted), &default),
            arg
        );
        // Persisted dir wins over the default.
        assert_eq!(
            resolve_install_dir(None, Some(persisted), &default),
            PathBuf::from(persisted)
        );
        // Blank persisted content falls back to the default.
        assert_eq!(resolve_install_dir(None, Some("   \n"), &default), default);
        assert_eq!(resolve_install_dir(None, None, &default), default);
    }

    #[test]
    fn persisted_install_dir_roundtrip_trims_and_ignores_empty() {
        let root = tmp_dir("persisted");
        let file = install_dir_file_in(root.to_str().unwrap());
        assert_eq!(file, root.join("FluxRec").join("install_dir.txt"));

        // Missing file -> None.
        assert_eq!(read_install_dir_file(&file), None);

        let dir = PathBuf::from(r"C:\Games\FluxRec");
        write_install_dir_file(&file, &dir);
        assert!(file.exists());
        assert_eq!(
            read_install_dir_file(&file),
            Some(dir.display().to_string())
        );

        // The writer's trailing newline must not change resolution.
        let raw = std::fs::read_to_string(&file).unwrap();
        assert!(raw.ends_with('\n'));
        assert_eq!(
            resolve_install_dir(None, Some(&raw), &PathBuf::from(r"D:\other")),
            dir
        );

        // Empty/blank file -> None (caller falls back to default).
        std::fs::write(&file, "  \n ").unwrap();
        assert_eq!(read_install_dir_file(&file), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn logo_install_happy_path_backs_up_once_and_replaces() {
        let d = tmp_dir("happy");
        let target = d.join("ui.bundle");
        std::fs::write(&target, b"stock-bytes").unwrap();
        let payload = b"patched-logo-bytes";
        let gz = make_gz(&d, payload, "logo.gz");

        install_logo_bundle(&gz, &target, &md5_of(payload), payload.len() as u64).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), payload);
        let backup = target.with_extension("bundle.stock");
        assert_eq!(std::fs::read(&backup).unwrap(), b"stock-bytes");
        // No temp leftovers.
        assert!(!target.with_extension("bundle.logo-new").exists());

        // Second run with a *different* payload must not overwrite the backup.
        let payload2 = b"patched-logo-bytes-v2";
        let gz2 = make_gz(&d, payload2, "logo2.gz");
        // Pretend the first payload is the "stock" again by resetting target.
        std::fs::write(&target, b"stock-bytes").unwrap();
        install_logo_bundle(&gz2, &target, &md5_of(payload2), payload2.len() as u64).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), payload2);
        assert_eq!(std::fs::read(&backup).unwrap(), b"stock-bytes");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn logo_install_corrupt_gzip_keeps_target() {
        let d = tmp_dir("corrupt");
        let target = d.join("ui.bundle");
        std::fs::write(&target, b"stock-bytes").unwrap();
        let gz = d.join("bad.gz");
        std::fs::write(&gz, b"this is not gzip data at all").unwrap();

        let err = install_logo_bundle(&gz, &target, "deadbeef", 10).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), b"stock-bytes");
        assert!(!target.with_extension("bundle.stock").exists());
        assert!(!target.with_extension("bundle.logo-new").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn logo_install_hash_mismatch_keeps_target() {
        let d = tmp_dir("mismatch");
        let target = d.join("ui.bundle");
        std::fs::write(&target, b"stock-bytes").unwrap();
        let payload = b"patched-logo-bytes";
        let gz = make_gz(&d, payload, "logo.gz");

        // Wrong expected hash: verification must fail and the target stays stock.
        let err =
            install_logo_bundle(&gz, &target, &md5_of(b"something-else"), payload.len() as u64)
                .unwrap_err();
        assert!(err.contains("verification"));
        assert_eq!(std::fs::read(&target).unwrap(), b"stock-bytes");
        assert!(!target.with_extension("bundle.stock").exists());
        assert!(!target.with_extension("bundle.logo-new").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
