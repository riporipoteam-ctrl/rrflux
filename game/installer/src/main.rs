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
//   5. Write steam_appid.txt = "480" beside recroom.exe (Steam bypass).
//   6. Create Start Menu + desktop shortcuts (Windows only).
//
// Exit 0 on success, 1 with an ERROR line on failure.
// Usage: FluxRec-Setup [--dir <path>] [--ns-host <url>] [--photon-rt <id>] [--photon-voice <id>] [--photon-chat <id>]

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_md5: Option<&str>,
    expected_size: Option<u64>,
    label: &str,
) -> Result<(), String> {
    if file_ok(dest, expected_md5, expected_size) {
        println!("[{label}] already present and verified, skipping.");
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
        let out = std::process::Command::new("powershell")
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
fn find_game_exe(dir: &Path) -> Option<PathBuf> {
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
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA").map_err(|e| e.to_string())?;
        let start_menu = PathBuf::from(format!(
            "{appdata}\\Microsoft\\Windows\\Start Menu\\Programs\\Flux Rec.lnk"
        ));
        let desktop = std::process::Command::new("powershell")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "[Environment]::GetFolderPath('Desktop')",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        let desktop_dir = String::from_utf8_lossy(&desktop.stdout).trim().to_string();
        let desktop_lnk = PathBuf::from(format!("{desktop_dir}\\Flux Rec.lnk"));
        create_shortcut(&start_menu, &exe, "+forcemode:screen", dir)?;
        create_shortcut(&desktop_lnk, &exe, "+forcemode:screen", dir)?;
    }
    #[cfg(not(windows))]
    {
        create_shortcut(&PathBuf::from("Flux Rec.lnk"), &exe, "+forcemode:screen", dir)?;
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
    std::fs::rename(&tmp_path, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        e.to_string()
    })?;
    println!("[logo] Flux Rec logo bundle applied.");
    Ok(())
}

async fn apply_logo_bundle(client: &reqwest::Client, dir: &Path) -> Result<(), String> {
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

async fn run(dir: &Path, ns_host: &str, photon_rt: &str, photon_voice: &str, photon_chat: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent("FluxRec-Setup/0.1.0")
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    // 1. Game client.
    let client_zip = dir.join("client.zip");
    download(&client, CLIENT_ZIP_URL, &client_zip, Some(CLIENT_ZIP_MD5), None, "client").await?;
    extract_zip(&client_zip, dir, "client")?;
    let _ = std::fs::remove_file(&client_zip); // free ~3.8GB after extract

    // 1b. Flux Rec logo bundle: patched loading-screen bundle over the stock one.
    // Never triggers a full client re-download: applies in place, stock backed up once.
    apply_logo_bundle(&client, dir).await?;

    // 2. BepInEx.
    let bepinex_zip = dir.join("bepinex.zip");
    download(&client, BEPINEX_URL, &bepinex_zip, None, Some(BEPINEX_SIZE), "bepinex").await?;
    extract_zip(&bepinex_zip, dir, "bepinex")?;
    let _ = std::fs::remove_file(&bepinex_zip);

    // 3. Redirect plugin.
    let plugins_dir = dir.join("BepInEx").join("plugins");
    std::fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;
    download(
        &client,
        PLUGIN_URL,
        &plugins_dir.join("RecNetPlugin.dll"),
        None,
        Some(PLUGIN_SIZE),
        "plugin",
    )
    .await?;

    // 4. Plugin config (ns host + Photon IDs baked at packaging time).
    write_plugin_config(dir, ns_host, photon_rt, photon_voice, photon_chat)?;

    // 5. Steam bypass.
    std::fs::write(dir.join("steam_appid.txt"), "480").map_err(|e| e.to_string())?;
    println!("[steam] wrote steam_appid.txt = 480.");

    // 6. Shortcuts.
    create_shortcuts(dir)?;

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

fn main() {
    let mut dir = default_install_dir();
    let mut ns_arg: Option<String> = None;
    let mut rt_arg: Option<String> = None;
    let mut voice_arg: Option<String> = None;
    let mut chat_arg: Option<String> = None;
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
            _ => {}
        }
    }
    let ns_host = resolve_value(ns_arg, "FLUXREC_NS_HOST", NS_HOST_DEFAULT);
    let photon_rt = resolve_value(rt_arg, "FLUXREC_PHOTON_RT", PHOTON_RT_DEFAULT);
    let photon_voice = resolve_value(voice_arg, "FLUXREC_PHOTON_VOICE", PHOTON_VOICE_DEFAULT);
    let photon_chat = resolve_value(chat_arg, "FLUXREC_PHOTON_CHAT", PHOTON_CHAT_DEFAULT);
    println!("Flux Rec setup — installing to {}", dir.display());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(run(&dir, &ns_host, &photon_rt, &photon_voice, &photon_chat)) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
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
