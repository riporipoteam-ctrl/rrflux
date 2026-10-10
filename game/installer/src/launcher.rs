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
//!   3. Otherwise: "Launching game…" — `Injector.exe` is started first (it
//!      injects 2025Patch.dll once the game loads), then
//!      `Recroom_Release.exe +forcemode:screen` is spawned with no console
//!      window, the window lingers a moment, then this exits.
//!
//! v0.3.1: 2025-only. The 2023 client (`RecRoom.exe`, BepInEx) is never
//! launched — a 2023 tree triggers the setup migration path instead.
//!
//! Every failure path still ends with the game launching (or a readable
//! status) — an update check can never strand the player.

use std::path::Path;
use std::time::Duration;

use crate::progress::Progress;
use crate::updater::UpdateDecision;


/// Ensure rec.net resolves to localhost to prevent Store WebView hangs.
/// The 2023 client has hardcoded https://rec.net/shop URLs; rec.net is dead
/// (Rec Room shut down June 2026), causing 6s DNS timeouts. Adding a hosts
/// entry makes it fail fast. Fails silently without admin — not critical.
fn ensure_recnet_hosts_entry() {
    let hosts_path = "C:\\Windows\\System32\\drivers\\etc\\hosts";
    let entry = "127.0.0.1 rec.net";
    
    // Check if already present
    if let Ok(content) = std::fs::read_to_string(hosts_path) {
        if content.contains("rec.net") {
            return;
        }
    }
    
    // Try to append (fails silently without admin)
    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(hosts_path) {
        use std::io::Write;
        let _ = writeln!(file, "\n{}", entry);
    }
}


