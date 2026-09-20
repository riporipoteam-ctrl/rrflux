// Flux Rec — headless game bootstrapper (this IS the "game" as far as the
// player is concerned; there is no launcher window and no login screen).
//
// Double-clicking the desktop icon:
//   0. Self-updates: a newer bootstrapper or changed game files download
//      automatically (parallel, resumable, no sleeping mid-download), so
//      the setup only ever runs once.
//   1. If another Flux Rec is already running, exits quietly.
//   2. Signs in silently (anonymous Firebase account, cached locally).
//   3. Starts the local translator on 127.0.0.1:443 (HTTPS) + :80 (HTTP),
//      which the patched game client talks to. If port 443 is taken,
//      figures out by what and says so.
//   4. Launches RecRoom.exe and waits for it. If the game dies instantly,
//      explains why instead of vanishing silently.

#![windows_subsystem = "windows"]

mod auth;
mod translator;

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use translator::SharedSession;

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(
        h_wnd: *mut c_void,
        lp_text: *const u16,
        lp_caption: *const u16,
        u_type: u32,
    ) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn msgbox(text: &str) {
    let (t, c) = (wide(text), wide("Flux Rec"));
    unsafe {
        // MB_ICONERROR
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), 0x10);
    }
}

// Flat-API Steam stub (v0.3.15+): exports the exact 995 names from
// steam_api64.dll (SDK 1.48). Steamworks.NET 14.0.0 P/Invokes flat
// functions (no vtables). Key functions return fake-but-valid values
// (SteamAPI_Init -> true, fake SteamID, fake auth ticket); all others
// return safe zeros. This lets the game run without Steam installed.
const STEAM_STUB: &[u8] = include_bytes!("steam_api64_stub.dll");

fn data_dir() -> PathBuf {
    data_dir_opt().unwrap_or_else(|| PathBuf::from(".").join("FluxRec"))
}

fn data_dir_opt() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("APPDATA").map(PathBuf::from))
        .ok()
        .map(|p| p.join("FluxRec"))
}

