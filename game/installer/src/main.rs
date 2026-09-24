// FluxRec-Setup — zero-touch installer for the RecFlare-pipeline Flux Rec client.
//
// Flow:
//   1. Download the 2023 Rec Room client zip from the public mirror, verify MD5,
//      extract into the install dir. (We host no game binaries ourselves.)
//   1b. Download the Flux Rec logo bundle (gzipped patched Addressables UI
//      bundle, hosted on our Hugging Face dataset), verify MD5, gunzip, and
//      overwrite the stock bundle so the loading screen shows Flux Rec
//      branding. The stock bundle is backed up as *.bundle.stock once.
//   2. Download BepInEx 6.0.0-pre.2 (Unity IL2CPP win-x64), extract into the dir.
//   3. Download RecNetPlugin.dll (20230414.2) into BepInEx/plugins/.
//   4. Write BepInEx/config/net.rec.plugin.cfg — ns host and Photon App IDs
//      baked in at packaging time from FLUXREC_NS_HOST / FLUXREC_PHOTON_RT /
//      FLUXREC_PHOTON_VOICE / FLUXREC_PHOTON_CHAT env vars (or pass --ns-host /
//      --photon-rt / --photon-voice / --photon-chat at install time).
//   5. Steam bypass via the Goldberg emulator (gbe_fork): the game's
//      steam_api64.dll is swapped for the emulator (stock backed up once),
//      steam_settings/steam_appid.txt = 471710 + steam_interfaces.txt are
//      written beside it. No Steam client installed, none needed.
//   6. Create Start Menu + desktop shortcuts (Windows only).
//
// Exit 0 on success, 1 with an ERROR line on failure.
// Usage: FluxRec-Setup [--dir <path>] [--ns-host <url>] [--photon-rt <id>] [--photon-voice <id>] [--photon-chat <id>]
//        FluxRec-Setup --play [--dir <path>]      (launcher mode: update check, then launch the game)
//        FluxRec-Setup --updated [--dir <path>]  (spawned by the launcher: install, then launch the game)

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod assets;
mod gui;
mod launcher;
mod progress;
mod stealth;
mod updater;

const CLIENT_ZIP_URL: &str =
    "https://s3.g.megas4.com/2koayuyiwxv4groxzwdbbxg43cwustavrkvfb/recflare/client.zip";
const CLIENT_ZIP_MD5: &str = "4c4a94624eba99028bb36445ccb03253";
const BEPINEX_URL: &str = "https://github.com/BepInEx/BepInEx/releases/download/v6.0.0-pre.2/BepInEx-Unity.IL2CPP-win-x64-6.0.0-pre.2.zip";
const BEPINEX_SIZE: u64 = 34_146_254;
const PLUGIN_URL: &str =
    "https://github.com/recflare/patch/releases/download/20230414.2/RecNetPlugin.dll";
const PLUGIN_SIZE: u64 = 45_056;
/// Flux Rec logo bundle: gzipped patched Addressables UI bundle (loading
/// screen logos replaced). Hosted on our Hugging Face dataset; verified by
/// MD5 before use, then gunzipped over the stock bundle.
const LOGO_BUNDLE_URL: &str = "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/logo-bundle/logo-bundle.gz";
const LOGO_BUNDLE_MD5: &str = "537b8583e64f27469874deb7ee00a32f";
const LOGO_BUNDLE_SIZE: u64 = 39_155_393;
const LOGO_BUNDLE_NAME: &str = "682ba40059cd6c037bace975e7aea07f.bundle";
const LOGO_BUNDLE_UNZIPPED_SIZE: u64 = 86_361_811;
/// MD5 of the gunzipped patched bundle (Flux Rec logo replacement).
const LOGO_BUNDLE_UNZIPPED_MD5: &str = "47f1cd2a2c6004d196a539d208a7358d";

/// Goldberg Steam emulator (gbe_fork) release — pinned. This is the exact
/// archive our cloud test runs use to make the 2023 client boot with no
/// Steam client installed.
const GBE_URL: &str =
    "https://github.com/Detanup01/gbe_fork/releases/download/release-2026_09_16_2/emu-win-release-vs22.7z";
/// Path of the win-x64 emulator DLL inside the archive.
const GBE_DLL_INNER: &str = "release/regular/x64/steam_api64.dll";
/// Rec Room's real Steam app ID (matches our cloud test setup).
const STEAM_APP_ID: &str = "471710";
/// steam_interfaces.txt, embedded from assets/ (same file the cloud tests copy).
const STEAM_INTERFACES: &str = include_str!("../assets/steam_interfaces.txt");

const NS_PLACEHOLDER: &str = "%%FLUXREC_NS%%";
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
const MAX_ATTEMPTS: u32 = 3;

