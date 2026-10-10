//! Ripo Team Launcher v1.0.0 — game library flow.
//!
//! Replaces the old compact `--play` launcher window with a full game-library
//! experience: sidebar, game cards, PLAY button, settings (light/dark theme),
//! and a "Launching..." splash.
//!
//! Architecture:
//! - The GUI runs on its own thread (`gui_library::run_library_gui`).
//! - A worker thread does the pre-launch checks (fast, cached) and reports
//!   state via `LibMsg`.
//! - This module's `run_library` sits on the main thread, routing GUI
//!   commands (Play, Uninstall, theme changes) and driving the launch.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime};

use crate::gui_library;

/// Commands from the GUI thread to the main thread.
pub enum LibCmd {
    /// User clicked PLAY.
    Play,
    /// User clicked Uninstall (already confirmed in the GUI).
    Uninstall,
    /// User changed the theme. The GUI applies it live; we persist it.
    SetTheme(gui_library::Theme),
    /// User closed the window.
    Close,
}

/// State updates from the worker thread to the GUI thread.
pub enum LibMsg {
    /// Pre-launch checks finished: ready to play, with update info.
    Ready { update_available: bool, update_version: String },
    /// Checks are running (fast path).
    Checking,
    /// Check failed but the game can still launch (fail-soft).
    CheckFailed(String),
    /// Player count for the game card.
    PlayerCount(u32),
    /// Progress update during update-download/install (reuses GuiMsg shape).
    Progress { percent: u8, stage: String, detail: String },
    /// Tell the GUI to close (game is launching; splash takes over).
    CloseGui,
}

/// Where the launcher stores its small state files.
fn state_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(|l| PathBuf::from(l).join("RipoTeamLauncher"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Last time we did the slow network checks (update check, plugin sync,
/// logo bundles). Cached for 1 hour so launching stays fast.
fn last_check_path() -> PathBuf {
    state_dir().join("last_check.txt")
}

/// Returns true if the network checks were done recently (< 1 hour ago).
pub fn network_check_fresh() -> bool {
    let p = last_check_path();
    let content = std::fs::read_to_string(&p).unwrap_or_default();
    let ts: u64 = content.trim().parse().unwrap_or(0);
    if ts == 0 {
        return false;
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(ts) < 3600
}

/// Record that we just did the network checks.
pub fn mark_network_check() {
    let p = last_check_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(&p, now.to_string());
}

/// Load the saved theme (default: Light, per Armin's order).
pub fn load_theme() -> gui_library::Theme {
    let p = state_dir().join("theme.txt");
    match std::fs::read_to_string(&p).unwrap_or_default().trim() {
        "dark" => gui_library::Theme::Dark,
        _ => gui_library::Theme::Light,
    }
}

/// Persist the theme.
pub fn save_theme(t: gui_library::Theme) {
    let p = state_dir().join("theme.txt");
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let s = match t {
        gui_library::Theme::Dark => "dark",
        gui_library::Theme::Light => "light",
    };
    let _ = std::fs::write(&p, s);
}

/// Fetch the live player count (fail-soft: None on any error).
fn fetch_player_count() -> Option<u32> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .ok()?;
        let text = client
            .get("https://api.ripo-ripoteam.workers.dev/api/public/v1/players/count")
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("count")?.as_u64().map(|c| c as u32)
    })
}

/// Entry point for `--play` in v1.0.0. Never returns.
pub fn run_library(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
) -> ! {
    let (cmd_tx, cmd_rx): (Sender<LibCmd>, Receiver<LibCmd>) = mpsc::channel();
    let (msg_tx, msg_rx): (Sender<LibMsg>, Receiver<LibMsg>) = mpsc::channel();

    let theme = load_theme();
    let version = env!("CARGO_PKG_VERSION").to_string();

    // GUI thread: the library window.
    let gui_config = gui_library::LibraryConfig {
        theme,
        version: version.clone(),
        cmd_tx: cmd_tx.clone(),
    };
    let gui_thread = std::thread::spawn(move || {
        gui_library::run_library_gui(gui_config, msg_rx);
    });

    // Worker thread: fast pre-launch checks + player count.
    let dir_w = dir.to_path_buf();
    let ns_w = ns_host.to_string();
    let msg_tx_w = msg_tx.clone();
    std::thread::spawn(move || {
        worker_checks(&dir_w, &ns_w, &msg_tx_w);
    });

    // Main thread: route GUI commands.
    loop {
        match cmd_rx.recv() {
            Ok(LibCmd::Play) => {
                do_play(dir, ns_host, photon_rt, photon_voice, photon_chat, &msg_tx);
                // do_play never returns (launches or exits)
            }
            Ok(LibCmd::Uninstall) => {
                do_uninstall(dir, &msg_tx);
            }
            Ok(LibCmd::SetTheme(t)) => {
                save_theme(t);
            }
            Ok(LibCmd::Close) | Err(_) => {
                let _ = msg_tx.send(LibMsg::CloseGui);
                let _ = gui_thread.join();
                std::process::exit(0);
            }
        }
    }
}

/// Background checks: verify the install is sane, check for updates (cached),
/// fetch the player count. All fail-soft.
fn worker_checks(dir: &Path, _ns_host: &str, msg_tx: &Sender<LibMsg>) {
    let _ = msg_tx.send(LibMsg::Checking);

    // Player count (independent, fail-soft).
    if let Some(count) = fetch_player_count() {
        let _ = msg_tx.send(LibMsg::PlayerCount(count));
    }

    // If the game was never installed, the library shows "Install" state.
    // The PLAY button will run the full install.
    let game_exe = crate::find_game_exe(dir);
    if game_exe.is_none() {
        let _ = msg_tx.send(LibMsg::Ready {
            update_available: false,
            update_version: String::new(),
        });
        return;
    }

    // Update check: skip the network if we checked recently.
    if network_check_fresh() {
        let _ = msg_tx.send(LibMsg::Ready {
            update_available: false,
            update_version: String::new(),
        });
        return;
    }

    // Do the update check on a current-thread runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let decision = match rt {
        Ok(r) => {
            let (progress, _rx) = crate::progress::channel();
            r.block_on(crate::updater::check_for_updates(dir, &progress))
        }
        Err(_) => crate::updater::UpdateDecision::UpToDate,
    };
    mark_network_check();

    match decision {
        crate::updater::UpdateDecision::Available { version, .. } => {
            let _ = msg_tx.send(LibMsg::Ready {
                update_available: true,
                update_version: version,
            });
        }
        // v1.0.5: tell the UI the check failed so it can warn the user.
        crate::updater::UpdateDecision::CheckFailed => {
            let _ = msg_tx.send(LibMsg::CheckFailed(
                "Couldn't reach GitHub to check for updates. Check your connection.".to_string(),
            ));
        }
        _ => {
            let _ = msg_tx.send(LibMsg::Ready {
                update_available: false,
                update_version: String::new(),
            });
        }
    }
}