fn crash_log(msg: &str) {
    use std::io::Write as _;
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("-- Flux Rec start (epoch {epoch}) --\n{msg}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("crash.log"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Show an error popup, log it, and quit. The one visible failure path.
/// Also uploads the log to ix.io so Tim can read it remotely.
fn fatal(msg: String) -> ! {
    crash_log(&msg);
    // Upload logs for remote diagnosis (fail-soft: if upload fails, just show local path).
    let log_url = upload_logs();
    let full_msg = match log_url {
        Some(url) => format!("{msg}\n\nLog uploaded for support: {url}"),
        None => format!("{msg}\n\nLog saved to %LOCALAPPDATA%\\FluxRec\\crash.log"),
    };
    msgbox(&full_msg);
    std::process::exit(1);
}

/// Upload crash.log + translator.log to ix.io for remote diagnosis.
/// Returns the URL on success, None on failure (fail-soft).
fn upload_logs() -> Option<String> {
    let dir = data_dir();
    let mut combined = String::new();
    for name in ["crash.log", "translator.log", "panic.log"] {
        if let Ok(content) = std::fs::read_to_string(dir.join(name)) {
            combined.push_str(&format!("=== {name} ===\n{content}\n\n"));
        }
    }
    if combined.is_empty() {
        return None;
    }
    // ix.io: POST with form field 'f:1' containing the text.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client
        .post("https://ix.io")
        .form(&[("f:1", combined)])
        .send()
        .ok()?;
    if resp.status().is_success() {
        resp.text().ok().map(|t| t.trim().to_string()).filter(|s| !s.is_empty())
    } else {
        None
    }
}

/// Is another Flux Rec already running its translator? Ask it over HTTPS,
/// trusting our own local CA.
async fn translator_alive() -> bool {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(2));
    if let Some(dir) = data_dir_opt() {        let ca_path = dir.join("certs").join("ca.pem");
        if let Ok(pem) = std::fs::read(&ca_path) {
            if let Ok(ca) = reqwest::Certificate::from_pem(&pem) {
                builder = builder.add_root_certificate(ca);
            }
        }
    }
    let Ok(c) = builder.build() else {
        return false;
    };
    match c.get("https://127.0.0.1/health").send().await {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .map(|v| v.get("app").and_then(|a| a.as_str()) == Some("fluxrec"))
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn process_name(pid: u32) -> Option<String> {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let name = line.split(',').next()?.trim_matches('"').to_string();
    if name.is_empty() || name.eq_ignore_ascii_case("INFO:") {
        None
    } else {
        Some(name)
    }
}

/// Who is squatting on the given local port? Returns (pid, image name).
fn port_holder(port: u16) -> Option<(u32, String)> {
    let out = Command::new("netstat").args(["-ano"]).output().ok()?;
    let want = format!(":{port}");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        // TCP    0.0.0.0:443    0.0.0.0:0    LISTENING    1234
        if p.len() < 5 || p[0] != "TCP" || p[3] != "LISTENING" {
            continue;
        }
        if !p[1].ends_with(&want) {
            continue;
        }
        if let Ok(pid) = p[4].parse::<u32>() {
            let name = process_name(pid).unwrap_or_else(|| "unknown program".into());
            return Some((pid, name));
        }
    }
    None
}

fn is_our_exe(name: &str) -> bool {
    name.eq_ignore_ascii_case("Flux Rec.exe") || name.eq_ignore_ascii_case("fluxrec.exe")
}

fn describe_bind_error(e: &str) -> String {
    match port_holder(443) {
        Some((pid, name)) => format!(
            "Flux Rec needs local port 443, but \"{name}\" (PID {pid}) is using it.\n\n\
             Close {name} and start Flux Rec again.\n\n\
             (technical: {e})"
        ),
        None => format!("The local game server failed to start:\n{e}"),
    }
}

fn main() {
    // Log panics to %LOCALAPPDATA%\FluxRec\panic.log so a crashing
    // background task (like the translator) leaves a trace we can show.
    let pd = data_dir();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write as _;
        let _ = std::fs::create_dir_all(&pd);
        let msg = format!("PANIC: {}\n", info);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(pd.join("panic.log"))
            .and_then(|mut f| f.write_all(msg.as_bytes()));
    }));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| fatal(format!("Couldn't start Flux Rec:\n{e}")));
    rt.block_on(async_main());
}

