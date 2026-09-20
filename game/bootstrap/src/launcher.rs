// Flux Rec launcher (default mode of the bootstrap binary).
//
// The launcher is thin in v0.4.0+: the persistent backend owns auth and
// the local translator. The launcher only
//   0. self-updates, refreshes game files, deploys the Steam emulator,
//   1. makes sure the backend (FluxRec-backend.exe, scheduled task
//      "FluxRecBackend" with a logon trigger) is installed, current, and
//      healthy — replacing an outdated backend safely,
//   2. registers WER crash dumps for RecRoom.exe,
//   3. starts RecRoom.exe with the game dir as its working directory,
//   4. waits for it, then records the exit code, grabs the Unity
//      Player.log, and reports crashes.
//
// `--setup-backend` (used by the installer) only does step 1 and exits.

use crate::util;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

// Clean-room Steam emulator (v0.4.0+): exact 995 exports of steam_api64.dll
// (SDK 1.48), real interface objects, fake-but-valid identity and tickets.
// Lets the 2022 client boot with no Steam installed, ever.
// Auditable source: game/bootstrap/steam_emu/.
const STEAM_EMULATOR: &[u8] = include_bytes!("steam_api64_stub.dll");

const BACKEND_TASK: &str = "FluxRecBackend";
const BACKEND_EXE: &str = "FluxRec-backend.exe";
const DETACHED_PROCESS: u32 = 0x00000008;

struct BackendInfo {
    version: String,
    pid: u32,
}

/// HTTPS client trusting our own local CA (backend serves https://localhost
/// with a cert signed by it).
fn local_ca_client() -> Option<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(3));
    if let Some(dir) = util::data_dir_opt() {
        let ca_path = dir.join("certs").join("ca.pem");
        if let Ok(pem) = std::fs::read(&ca_path) {
            if let Ok(ca) = reqwest::Certificate::from_pem(&pem) {
                builder = builder.add_root_certificate(ca);
            }
        }
    }
    builder.build().ok()
}

/// Ask the backend who it is. None = not running / not ours.
async fn backend_health() -> Option<BackendInfo> {
    let c = local_ca_client()?;
    let v: serde_json::Value = c
        .get("https://127.0.0.1/health")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if v.get("app").and_then(|a| a.as_str()) != Some("fluxrec") {
        return None;
    }
    Some(BackendInfo {
        version: v
            .get("version")
            .and_then(|x| x.as_str())
            .unwrap_or("0")
            .to_string(),
        pid: v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
    })
}

