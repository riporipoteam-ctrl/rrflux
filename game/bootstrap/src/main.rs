// Flux Rec — headless game bootstrapper (this IS the "game" as far as the
// player is concerned; there is no launcher window and no login screen).
//
// Double-clicking the desktop icon:
//   0. Self-updates: a newer bootstrapper or changed game files download
//      automatically (parallel, resumable, no sleeping mid-download), so
//      the setup only ever runs once.
//   1. If another Flux Rec is already running, exits quietly.
//   2. Signs in silently (anonymous Firebase account, cached locally).
//   3. Starts the local translator on 127.0.0.1:80, which the patched game
//      client talks to. If port 80 is taken, figures out by what and says so.
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

fn data_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("FluxRec")
}

fn crash_log(msg: &str) {
    use std::io::Write as _;
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let epoch = std::time::SystemTime::now()
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
fn fatal(msg: String) -> ! {
    crash_log(&msg);
    msgbox(&msg);
    std::process::exit(1);
}

/// Is another Flux Rec already running its translator? Ask it.
async fn translator_alive() -> bool {
    let Ok(c) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    match c.get("http://127.0.0.1/health").send().await {
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

/// Who is squatting on port 80? Returns (pid, image name).
fn port_80_holder() -> Option<(u32, String)> {
    let out = Command::new("netstat").args(["-ano"]).output().ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        // TCP    0.0.0.0:80    0.0.0.0:0    LISTENING    1234
        if p.len() < 5 || p[0] != "TCP" || p[3] != "LISTENING" {
            continue;
        }
        if !p[1].ends_with(":80") {
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
    match port_80_holder() {
        Some((pid, name)) => format!(
            "Flux Rec needs local port 80, but \"{name}\" (PID {pid}) is using it.\n\n\
             Close {name} and start Flux Rec again.\n\n\
             (technical: {e})"
        ),
        None => format!("The local game server failed to start:\n{e}"),
    }
}

fn main() {
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

    // 2. Local translator on 127.0.0.1:80. One retry if a stale Flux Rec
    //    process is squatting the port; otherwise name the culprit.
    let mut started = false;
    for attempt in 0..2 {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let s3 = session.clone();
        tokio::spawn(translator::serve(s3, tx));
        match rx.await {
            Ok(Ok(())) => {
                started = true;
                break;
            }
            Ok(Err(e)) => {
                let ours = port_80_holder().map(|(_, n)| is_our_exe(&n)).unwrap_or(false);
                if attempt == 0 && ours {
                    if let Some((pid, _)) = port_80_holder() {
                        let _ = Command::new("taskkill")
                            .args(["/PID", &pid.to_string(), "/F"])
                            .output();
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                fatal(describe_bind_error(&e));
            }
            Err(_) => fatal("The local game server died during startup.".into()),
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
    let _ = child.wait().await;
}
