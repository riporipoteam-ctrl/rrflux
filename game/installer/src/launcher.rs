//! `--play` launcher mode: the Flux Rec entry point.
//!
//! The desktop/Start Menu shortcut points at the installed
//! `FluxRecLauncher.exe` (a copy of the setup binary) with `--play`.
//! Launch flow, all behind the little Flux Rec window (the console stays
//! hidden the whole time):
//!
//!   0. Self-heal: verify the Steam bypass (emulator/stub DLL + settings +
//!      VC++ runtime) and repair it automatically before anything else.
//!   1. "Checking for updates\u{2026}" — live on every launch (5s timeout,
//!      fail-soft). No skip-cache: a release published after the last
//!      launch is offered on the next one.
//!   2. If a newer setup exists: "Downloading update\u{2026}" with progress,
//!      then the new setup is spawned with `--updated` and we WAIT for it.
//!      On success the update is fully installed and the fresh setup has
//!      already launched the game, so we just exit. On any failure we fall
//!      through and launch the installed game — an update must never strand
//!      the player with nothing running.
//!   3. Otherwise: "Launching game\u{2026}" — `RecRoom.exe` is spawned with
//!      no console window (so the BepInEx console never flashes), the
//!      window lingers a moment, then this exits.
//!
//! Every failure path still ends with the game launching (or a readable
//! status) — an update check can never strand the player.

use std::path::Path;
use std::time::Duration;

use crate::progress::Progress;
use crate::updater::UpdateDecision;

/// `--play` entry point. Never returns (exits the process).
pub fn run_launcher(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
) -> ! {
    let (progress, rx) = crate::progress::channel();
    let gui_thread = crate::spawn_gui(rx);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();

    // 1. If the game was never installed (or was wiped), install it first —
    // the launcher is a complete entry point, not just a game starter.
    if crate::find_game_exe(dir).is_none() {
        progress.set_status("Installing game files\u{2026}", 2);
        let installed = match &rt {
            Ok(r) => r
                .block_on(crate::run_install(
                    dir,
                    ns_host,
                    photon_rt,
                    photon_voice,
                    photon_chat,
                    &progress,
                ))
                .is_ok(),
            Err(_) => false,
        };
        if !installed {
            progress.set_status("Install failed — please run FluxRec-Setup.", 100);
            std::thread::sleep(Duration::from_secs(6));
            progress.done();
            let _ = gui_thread.join();
            std::process::exit(1);
        }
    }

    // 1b. Self-heal: the Steam bypass must be intact or the game crashes at
    // launch / shows "Failed to initialize Steam Platform". Verify on every
    // --play and repair automatically instead of ever letting the game hit
    // the broken state.
    //
    // 1c. AV self-heal (defender.rs, 2026-09-24): Windows Security
    // quarantined a game file on Armin's PC, hanging the game at
    // "Connecting to server...". Verify the quarantine-prone files too.
    {
        let bypass_state = crate::bypass::verify(dir);
        let vcredist_ok = crate::vcredist::is_installed();
        let av_problems = crate::defender::verify_quarantine_targets(dir, ns_host);
        let broken_reason: Option<String> = match (&bypass_state, vcredist_ok) {
            (crate::bypass::BypassState::Ok(_), true) if av_problems.is_empty() => None,
            (crate::bypass::BypassState::Broken(r), _) => Some(r.clone()),
            (_, false) => Some("VC++ 2022 runtime is missing".to_string()),
            _ => Some(crate::defender::describe_problems(&av_problems)),
        };
        if let Some(reason) = broken_reason {
            println!("[launcher] Steam bypass broken ({reason}) — repairing.");
            progress.set_status("Repairing game files…", 8);
            if !crate::vcredist::is_admin() {
                // Repair writes into the game dir: needs one elevation.
                let ok = crate::message_box_ok_cancel(
                    "Flux Rec",
                    &format!(
                        "Flux Rec needs to repair its game files ({reason}).\n\n\
                         Click OK to allow the one-time fix (it will ask for \
                         administrator rights)."
                    ),
                );
                if ok {
                    let _ = crate::vcredist::relaunch_elevated();
                }
                // Never launch a knowingly-broken game.
                progress.done();
                let _ = gui_thread.join();
                std::process::exit(0);
            }
            let client = reqwest::Client::builder()
                .user_agent("FluxRec-Setup/0.1.8")
                .connect_timeout(Duration::from_secs(30))
                .build();
            let repaired = match (&rt, client) {
                (Ok(r), Ok(c)) => {
                    let mut ok = true;
                    // Only re-run the bypass pipeline when the bypass (or the
                    // VC++ runtime it needs) is actually broken.
                    if !matches!(bypass_state, crate::bypass::BypassState::Ok(_)) || !vcredist_ok
                    {
                        ok &= r
                            .block_on(crate::bypass::repair_bypass(&c, dir, &progress))
                            .map_err(|e| {
                                eprintln!("[launcher] repair failed: {e}");
                                e
                            })
                            .is_ok();
                    }
                    // AV repair (defender.rs): re-download quarantined files,
                    // re-apply the hosts entry, re-add the Defender exclusion.
                    if !av_problems.is_empty() {
                        let left = r.block_on(
                            crate::defender::repair_quarantine_targets(
                                &c, dir, ns_host, &progress, &av_problems,
                            ),
                        );
                        if !left.is_empty() {
                            eprintln!(
                                "[launcher] AV repair incomplete: {}",
                                crate::defender::describe_problems(&left)
                            );
                        }
                        ok &= left.is_empty();
                    }
                    ok
                }
                _ => false,
            };
            if !repaired {
                crate::message_box(
                    "Flux Rec",
                    &format!(
                        "Flux Rec could not repair its game files ({reason}).\n\n\
                         Please re-run FluxRec-Setup as administrator."
                    ),
                    true,
                );
                progress.set_status("Repair failed — please re-run setup.", 100);
                std::thread::sleep(Duration::from_secs(6));
                progress.done();
                let _ = gui_thread.join();
                std::process::exit(1);
            }
            println!("[launcher] Steam bypass repaired.");
        }
    }

    // 2. Update check (fast + fail-soft by design).
    let decision = match &rt {
        Ok(r) => r.block_on(crate::updater::check_for_updates(dir, &progress)),
        Err(_) => UpdateDecision::UpToDate,
    };
    if let UpdateDecision::Available {
        version,
        download_url,
    } = decision
    {
        progress.set_status(&format!("Update {version} found \u{2014} installing\u{2026}"), 84);
        let dest = std::env::temp_dir().join("FluxRec-Setup-update.exe");
        let downloaded = match &rt {
            Ok(r) => r
                .block_on(crate::updater::download_update(
                    &download_url,
                    &dest,
                    &progress,
                ))
                .is_ok(),
            Err(_) => false,
        };
        if downloaded {
            // Hand off: the fresh setup runs with --updated (installs, then
            // launches the game itself). We WAIT for it instead of exiting
            // fire-and-forget: on success the game is already launching, so
            // we just exit; on any failure we fall through and launch the
            // installed game. An update must never strand the player.
            let dir_s = dir.to_string_lossy().to_string();
            let update_ok = spawn_update(&dest, &dir_s, &progress);
            if update_ok {
                progress.done();
                let _ = gui_thread.join();
                std::process::exit(0);
            }
            progress.set_status(
                "Update failed \u{2014} launching installed version\u{2026}",
                90,
            );
        }
        // Download or update failed: fall through and launch the installed
        // game anyway.
    }

    // 3. Launch the game.
    launch_game(dir, &progress);
    progress.done();
    std::thread::sleep(Duration::from_millis(500));
    let _ = gui_thread.join();
    std::process::exit(0);
}