/// `--play` entry point. Never returns (exits the process).
pub fn run_launcher(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
) -> ! {
    let (progress, rx) = crate::progress::channel();
    let gui_thread = crate::spawn_gui_launcher(rx);
    ensure_recnet_hosts_entry();

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
    // v0.2.6: BepInEx self-heal (FluxLoader removed). Verify the loader is
    // intact; if it is broken, repair it by re-running the BepInEx install
    // step.
    {
        let loader_state = crate::bepinex::verify_for_launcher(dir);
        if let Err(reason) = loader_state {
            println!("[launcher] 2025Patch broken ({reason}) — repairing.");
            progress.set_status("Repairing 2025Patch…", 8);
            let client = reqwest::Client::builder()
                .user_agent("FluxRec-Setup/0.2.0")
                .connect_timeout(Duration::from_secs(30))
                .build();
            let repaired = match (&rt, client) {
                (Ok(r), Ok(c)) => r
                    .block_on(crate::bepinex::repair_for_launcher(
                        &c, dir, ns_host, photon_rt, photon_voice, photon_chat, &progress,
                    ))
                    .map_err(|e| {
                        eprintln!("[launcher] 2025Patch repair failed: {e}");
                        e
                    })
                    .is_ok(),
                _ => false,
            };
            if !repaired {
                progress.set_status("2025Patch repair failed — please re-run FluxRec-Setup.", 100);
                std::thread::sleep(Duration::from_secs(6));
                progress.done();
                let _ = gui_thread.join();
                std::process::exit(1);
            }
            println!("[launcher] 2025Patch repaired.");
        }
    }

    // 1c. Plugin auto-update: keep RecNetPlugin.dll in sync with the HF
    // mirror on every launch (fail-soft — offline keeps the installed one).
    if let Ok(r) = &rt {
        r.block_on(crate::bepinex::sync_plugin_from_mirror(dir, &progress));
    }

    // --play and repair automatically instead of ever letting the game hit
    // the broken state.
    //
    // 1d. AV self-heal (defender.rs, 2026-09-24): Windows Security
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

    // 1e. Logo bundle (v0.6.13): apply Flux Rec branding (loading screens,
    // etc.) on every launch so existing installs pick it up once the
    // bundles are uploaded. FAIL-SOFT: 404s are warnings, not errors —
    // the game launches regardless.
    if let Ok(r) = &rt {
        let client = reqwest::Client::builder()
            .user_agent("FluxRec-Setup/0.6.13")
            .connect_timeout(Duration::from_secs(30))
            .build();
        if let Ok(c) = client {
            if let Err(e) = r.block_on(crate::apply_logo_bundle(&c, dir, &progress)) {
                eprintln!("[launcher] logo bundle failed ({e}); continuing.");
            }
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

    // 2b. Flux account linking prompt (v0.6.13). Offer once: the choice is
    // persisted in %LOCALAPPDATA%\FluxRec\flux_link.txt and never asked
    // again. Fail-soft: any error skips the prompt and launches the game.
    prompt_flux_account_link();

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

/// v0.6.13: Offer to link the Flux Rec account with the Flux social media
/// account (flux.sitey.my). Asked at most once — the answer is persisted in
/// %LOCALAPPDATA%\FluxRec\flux_link.txt (`linked`, `pending`, or
/// `dismissed`). Fail-soft: any error returns silently and the game launches.
fn prompt_flux_account_link() {
    let flag_path = match std::env::var("LOCALAPPDATA") {
        Ok(local) => std::path::PathBuf::from(local)
            .join("FluxRec")
            .join("flux_link.txt"),
        Err(_) => return,
    };
    // Already answered? Never nag again.
    if let Ok(content) = std::fs::read_to_string(&flag_path) {
        match content.trim().to_lowercase().as_str() {
            "linked" | "pending" | "dismissed" => return,
            _ => {}
        }
    }
    let yes = crate::message_box_yes_no(
        "Flux Rec",
        "Do you wish to connect your Flux Rec account with your Flux social media account?\n\nYou can see your rooms and photos on flux.sitey.my after connecting.",
    );
    if let Some(parent) = flag_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if yes {
        open_url_in_browser("https://flux.sitey.my/settings?link=fluxrec");
        // Don't nag again; the user completes the link on the site.
        let _ = std::fs::write(&flag_path, "pending");
    } else {
        let _ = std::fs::write(&flag_path, "dismissed");
    }
}

/// Open a URL in the default browser. Fire-and-forget: failures are ignored.
#[cfg(windows)]
fn open_url_in_browser(url: &str) {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let uri = HSTRING::from(url);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(uri.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(not(windows))]
fn open_url_in_browser(_url: &str) {}

/// Spawn the game with no console window. v0.3.0: 2025 client flow —
/// start `Injector.exe` FIRST (it waits for the game process, then injects
/// 2025Patch.dll once GameAssembly.dll + Referee.dll are loaded), then start
/// the game exe. Injector skips already-patched instances, so re-running is
/// v0.3.9: Clear Unity's HTTP cache so the client fetches fresh backend data
/// on every launch. Unity caches HTTP responses (including the storefront
/// JSON) in LocalLow; stale cache was causing the client to use old
/// AvatarItemType data even after backend fixes deployed.
fn clear_unity_http_cache(progress: &Progress) {
    progress.set_status("Launching Flux Rec…", 100);
    // Unity player cache locations for Rec Room on Windows.
    // v0.6.16: Fast path — directly target known Unity cache dirs instead of
    // recursively walking all of AppData (which was slow).
    let mut cleared = 0;
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let base = std::path::PathBuf::from(profile);
        // Known Unity HTTP cache locations for Rec Room
        let cache_dirs = [
            base.join("AppData").join("LocalLow").join("Rec Room").join("Rec Room").join("cache"),
            base.join("AppData").join("LocalLow").join("Rec Room").join("cache"),
            base.join("AppData").join("Local").join("Rec Room").join("cache"),
        ];
        for cache_dir in &cache_dirs {
            if cache_dir.is_dir() {
                if std::fs::remove_dir_all(cache_dir).is_ok() {
                    cleared += 1;
                    println!("[launcher] cleared cache dir: {}", cache_dir.display());
                }
            }
        }
    }
    if cleared > 0 {
        println!("[launcher] cleared {cleared} Unity cache dir(s)");
    } else {
        println!("[launcher] no Unity cache dirs found to clear");
    }
}

/// safe. Infallible: a missing exe shows a readable status instead of panicking.
///
/// v0.3.2: pre-flight check — the game is USELESS without the patch (it
/// boots unpatched, hits the dead backend, and dies on "An error occurred"
/// with no 2025patch.log). If Injector.exe / 2025Patch.dll / 2025patch.ini
/// is missing (e.g. antivirus quarantine), say so plainly instead of
/// launching into that guaranteed failure. Re-running setup restores them.
pub fn launch_game(dir: &Path, progress: &Progress) {
    // v0.3.9: Clear Unity HTTP cache before launch so the client always
    // fetches fresh backend data (storefront types, box art, etc.) instead
    // of using stale cached responses from before backend fixes deployed.
    clear_unity_http_cache(progress);
    match crate::find_game_exe(dir) {
        Some(exe) => {
            // 2026 client has Referee anti-cheat service (RefereeClientInstaller.exe)
            // which kills the game if it detects DLL injection. Skip the
            // 2025Patch injector for 2026 and rely on the hosts-file redirect
            // instead. The 2025 client also has Referee.dll but no service —
            // it NEEDS the injector.
            let is_2026 = dir.join("RefereeClientInstaller.exe").exists()
                || dir.join("RecRoom_Data").is_dir();
            if !is_2026 {
                let injector = dir.join("Injector.exe");
                let patch_dll = dir.join("2025Patch.dll");
                let patch_ini = dir.join("2025patch.ini");
                let mut missing = Vec::new();
                if !injector.is_file() {
                    missing.push("Injector.exe");
                }
                if !patch_dll.is_file() {
                    missing.push("2025Patch.dll");
                }
                if !patch_ini.is_file() {
                    missing.push("2025patch.ini");
                }
                if !missing.is_empty() {
                    let msg = format!(
                        "Patch files missing ({}): the game cannot reach Flux Rec without them. Re-run setup to restore them.",
                        missing.join(", ")
                    );
                    eprintln!("[launcher] {msg}");
                    progress.set_status(&msg, 100);
                    std::thread::sleep(Duration::from_secs(10));
                    return;
                }
                progress.set_status("Launching game\u{2026}", 100);
                // 2025Patch injector goes first — it attaches when the game loads.
                println!("[launcher] starting injector: {}", injector.display());
                crate::stealth::launch_hidden(&injector, &[]);
                std::thread::sleep(Duration::from_millis(500));
            } else {
                println!("[launcher] 2026 client detected (Referee anti-cheat) — skipping injector.");
                progress.set_status("Launching game\u{2026}", 100);
            }
            println!("[launcher] starting game: {}", exe.display());
            crate::stealth::launch_hidden(&exe, &["+forcemode:screen"]);
            std::thread::sleep(Duration::from_secs(3));
        }
        None => {
            progress.set_status("Game files not found \u{2014} please reinstall.", 100);
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}
