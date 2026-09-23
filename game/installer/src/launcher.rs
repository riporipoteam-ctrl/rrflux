//! `--play` launcher mode: the Flux Rec entry point.
//!
//! The desktop/Start Menu shortcut points at the installed
//! `FluxRecLauncher.exe` (a copy of the setup binary) with `--play`.
//! Launch flow, all behind the little Flux Rec window (the console stays
//! hidden the whole time):
//!
//!   1. "Checking for updates\u{2026}" — fast (24h cache, 8s timeout, fail-soft).
//!   2. If a newer setup exists: "Downloading update\u{2026}" with progress,
//!      then the new setup is spawned with `--updated` and this process exits.
//!      The fresh setup installs over the game dir and launches the game
//!      itself once the update is fully installed.
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
            // launches the game itself). This process exits so nothing is
            // locked while files are replaced.
            let dir_s = dir.to_string_lossy().to_string();
            let _ = crate::stealth::hidden_command(
                dest.to_str().unwrap_or("FluxRec-Setup-update.exe"),
            )
            .args(["--updated", "--dir", &dir_s])
            .spawn();
            progress.done();
            std::thread::sleep(Duration::from_millis(500));
            std::process::exit(0);
        }
        // Download failed: fall through and launch the installed game anyway.
    }

    // 3. Launch the game.
    launch_game(dir, &progress);
    progress.done();
    std::thread::sleep(Duration::from_millis(500));
    let _ = gui_thread.join();
    std::process::exit(0);
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