fn default_install_dir() -> PathBuf {
    let drive = std::env::var("SYSTEMDRIVE").unwrap_or_else(|_| "C:".to_string());
    PathBuf::from(format!("{drive}\\Games\\FluxRec"))
}

fn md5_of_file(path: &Path) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, f);
    let mut ctx = md5::Context::new();
    std::io::copy(&mut reader, &mut ctx).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", ctx.compute()))
}

fn file_ok(path: &Path, expected_md5: Option<&str>, expected_size: Option<u64>) -> bool {
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
async fn download(
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
        let resp = req.send().await.map_err(|e| e.to_string())?;
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

fn extract_zip(zip_path: &Path, dest_dir: &Path, label: &str) -> Result<(), String> {
    println!("[{label}] extracting...");
    let f = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let n = archive.len();
    archive.extract(dest_dir).map_err(|e| e.to_string())?;
    println!("[{label}] extracted {n} entries.");
    Ok(())
}

fn write_plugin_config(dir: &Path, ns_host: &str, rt: &str, voice: &str, chat: &str) -> Result<(), String> {
    let cfg_dir = dir.join("BepInEx").join("config");
    std::fs::create_dir_all(&cfg_dir).map_err(|e| e.to_string())?;
    let cfg = format!(
        "## Flux Rec — RecNet plugin config (plugin GUID net.rec.plugin, v1.0.0)\n\
         ## Values baked in at packaging time; override at install time with\n\
         ## --ns-host / --photon-rt / --photon-voice / --photon-chat or the\n\
         ## FLUXREC_NS_HOST / FLUXREC_PHOTON_RT / FLUXREC_PHOTON_VOICE / FLUXREC_PHOTON_CHAT env vars.\n\
         \n\
         [Server]\n\
         RecNet NameServer Host = {ns_host}\n\
         \n\
         [Photon]\n\
         App Id Realtime = {rt}\n\
         App Id Voice = {voice}\n\
         App Id Chat = {chat}\n\
         \n\
         [Advanced]\n\
         Enabled Advanced Settings = false\n\
         Suppress DUID Mismatch = true\n\
         Debug = false\n\
         \n\
         [Signing]\n\
         Disable Signature Verification = true\n\
         \n\
         [Analytics]\n\
         Disable Telemetry = true\n"
    );
    std::fs::write(cfg_dir.join("net.rec.plugin.cfg"), cfg).map_err(|e| e.to_string())?;
    println!("[config] wrote BepInEx/config/net.rec.plugin.cfg (ns host: {ns_host}).");
    Ok(())
}

/// Keep the BepInEx console window from popping at game launch.
///
/// Appends `[Logging.Console] Enabled = false` to BepInEx.cfg when no
/// `[Logging.Console]` section exists yet. Never rewrites or clobbers an
/// existing config — purely additive. Fail-soft: the launcher also spawns
/// the game with CREATE_NO_WINDOW, so this is belt-and-braces.
fn write_bepinex_console_config(dir: &Path) -> Result<(), String> {
    let cfg_path = dir.join("BepInEx").join("config").join("BepInEx.cfg");
    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    if existing.contains("[Logging.Console]") {
        println!("[config] BepInEx.cfg already has [Logging.Console], leaving it.");
        return Ok(());
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[Logging.Console]\nEnabled = false\n");
    std::fs::write(&cfg_path, out).map_err(|e| e.to_string())?;
    println!("[config] disabled BepInEx console window in BepInEx.cfg.");
    Ok(())
}

#[cfg(windows)]
fn ps_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Windows-only: create a .lnk via WScript.Shell. Skipped elsewhere.
fn create_shortcut(lnk: &Path, target: &Path, args: &str, workdir: &Path) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (lnk, target, args, workdir);
        println!("[shortcut] skipped (not Windows): {}", lnk.display());
        return Ok(());
    }
    #[cfg(windows)]
    {
        let cmd = format!(
            "$ws = New-Object -ComObject WScript.Shell; \
             $sc = $ws.CreateShortcut('{lnk}'); \
             $sc.TargetPath = '{tgt}'; $sc.Arguments = '{args}'; \
             $sc.WorkingDirectory = '{wd}'; $sc.Description = 'Flux Rec'; $sc.Save()",
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

/// Game exe: the mirror ships RecRoom.exe (capitalized). Fall back to
/// lowercase for robustness.
pub(crate) fn find_game_exe(dir: &Path) -> Option<PathBuf> {
    for name in ["RecRoom.exe", "recroom.exe"] {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
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
        create_shortcut(&start_menu, &target, args, dir)?;
        create_shortcut(&desktop_lnk, &target, args, dir)?;
    }
    #[cfg(not(windows))]
    {
        create_shortcut(&PathBuf::from("Flux Rec.lnk"), &target, args, dir)?;
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

async fn apply_logo_bundle(
    client: &reqwest::Client,
    dir: &Path,
    progress: &progress::Progress,
) -> Result<(), String> {
    // The stock UI bundle inside the extracted client layout.
    let target = dir
        .join("RecRoom_Data")
        .join("StreamingAssets")
        .join("aa")
        .join("StandaloneWindows64")
        .join(LOGO_BUNDLE_NAME);

    if !target.exists() {
        println!("[logo] target bundle not found (fresh layout?), skipping.");
        return Ok(());
    }

    // Idempotency: if the installed bundle already has our patched hash, done.
    if let Ok(h) = md5_of_file(&target) {
        if h.eq_ignore_ascii_case(LOGO_BUNDLE_UNZIPPED_MD5) {
            println!("[logo] Flux Rec logo bundle already applied, skipping.");
            return Ok(());
        }
    }

    // Download the gzipped patched bundle (verified by MD5 + size).
    let gz_path = dir.join("logo-bundle.gz");
    download(
        client,
        LOGO_BUNDLE_URL,
        &gz_path,
        Some(LOGO_BUNDLE_MD5),
        Some(LOGO_BUNDLE_SIZE),
        "logo-bundle",
        Some((progress, "Applying Flux Rec branding…")),
    )
    .await?;

    let res = install_logo_bundle(
        &gz_path,
        &target,
        LOGO_BUNDLE_UNZIPPED_MD5,
        LOGO_BUNDLE_UNZIPPED_SIZE,
    );
    let _ = std::fs::remove_file(&gz_path); // free ~39MB either way
    res
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
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent("FluxRec-Setup/0.1.0")
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    // 1. Game client.
    let client_zip = dir.join("client.zip");
    download(
        &client,
        CLIENT_ZIP_URL,
        &client_zip,
        Some(CLIENT_ZIP_MD5),
        None,
        "client",
        Some((progress, "Downloading game files…")),
    )
    .await?;
    progress.set_stage("Extracting game files…");
    extract_zip(&client_zip, dir, "client")?;
    let _ = std::fs::remove_file(&client_zip); // free ~3.8GB after extract

    // 1a. Steam bypass FIRST: the game cannot boot without this, and no
    // later step may ever prevent it from being in place. This swaps the
    // stock steam_api64.dll for the Goldberg emulator (the setup our cloud
    // test runs prove boots with no Steam client installed). Hard-fails:
    // without a working Steam bypass the install is unplayable, so fail
    // loudly instead of shipping a broken game.
    apply_goldberg_steam_fix(&client, dir, progress).await?;

    // 1b. Flux Rec logo bundle: patched loading-screen bundle over the stock one.
    // Never triggers a full client re-download: applies in place, stock backed up once.
    // FAIL-SOFT: the logo is cosmetic. If it fails for any reason, warn and
    // continue — the game must always end up fully installed and playable.
    if let Err(e) = apply_logo_bundle(&client, dir, progress).await {
        eprintln!("[logo] WARNING: logo bundle step failed ({}); continuing without it.", e);
    }

    // 2. BepInEx.
    let bepinex_zip = dir.join("bepinex.zip");
    download(
        &client,
        BEPINEX_URL,
        &bepinex_zip,
        None,
        Some(BEPINEX_SIZE),
        "bepinex",
        Some((progress, "Installing BepInEx…")),
    )
    .await?;
    extract_zip(&bepinex_zip, dir, "bepinex")?;
    let _ = std::fs::remove_file(&bepinex_zip);

    // 3. Redirect plugin. Fail-soft: warn and continue so a transient
    // download hiccup can never leave the game without its Steam bypass.
    let plugins_dir = dir.join("BepInEx").join("plugins");
    let plugin_res: Result<(), String> = async {
        std::fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
        download(
            &client,
            PLUGIN_URL,
            &plugins_dir.join("RecNetPlugin.dll"),
            None,
            Some(PLUGIN_SIZE),
            "plugin",
            Some((progress, "Installing Flux Rec plugin…")),
        )
        .await
    }
    .await;
    if let Err(e) = plugin_res {
        eprintln!("[plugin] WARNING: plugin step failed ({}); continuing.", e);
    }

    // 4. Plugin config (ns host + Photon IDs baked at packaging time).
    // Fail-soft for the same reason as above.
    progress.set_stage("Writing configuration…");
    if let Err(e) = write_plugin_config(dir, ns_host, photon_rt, photon_voice, photon_chat) {
        eprintln!("[config] WARNING: plugin config step failed ({}); continuing.", e);
    }
    // 4b. Keep the BepInEx console window off at game launch (belt-and-braces
    // alongside the hidden spawn flags used by the launcher).
    if let Err(e) = write_bepinex_console_config(dir) {
        eprintln!("[config] WARNING: bepinex console config failed ({}); continuing.", e);
    }

    // 6. Shortcuts. Fail-soft: missing shortcuts never break the game.
    progress.set_stage("Creating shortcuts…");
    if let Err(e) = create_shortcuts(dir) {
        eprintln!("[shortcut] WARNING: shortcut step failed ({}); continuing.", e);
    }

    // 7. Final verification: report exactly what is (and isn't) in place,
    // so a broken install is never silent.
    progress.set_stage("Final checks…");
    let mut missing = Vec::new();
    if find_game_exe(dir).is_none() {
        missing.push("RecRoom.exe");
    }
    let steam_settings = dir
        .join("RecRoom_Data")
        .join("Plugins")
        .join("x86_64")
        .join("steam_settings");
    if !steam_settings.join("steam_appid.txt").exists() {
        missing.push("steam_settings/steam_appid.txt (Steam bypass)");
    }
    if !steam_settings.join("steam_interfaces.txt").exists() {
        missing.push("steam_settings/steam_interfaces.txt (Steam bypass)");
    }
    if !dir.join("BepInEx").exists() {
        missing.push("BepInEx");
    }
    if !plugins_dir.join("RecNetPlugin.dll").exists() {
        missing.push("RecNetPlugin.dll");
    }
    if missing.is_empty() {
        println!("[verify] all critical files present: game is ready.");
    } else {
        eprintln!("[verify] WARNING: missing: {}.", missing.join(", "));
    }

    Ok(())
}

/// Steam bypass via the Goldberg emulator (gbe_fork) — the exact approach
/// our cloud test runs use to boot the 2023 client with no Steam client
/// installed anywhere on the machine.
///
/// What it does:
///   1. Downloads the pinned emulator release archive.
///   2. Extracts `release/regular/x64/steam_api64.dll` from it.
///   3. Backs up the stock DLL once (`steam_api64.dll.fluxrec-stock`), then
///      overwrites `RecRoom_Data/Plugins/x86_64/steam_api64.dll` with it.
///   4. Writes `steam_settings/steam_appid.txt` (= 471710, Rec Room's real
///      app ID, no trailing newline) + the embedded `steam_interfaces.txt`.
///   5. Deletes the legacy root `steam_appid.txt` (the old 480 trick) if a
///      previous install left one behind.
///
/// Hard-fails: without this the game shows "Failed to initialize Steam
/// Platform" and cannot boot, so a broken download must fail loudly rather
/// than ship an unplayable install.
async fn apply_goldberg_steam_fix(
    client: &reqwest::Client,
    dir: &Path,
    progress: &progress::Progress,
) -> Result<(), String> {
    progress.set_stage("Applying Steam bypass…");
    let plug_dir = dir.join("RecRoom_Data").join("Plugins").join("x86_64");
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

fn main() {
    stealth::hide_own_console(); // first: no console flash, ever
    let mut dir = default_install_dir();
    let mut ns_arg: Option<String> = None;
    let mut rt_arg: Option<String> = None;
    let mut voice_arg: Option<String> = None;
    let mut chat_arg: Option<String> = None;
    let mut play_mode = false;
    let mut updated_mode = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => {
                if let Some(v) = args.next() {
                    dir = PathBuf::from(v);
                }
            }
            "--ns-host" => ns_arg = args.next(),
            "--photon-rt" => rt_arg = args.next(),
            "--photon-voice" => voice_arg = args.next(),
            "--photon-chat" => chat_arg = args.next(),
            "--play" => play_mode = true,
            "--updated" => updated_mode = true,
            _ => {}
        }
    }
    let ns_host = resolve_value(ns_arg, "FLUXREC_NS_HOST", NS_HOST_DEFAULT);
    let photon_rt = resolve_value(rt_arg, "FLUXREC_PHOTON_RT", PHOTON_RT_DEFAULT);
    let photon_voice = resolve_value(voice_arg, "FLUXREC_PHOTON_VOICE", PHOTON_VOICE_DEFAULT);
    let photon_chat = resolve_value(chat_arg, "FLUXREC_PHOTON_CHAT", PHOTON_CHAT_DEFAULT);

    // Launcher mode (the desktop shortcut target): update check, then game.
    // Never returns.
    if play_mode {
        launcher::run_launcher(&dir, &ns_host, &photon_rt, &photon_voice, &photon_chat);
    }

    // Install mode: pretty window + hidden console from here on.
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