/// PLAY was clicked: run the full pre-launch sequence (verify, repair,
/// update, launch), then show the splash and start the game.
fn do_play(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    msg_tx: &Sender<LibMsg>,
) -> ! {
    let send = |percent: u8, stage: &str| {
        let _ = msg_tx.send(LibMsg::Progress {
            percent,
            stage: stage.to_string(),
            detail: String::new(),
        });
    };

    send(5, "Preparing Flux Rec…");

    // If the game was never installed, run the full install first.
    if crate::find_game_exe(dir).is_none() {
        send(8, "Installing game files…");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build();
        let installed = match rt {
            Ok(r) => {
                // Forward install progress to the library GUI.
                let msg_tx_c = msg_tx.clone();
                let (p2, rx2) = crate::progress::channel();
                std::thread::spawn(move || {
                    for msg in rx2 {
                        let _ = msg_tx_c.send(LibMsg::Progress {
                            percent: msg.percent,
                            stage: msg.stage,
                            detail: msg.detail,
                        });
                    }
                });
                r.block_on(crate::run_install(
                    dir, ns_host, photon_rt, photon_voice, photon_chat, &p2,
                ))
                .is_ok()
            }
            Err(_) => false,
        };
        if !installed {
            send(100, "Install failed — please run RipoTeamLauncher setup.");
            std::thread::sleep(Duration::from_secs(6));
            std::process::exit(1);
        }
    }

    // Fast self-heal: verify critical files (local, fast).
    send(70, "Verifying game files…");
    if let Err(reason) = crate::bepinex::verify_for_launcher(dir) {
        send(75, "Repairing game files…");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        if let Ok(r) = rt {
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build();
            if let Ok(c) = client {
                let (p, _rx) = crate::progress::channel();
                let _ = r.block_on(crate::bepinex::repair_for_launcher(
                    &c, dir, ns_host, photon_rt, photon_voice, photon_chat, &p,
                ));
            }
        }
        let _ = reason;
    }

    // Apply Flux Rec logo bundles (overwrites wrong logos in place).
    // Fail-soft: the game launches even if this fails.
    send(80, "Applying Flux Rec branding…");
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        if let Ok(r) = rt {
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build();
            if let Ok(c) = client {
                let (p, _rx) = crate::progress::channel();
                if let Err(e) = r.block_on(crate::apply_logo_bundle(&c, dir, &p)) {
                    eprintln!("[library] logo bundle failed ({}); continuing.", e);
                }
            }
        }
    }

    // Update check (only if the worker didn't just do it).
    if !network_check_fresh() {
        send(82, "Checking for updates…");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        if let Ok(r) = rt {
            let (p, _rx) = crate::progress::channel();
            match r.block_on(crate::updater::check_for_updates(dir, &p)) {
                crate::updater::UpdateDecision::Available {
                    version,
                    download_url,
                } => {
                    send(84, &format!("Update {version} found — installing…"));
                    let dest = std::env::temp_dir().join("RipoTeamLauncher-update.exe");
                    let downloaded = {
                        let (p2, rx2) = crate::progress::channel();
                        let msg_tx_c = msg_tx.clone();
                        std::thread::spawn(move || {
                            for msg in rx2 {
                                let _ = msg_tx_c.send(LibMsg::Progress {
                                    percent: msg.percent,
                                    stage: msg.stage,
                                    detail: msg.detail,
                                });
                            }
                        });
                        r.block_on(crate::updater::download_update(
                            &download_url,
                            &dest,
                            &p2,
                        ))
                        .is_ok()
                    };
                    if downloaded {
                        // The fresh setup installs and launches the game itself.
                        let dir_s = dir.to_string_lossy().to_string();
                        let _ = msg_tx.send(LibMsg::CloseGui);
                        std::thread::sleep(Duration::from_millis(300));
                        let (p3, _rx3) = crate::progress::channel();
                        crate::launcher::spawn_update(&dest, &dir_s, &p3);
                        std::process::exit(0);
                    }
                    send(90, "Update failed — launching installed version…");
                }
                _ => {}
            }
            mark_network_check();
        }
    }

    // v1.0.3: Patch the game exe icon (once) so the taskbar shows the blue
    // Flux Rec logo. Skipped if already done (flag file).
    {
        let flag = dir.join(".icon_patched_v1");
        if !flag.exists() {
            if let Some(exe) = crate::find_game_exe(dir) {
                const ICON_BYTES: &[u8] = include_bytes!("../assets/fluxrec.ico");
                match crate::icon_patch::patch_exe_icon(&exe, ICON_BYTES) {
                    Ok(true) => {
                        println!("[icon] patched game exe icon.");
                        let _ = std::fs::write(&flag, "1");
                    }
                    Ok(false) => {
                        let _ = std::fs::write(&flag, "1");
                    }
                    Err(e) => eprintln!("[icon] icon patch failed ({}); continuing.", e),
                }
            }
        }
    }

    // Clear Unity's HTTP cache (fast path) so the client fetches fresh data.
    send(92, "Launching Flux Rec…");
    {
        let (p, _rx) = crate::progress::channel();
        crate::launcher::clear_unity_http_cache(&p);
    }

    // Launch!
    send(95, "Launching Flux Rec…");
    let _ = msg_tx.send(LibMsg::CloseGui);
    std::thread::sleep(Duration::from_millis(400));

    // Show the splash, then start the game.
    let (splash_tx, splash_rx) = mpsc::channel();
    let theme = load_theme();
    std::thread::spawn(move || {
        gui_library::run_splash_gui(theme, splash_rx);
    });

    let (progress, _rx) = crate::progress::channel();
    crate::launcher::launch_game(dir, &progress);

    // Give the game a moment to start, then close the splash.
    std::thread::sleep(Duration::from_secs(4));
    let _ = splash_tx.send(());
    std::process::exit(0);
}

/// Uninstall: remove the game directory and shortcuts.
fn do_uninstall(dir: &Path, msg_tx: &Sender<LibMsg>) {
    let _ = msg_tx.send(LibMsg::Progress {
        percent: 50,
        stage: "Uninstalling Flux Rec…".to_string(),
        detail: String::new(),
    });
    // Remove the game dir.
    let _ = std::fs::remove_dir_all(dir);
    // Remove shortcuts (best effort).
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let start_menu = PathBuf::from(format!(
                "{appdata}\\Microsoft\\Windows\\Start Menu\\Programs\\Flux Rec.lnk"
            ));
            let _ = std::fs::remove_file(&start_menu);
            let start_menu2 = PathBuf::from(format!(
                "{appdata}\\Microsoft\\Windows\\Start Menu\\Programs\\Ripo Team Launcher.lnk"
            ));
            let _ = std::fs::remove_file(&start_menu2);
        }
    }
    let _ = msg_tx.send(LibMsg::CloseGui);
    std::process::exit(0);
}