/// Ask an outdated backend to shut itself down (loopback-only endpoint),
/// then make sure it's actually gone.
async fn stop_backend(info: &BackendInfo) {
    if let Some(c) = local_ca_client() {
        let _ = c.post("https://127.0.0.1/shutdown").send().await;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if backend_health().await.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Still there — kill by pid from /health.
    if info.pid != 0 {
        let _ = Command::new("taskkill")
            .args(["/PID", &info.pid.to_string(), "/F"])
            .output();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// (Re)create the "FluxRecBackend" scheduled task with a logon trigger.
/// Idempotent (/f overwrites). Falls back to the HKCU Run key if schtasks
/// is unavailable.
fn create_backend_task(backend_exe: &Path) {
    let tr = format!("\"{}\" --backend", backend_exe.to_string_lossy());
    let out = Command::new("schtasks")
        .args(["/create", "/tn", BACKEND_TASK, "/tr", &tr, "/sc", "onlogon", "/f"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            util::crash_log("backend task: scheduled task \"FluxRecBackend\" (logon trigger) created");
        }
        _ => {
            util::crash_log("backend task: schtasks failed, falling back to HKCU Run key");
            let _ = Command::new("reg")
                .args([
                    "add",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    BACKEND_TASK,
                    "/t",
                    "REG_SZ",
                    "/d",
                    &tr,
                    "/f",
                ])
                .output();
        }
    }
}

/// Start the backend right now via the scheduled task. (If the task
/// doesn't take, ensure_backend falls back to a detached spawn.)
fn start_backend_now(_backend_exe: &Path) {
    let _ = Command::new("schtasks")
        .args(["/run", "/tn", BACKEND_TASK])
        .output();
}

/// Make sure a backend of OUR version is healthy. Deploys the backend copy,
/// (re)creates the logon task, starts it, and waits for it. Replaces an
/// outdated backend safely (asks it to shut down first).
async fn ensure_backend(exe_dir: &Path) -> BackendInfo {
    let me = env!("CARGO_PKG_VERSION");
    if let Some(info) = backend_health().await {
        if info.version == me {
            util::crash_log(&format!(
                "backend v{} already healthy (pid {})",
                info.version, info.pid
            ));
            return info;
        }
        util::crash_log(&format!(
            "backend v{} is outdated (launcher is v{me}); replacing",
            info.version
        ));
        stop_backend(&info).await;
    }

    // Deploy the backend copy of this exact binary.
    let this_exe = std::env::current_exe().unwrap_or_else(|_| exe_dir.join("Flux Rec.exe"));
    let backend_exe = exe_dir.join(BACKEND_EXE);
    match std::fs::copy(&this_exe, &backend_exe) {
        Ok(_) => util::crash_log(&format!(
            "backend: deployed {}",
            backend_exe.to_string_lossy()
        )),
        Err(e) => util::fatal(format!("Couldn't install the Flux Rec backend:\n{e}")),
    }

    create_backend_task(&backend_exe);
    start_backend_now(&backend_exe);
    // schtasks /run can be slow to take; if the backend still isn't up
    // after a few seconds, spawn it detached directly as a fallback.
    tokio::time::sleep(Duration::from_secs(4)).await;
    if backend_health().await.is_none() {
        util::crash_log("backend: schtasks /run didn't take; spawning detached");
        match Command::new(&backend_exe)
            .arg("--backend")
            .creation_flags(DETACHED_PROCESS)
            .spawn()
        {
            Ok(_) => {}
            Err(e) => util::fatal(format!("Couldn't start the Flux Rec backend:\n{e}")),
        }
    }

    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(info) = backend_health().await {
            if info.version == me {
                util::crash_log(&format!(
                    "backend v{} healthy (pid {})",
                    info.version, info.pid
                ));
                return info;
            }
        }
        if Instant::now() > deadline {
            util::fatal(util::describe_bind_error(
                "the Flux Rec backend did not become healthy",
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// `--setup-backend`: what the installer runs. Deploys the backend copy,
/// creates the logon task, starts it now. Quiet; exit code is the result.
pub async fn setup_backend() -> i32 {
    let exe_dir = util::exe_dir();
    let dir = util::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let this_exe = std::env::current_exe().unwrap_or_else(|_| exe_dir.join("Flux Rec.exe"));
    let backend_exe = exe_dir.join(BACKEND_EXE);
    if let Err(e) = std::fs::copy(&this_exe, &backend_exe) {
        util::crash_log(&format!("setup-backend: copy failed: {e}"));
        return 1;
    }
    create_backend_task(&backend_exe);
    start_backend_now(&backend_exe);
    util::crash_log("setup-backend: backend deployed, task created, start requested");
    0
}

fn game_running() -> bool {
    let out = match Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq RecRoom.exe", "/NH"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.to_lowercase().starts_with("recroom.exe"))
}

pub async fn run() {
    let exe_dir = util::exe_dir();
    let game_dir = exe_dir.join("game");
    let game_exe = game_dir.join("RecRoom.exe");
    let dir = util::data_dir();
    let _ = std::fs::create_dir_all(&dir);

    // The backend is persistent now, so the single-instance guard watches
    // the game instead of the translator: a second double-click while the
    // game runs exits quietly.
    if game_running() {
        return;
    }

    // 0. Self-update: if a newer bootstrapper is published, download it,
    //    hand it to the self-updater, and exit — the new copy takes over.
    //    Fail-soft: update checks must never block playing.
    let this_exe = std::env::current_exe().unwrap_or_else(|_| exe_dir.join("Flux Rec.exe"));
    let new_exe = exe_dir.join("Flux Rec.new.exe");
    match fluxrec_common::update::check_bootstrap_update(env!("CARGO_PKG_VERSION"), &new_exe).await
    {
        Ok(fluxrec_common::update::BootstrapUpdate::Available { .. }) => {
            let updater = exe_dir.join("fluxrec-selfupdate.exe");
            if updater.exists() {
                let from_s = new_exe.to_string_lossy().into_owned();
                let to_s = this_exe.to_string_lossy().into_owned();
                match Command::new(&updater)
                    .args(["--from", from_s.as_str(), "--to", to_s.as_str(), "--launch"])
                    .spawn()
                {
                    // Updater takes over: it waits for us to exit, swaps the
                    // exe, and relaunches.
                    Ok(_) => std::process::exit(0),
                    Err(e) => {
                        let _ = std::fs::remove_file(&new_exe);
                        util::crash_log(&format!("self-updater failed to start: {e}"));
                    }
                }
            }
            let _ = std::fs::remove_file(&new_exe);
        }
        Ok(fluxrec_common::update::BootstrapUpdate::UpToDate) => {}
        Err(e) => util::crash_log(&format!("bootstrap update check skipped: {e}")),
    }

    // 0b. Game files: fetch only what changed since last time (parallel,
    //     resumable, keeps the PC awake). Shows a progress window while busy.
    match fluxrec_common::update::update_game_files(
        fluxrec_common::MANIFEST_URL,
        &game_dir,
        &dir,
        "Flux Rec",
    )
    .await
    {
        Ok(o) => {
            if o.downloaded > 0 || o.deleted > 0 {
                util::crash_log(&format!(
                    "game files updated: {} downloaded, {} removed",
                    o.downloaded, o.deleted
                ));
            }
        }
        Err(e) => util::fatal(format!(
            "Couldn't update the game files:\n{e}\n\nCheck your internet connection and try again."
        )),
    }

    // 0c. Deploy the Steam emulator (v0.4.0+). This MUST run after the
    // updater (which would otherwise restore the real DLL from the mirror).
    // Clean-room reimplementation of steam_api64.dll: the exact 995 exports,
    // real non-null interface objects with typed ABI-safe vtable slots,
    // fake-but-valid identity (logged-on user, app ownership, auth tickets)
    // and working callback/call-result dispatch. Never needs real Steam.
    // Source: game/bootstrap/steam_emu/ (auditable, rebuilt from scratch).
    {
        let steam_dll = game_dir.join("RecRoom_Data/Plugins/x86_64/steam_api64.dll");
        let backup_dll = game_dir.join("RecRoom_Data/Plugins/x86_64/steam_api64.dll.fluxrec-backup");
        let _ = std::fs::remove_file(&backup_dll);
        // Always (over)write our emulator (516KB).
        match std::fs::write(&steam_dll, STEAM_EMULATOR) {
            Ok(_) => util::crash_log(&format!(
                "deployed Steam emulator ({} bytes)",
                STEAM_EMULATOR.len()
            )),
            Err(e) => util::crash_log(&format!("failed to deploy Steam emulator: {e}")),
        }
    }

    if !game_exe.exists() {
        util::fatal("Game files not found.\n\nPlease reinstall Flux Rec.".into());
    }

    // 1. The persistent backend owns auth + the translator. Make sure the
    //    running one matches this launcher, replacing it if outdated.
    let backend = ensure_backend(&exe_dir).await;

    // 2. Native crash dumps for RecRoom.exe (HKCU, no admin needed).
    crate::diag::configure_wer_dumps(&dir);

    // 3. Session diagnostics: exact exe, working dir, command line, backend
    //    state. Written beside the executable and under %LOCALAPPDATA%.
    let cmdline = format!("\"{}\"", game_exe.to_string_lossy());
    crate::diag::write_session_log(
        &exe_dir,
        &dir,
        &format!(
            "exe: {}\nworking dir: {}\ncommand line: {cmdline}\n\
             backend: v{} (pid {}), https=127.0.0.1:443 http=127.0.0.1:80\n",
            game_exe.to_string_lossy(),
            game_dir.to_string_lossy(),
            backend.version,
            backend.pid,
        ),
    );

    // 4. Launch the game and babysit it. The backend stays alive after it.
    let mut child = match tokio::process::Command::new(&game_exe)
        .current_dir(&game_dir)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => util::fatal(format!("Couldn't start the game:\n{e}")),
    };
    tokio::time::sleep(Duration::from_secs(3)).await;
    match child.try_wait() {
        Ok(Some(status)) => {
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
            crate::diag::write_session_log(
                &exe_dir,
                &dir,
                &format!("game exited instantly, code {code}\n"),
            );
            util::fatal(format!(
                "The game closed right away (exit code {code}).\n\n\
                 Details were saved to %LOCALAPPDATA%\\FluxRec\\crash.log"
            ))
        }
        Ok(None) => {}
        Err(e) => util::fatal(format!("Couldn't check on the game:\n{e}")),
    }
    // Wait until the player quits; the backend rides along and stays up.
    match child.wait().await {
        Ok(status) if status.success() => {
            // Normal exit (user quit the game).
            crate::diag::write_session_log(&exe_dir, &dir, "game exited normally (code 0)\n");
        }
        Ok(status) => {
            // Game crashed or exited with error. Grab diagnostics.
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
            util::crash_log(&format!("game exited with status: {status}"));
            let unity_log = crate::diag::collect_unity_log(&dir);
            let mut diag = format!("game exited with code {code}\n");
            match unity_log {
                Some(p) => diag.push_str(&format!("unity log: {}\n", p.to_string_lossy())),
                None => diag.push_str("unity log: not found\n"),
            }
            diag.push_str(&format!(
                "crash dumps: {}\\dumps\n",
                dir.to_string_lossy()
            ));
            crate::diag::write_session_log(&exe_dir, &dir, &diag);
            let log_url = util::upload_logs();
            let msg = match log_url {
                Some(url) => format!(
                    "The game closed unexpectedly (exit code {code}).\n\nLog uploaded for support: {url}"
                ),
                None => format!(
                    "The game closed unexpectedly (exit code {code}).\n\nSee %LOCALAPPDATA%\\FluxRec\\crash.log for details."
                ),
            };
            util::msgbox(&msg);
        }
        Err(e) => {
            util::crash_log(&format!("failed waiting for game: {e}"));
        }
    }
}