async fn async_main() {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let game_dir = exe_dir.join("game");
    let game_exe = game_dir.join("RecRoom.exe");
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);

    // v0.3.16+: visible startup marker on Desktop (debug). Proves the
    // bootstrap EXE actually ran and which version it is.
    {
        if let Ok(desktop) = std::env::var("USERPROFILE").map(|p| PathBuf::from(p).join("Desktop")) {
            let _ = std::fs::write(desktop.join("FLUXREC_RUNNING.txt"),
                format!("Flux Rec {} ran at {}\nIf you see this, the launcher works.\n",
                    env!("CARGO_PKG_VERSION"),
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)));
        }
    }

    // Already running? Bow out quietly — the running copy owns the game.
    if translator_alive().await {
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
                        crash_log(&format!("self-updater failed to start: {e}"));
                    }
                }
            }
            let _ = std::fs::remove_file(&new_exe);
        }
        Ok(fluxrec_common::update::BootstrapUpdate::UpToDate) => {}
        Err(e) => crash_log(&format!("bootstrap update check skipped: {e}")),
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
                crash_log(&format!(
                    "game files updated: {} downloaded, {} removed",
                    o.downloaded, o.deleted
                ));
            }
        }
        Err(e) => fatal(format!(
            "Couldn't update the game files:\n{e}\n\nCheck your internet connection and try again."
        )),
    }

    // 0c. Deploy the flat-API Steam stub (v0.3.15+). This MUST run after the
    // updater (which would otherwise restore the real DLL from the mirror).
    // The stub exports the exact 995 names from steam_api64.dll but returns
    // fake-but-valid values, letting the game run without Steam installed.
    // The old vtable-based stub (3.9-3.11, 92KB) is also purged here.
    {
        let steam_dll = game_dir.join("RecRoom_Data/Plugins/x86_64/steam_api64.dll");
        let backup_dll = game_dir.join("RecRoom_Data/Plugins/x86_64/steam_api64.dll.fluxrec-backup");
        let _ = std::fs::remove_file(&backup_dll);
        // Always (over)write our stub. It's 231KB; the old broken stub was 92KB.
        match std::fs::write(&steam_dll, STEAM_STUB) {
            Ok(_) => crash_log(&format!(
                "deployed Steam stub ({} bytes)",
                STEAM_STUB.len()
            )),
            Err(e) => crash_log(&format!("failed to deploy Steam stub: {e}")),
        }
    }

    if !game_exe.exists() {
        fatal("Game files not found.\n\nPlease reinstall Flux Rec.".into());
    }

    // 1. Silent sign-in (anonymous Firebase account).
    let session: SharedSession = Default::default();
    match auth::ensure_session(&dir).await {
        Ok(s) => *session.lock().await = Some(s),
        Err(e) => fatal(format!(
            "Couldn't sign you in:\n{e}\n\nCheck your internet connection and try again."
        )),
    }
    let s2 = session.clone();
    let d2 = dir.clone();
    tokio::spawn(async move { auth::refresh_loop(s2, d2).await; });

    // Log every request the game makes — that's how we learn which
    // endpoints the client actually needs.
    translator::set_log_dir(dir.clone());

    // 2. Local translator on 127.0.0.1:443 (HTTPS) + :80 (HTTP). One retry
    //    if a stale Flux Rec process is squatting the port; otherwise name
    //    the culprit.
    let mut started = false;
    for attempt in 0..2 {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let s3 = session.clone();
        let d3 = dir.clone();
        tokio::spawn(translator::serve(s3, d3, tx));
        match rx.await {
            Ok(Ok(())) => {
                started = true;
                break;
            }
            Ok(Err(e)) => {
                let ours = port_holder(443).map(|(_, n)| is_our_exe(&n)).unwrap_or(false);
                if attempt == 0 && ours {
                    if let Some((pid, _)) = port_holder(443) {
                        let _ = Command::new("taskkill")
                            .args(["/PID", &pid.to_string(), "/F"])
                            .output();
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                fatal(describe_bind_error(&e));
            }
            Err(_) => {
                // The serve task died without sending — almost always a
                // panic. The panic hook logs to panic.log; surface it.
                let panic_info = std::fs::read_to_string(dir.join("panic.log"))
                    .ok()
                    .and_then(|c| c.lines().last().map(|l| l.to_string()))
                    .unwrap_or_else(|| "no details captured".into());
                fatal(format!(
                    "The local game server died during startup.\n\n\
                     Details: {panic_info}\n\n\
                     A log was saved to %LOCALAPPDATA%\\FluxRec\\panic.log"
                ));
            }
        }
    }
    if !started {
        fatal("The local game server failed to start.".into());
    }

    // 3. Launch the game and babysit it.
    let mut child = match tokio::process::Command::new(&game_exe)
        .current_dir(&game_dir)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => fatal(format!("Couldn't start the game:\n{e}")),
    };
    tokio::time::sleep(Duration::from_secs(3)).await;
    match child.try_wait() {
        Ok(Some(status)) => fatal(format!(
            "The game closed right away (exit code {status}).\n\n\
             Details were saved to %LOCALAPPDATA%\\FluxRec\\crash.log"
        )),
        Ok(None) => {}
        Err(e) => fatal(format!("Couldn't check on the game:\n{e}")),
    }
    // Wait until the player quits; the translator rides along and dies with us.
    match child.wait().await {
        Ok(status) if status.success() => {
            // Normal exit (user quit the game).
        }
        Ok(status) => {
            // Game crashed or exited with error. Upload logs for diagnosis.
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
            crash_log(&format!("game exited with status: {status}"));
            let log_url = upload_logs();
            let msg = match log_url {
                Some(url) => format!(
                    "The game closed unexpectedly (exit code {code}).\n\nLog uploaded for support: {url}"
                ),
                None => format!(
                    "The game closed unexpectedly (exit code {code}).\n\nSee %LOCALAPPDATA%\\FluxRec\\crash.log for details."
                ),
            };
            msgbox(&msg);
        }
        Err(e) => {
            crash_log(&format!("failed waiting for game: {e}"));
        }
    }
}