/// Spawn the downloaded setup with `--updated --dir <dir>` and wait for it
/// to finish. Returns true only when the updater exited successfully — it
/// installs over the game dir and launches the game itself in that case.
/// Any spawn or wait failure returns false so the caller falls back to the
/// installed game. Shows "Installing update\u{2026}" while the child runs;
/// the update's own window carries the detailed progress.
fn spawn_update(dest: &Path, dir_s: &str, progress: &Progress) -> bool {
    let mut child = match crate::stealth::hidden_command(
        dest.to_str().unwrap_or("FluxRec-Setup-update.exe"),
    )
    .args(["--updated", "--dir", dir_s])
    .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[launcher] could not spawn updater: {e}");
            return false;
        }
    };
    progress.set_status("Installing update\u{2026}", 88);
    match child.wait() {
        Ok(status) => {
            if !status.success() {
                eprintln!("[launcher] updater exited with status {status}");
            }
            status.success()
        }
        Err(e) => {
            eprintln!("[launcher] waiting for updater failed: {e}");
            false
        }
    }
}

/// Spawn `RecRoom.exe` with no console window, showing
/// "Launching game\u{2026}" on the progress handle. Infallible: a missing exe
/// shows a readable status instead of panicking.
pub fn launch_game(dir: &Path, progress: &Progress) {
    match crate::find_game_exe(dir) {
        Some(exe) => {
            progress.set_status("Launching game\u{2026}", 100);
            crate::stealth::launch_hidden(&exe, &["+forcemode:screen"]);
            // Let the player see the "Launching game\u{2026}" state before the
            // window closes; the game outlives us.
            std::thread::sleep(Duration::from_secs(3));
        }
        None => {
            progress.set_status("Game files not found \u{2014} please reinstall.", 100);
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}
