//! Ripo Team Launcher v1.0.26 — game library flow.
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
    /// User explicitly requested an uncached update check.
    CheckForUpdates,
    /// Open the configured game install directory in Explorer.
    OpenInstallFolder,
    /// Select and persist a new Flux Rec install directory.
    SetInstallDir(PathBuf),
    /// User changed the theme. The GUI applies it live; we persist it.
    SetTheme(gui_library::Theme),
    /// Verify and repair the files this launcher can authoritatively restore.
    VerifyFiles,
    /// The player completed the first-run launcher welcome.
    CompleteOnboarding,
    /// Open Flux Social's existing sign-in page in the default browser.
    OpenFluxLogin,
    /// Open Flux Social's existing account creation page in the default browser.
    OpenFluxSignup,
    /// Open Flux Social's account-link page in the default browser.
    OpenFluxSettings,
    /// User closed the window.
    Close,
}

/// State updates from the worker thread to the GUI thread.
pub enum LibMsg {
    /// Pre-launch checks finished: ready to play, with update info.
    Ready {
        generation: u64,
        update_available: bool,
        update_version: String,
    },
    /// Checks are running (fast path).
    Checking { generation: u64 },
    /// No game executable exists yet; the primary action should say Install.
    InstallRequired { generation: u64 },
    /// Check failed but the game can still launch (fail-soft).
    CheckFailed { generation: u64, message: String },
    /// A new install directory was validated and persisted.
    InstallDirChanged { path: PathBuf, generation: u64 },
    /// The requested install directory could not be applied.
    InstallDirChangeFailed(String),
    /// Live player count; `None` means the request failed, not zero players.
    PlayerCount(Option<u32>),
    /// Result of an explicit core-file verification and repair pass.
    VerificationFinished {
        generation: u64,
        message: String,
        repaired: bool,
    },
    /// Progress update during update-download/install (reuses GuiMsg shape).
    Progress {
        percent: u8,
        stage: String,
        detail: String,
    },
    /// Tell the GUI to close (game is launching; splash takes over).
    CloseGui,
}

/// Where the launcher stores its small state files.
fn state_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(|l| PathBuf::from(l).join("RipoTeamLauncher"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

const ONBOARDING_MARKER: &str = "launcher-onboarding-v1";

fn is_onboarding_complete(value: &str) -> bool {
    value.trim() == ONBOARDING_MARKER
}

fn onboarding_complete() -> bool {
    std::fs::read_to_string(state_dir().join("onboarding.txt"))
        .map(|value| is_onboarding_complete(&value))
        .unwrap_or(false)
}

fn save_onboarding_complete() {
    let path = state_dir().join("onboarding.txt");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, ONBOARDING_MARKER);
}

/// Last time we did the slow network checks (update check, plugin sync,
/// logo bundles). Cached for 1 hour so launching stays fast.
fn last_check_path(dir: &Path) -> PathBuf {
    let key = format!("{:x}", md5::compute(dir.to_string_lossy().to_lowercase().as_bytes()));
    state_dir().join(format!("last_check_{key}.txt"))
}

/// Returns true if the network checks were done recently (< 1 hour ago).
pub fn network_check_fresh(dir: &Path) -> bool {
    let p = last_check_path(dir);
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
pub fn mark_network_check(dir: &Path) {
    let p = last_check_path(dir);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(&p, now.to_string());
}

/// Load the saved theme; new installations use the requested light palette.
pub fn load_theme() -> gui_library::Theme {
    let p = state_dir().join("theme.txt");
    theme_from_setting(&std::fs::read_to_string(&p).unwrap_or_default())
}

fn theme_from_setting(setting: &str) -> gui_library::Theme {
    match setting.trim() {
        "dark" => gui_library::Theme::Dark,
        _ => gui_library::Theme::Light,
    }
}

#[cfg(test)]
mod theme_tests {
    use super::{is_onboarding_complete, theme_from_setting, ONBOARDING_MARKER};
    use crate::gui_library::Theme;

    #[test]
    fn fresh_or_invalid_setting_uses_light_theme() {
        assert_eq!(theme_from_setting(""), Theme::Light);
        assert_eq!(theme_from_setting("unknown"), Theme::Light);
    }

    #[test]
    fn saved_theme_choice_is_respected() {
        assert_eq!(theme_from_setting("light\n"), Theme::Light);
        assert_eq!(theme_from_setting("dark\n"), Theme::Dark);
    }

    #[test]
    fn onboarding_marker_is_versioned_and_requires_an_exact_match() {
        assert!(is_onboarding_complete(ONBOARDING_MARKER));
        assert!(is_onboarding_complete("launcher-onboarding-v1\n"));
        assert!(!is_onboarding_complete(""));
        assert!(!is_onboarding_complete("launcher-onboarding-v0"));
    }
}

#[cfg(test)]
mod repair_tests {
    use super::{smart_repair, LibMsg};
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn smart_repair_restores_modified_launcher_owned_binaries() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-core-repair-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Recroom_Release.exe"), b"MZtest").unwrap();
        std::fs::write(dir.join("2025Patch.dll"), b"modified patch").unwrap();
        std::fs::write(dir.join("Injector.exe"), b"modified injector").unwrap();
        std::fs::write(dir.join("2025patch.ini"), b"[config]\n").unwrap();
        for index in 0..110 {
            std::fs::write(dir.join(format!("game-file-{index}.dat")), b"x").unwrap();
        }
        let (tx, _rx) = mpsc::channel::<LibMsg>();

        let result = smart_repair(
            &dir,
            "ns.example.test",
            "photon.example.test",
            "voice.example.test",
            "chat.example.test",
            &tx,
        );

        assert_eq!(result, Ok(true));
        assert_eq!(
            std::fs::read(dir.join("2025Patch.dll")).unwrap(),
            include_bytes!("../assets/2025Patch.dll")
        );
        assert_eq!(
            std::fs::read(dir.join("Injector.exe")).unwrap(),
            include_bytes!("../assets/Injector.exe")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod install_dir_tests {
    use super::validate_install_dir;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fluxrec-install-dir-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn accepts_empty_target_and_existing_flux_rec_install() {
        let current = scratch_dir("current");
        let empty_target = scratch_dir("empty");
        let game_target = scratch_dir("game");
        std::fs::write(game_target.join("Recroom_Release.exe"), b"test").unwrap();

        assert_eq!(
            validate_install_dir(&empty_target, &current),
            std::fs::canonicalize(&empty_target).map_err(|error| error.to_string())
        );
        assert_eq!(
            validate_install_dir(&game_target, &current),
            std::fs::canonicalize(&game_target).map_err(|error| error.to_string())
        );

        let _ = std::fs::remove_dir_all(current);
        let _ = std::fs::remove_dir_all(empty_target);
        let _ = std::fs::remove_dir_all(game_target);
    }

    #[test]
    fn rejects_unrelated_nonempty_folder_but_allows_current_path() {
        let current = scratch_dir("current");
        let unrelated = scratch_dir("unrelated");
        std::fs::write(unrelated.join("notes.txt"), b"keep this").unwrap();

        assert!(validate_install_dir(&unrelated, &current).is_err());
        assert_eq!(
            validate_install_dir(&current, &current),
            std::fs::canonicalize(&current).map_err(|error| error.to_string())
        );

        let _ = std::fs::remove_dir_all(current);
        let _ = std::fs::remove_dir_all(unrelated);
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

fn path_identity(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolved
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase()
}

fn validate_install_dir(selected: &Path, current: &Path) -> Result<PathBuf, String> {
    let selected = std::fs::canonicalize(selected)
        .map_err(|_| "Choose an existing folder for Flux Rec.".to_string())?;
    if !selected.is_dir() {
        return Err("The selected location is not a folder.".to_string());
    }
    if path_identity(&selected) == path_identity(current)
        || crate::find_game_exe(&selected).is_some()
    {
        return Ok(selected);
    }

    let has_launcher_files = [
        "2025Patch.dll",
        "Injector.exe",
        "2025patch.ini",
        "Recroom_Release_Data",
    ]
    .iter()
    .any(|name| selected.join(name).exists());
    if has_launcher_files {
        return Ok(selected);
    }

    let mut entries = std::fs::read_dir(&selected)
        .map_err(|_| "The selected folder could not be read.".to_string())?;
    if entries.next().is_none() {
        return Ok(selected);
    }
    Err("Choose an empty folder or an existing Flux Rec install folder.".to_string())
}

/// Store the selected folder in the same file used by future game-shortcut launches.
fn persist_install_dir(dir: &Path) -> Result<(), String> {
    let local_appdata = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "Windows did not provide a LOCALAPPDATA folder.".to_string())?;
    let file = PathBuf::from(local_appdata)
        .join("FluxRec")
        .join("install_dir.txt");
    let parent = file
        .parent()
        .ok_or_else(|| "Could not locate the install settings folder.".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Could not save the install location: {error}"))?;
    std::fs::write(&file, format!("{}\n", dir.display()))
        .map_err(|error| format!("Could not save the install location: {error}"))
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
        let response = client
            .get("https://www.ripo-ripoteam.workers.dev/server-status")
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let text = response.text().await.ok()?;
        parse_live_player_count(&text)
    })
}

fn parse_live_player_count(body: &str) -> Option<u32> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    if json.get("status")?.as_str()? != "online" {
        return None;
    }
    u32::try_from(json.get("players")?.as_u64()?).ok()
}

#[cfg(test)]
mod player_count_tests {
    use super::parse_live_player_count;

    #[test]
    fn accepts_a_live_zero_as_a_real_count() {
        assert_eq!(
            parse_live_player_count(r#"{"status":"online","players":0}"#),
            Some(0)
        );
    }

    #[test]
    fn accepts_a_positive_live_count() {
        assert_eq!(
            parse_live_player_count(r#"{"status":"online","players":12}"#),
            Some(12)
        );
    }

    #[test]
    fn rejects_wrong_status_schema_and_overflow() {
        assert_eq!(
            parse_live_player_count(r#"{"status":"offline","players":12}"#),
            None
        );
        assert_eq!(
            parse_live_player_count(r#"{"status":"online","count":12}"#),
            None
        );
        assert_eq!(
            parse_live_player_count(r#"{"status":"online","players":"12"}"#),
            None
        );
        assert_eq!(
            parse_live_player_count(r#"{"status":"online","players":4294967296}"#),
            None
        );
        assert_eq!(parse_live_player_count("not json"), None);
    }
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
    let mut install_dir = dir.to_path_buf();
    let mut install_dir_generation = 0u64;

    // GUI thread: the library window.
    let gui_config = gui_library::LibraryConfig {
        theme,
        version: version.clone(),
        install_dir: install_dir.clone(),
        show_onboarding: !onboarding_complete(),
        cmd_tx: cmd_tx.clone(),
    };
    let gui_thread = std::thread::spawn(move || {
        gui_library::run_library_gui(gui_config, msg_rx);
    });

    // Worker thread: fast pre-launch checks + player count.
    let dir_w = install_dir.clone();
    let ns_w = ns_host.to_string();
    let msg_tx_w = msg_tx.clone();
    std::thread::spawn(move || {
        worker_checks(&dir_w, &ns_w, 0, &msg_tx_w);
    });

    // Main thread: route GUI commands.
    loop {
        match cmd_rx.recv() {
            Ok(LibCmd::Play) => {
                do_play(&install_dir, ns_host, photon_rt, photon_voice, photon_chat, &msg_tx);
                // do_play never returns (launches or exits)
            }
            Ok(LibCmd::Uninstall) => {
                do_uninstall(&install_dir, &msg_tx);
            }
            Ok(LibCmd::CheckForUpdates) => {
                let check_dir = install_dir.clone();
                let generation = install_dir_generation;
                let check_tx = msg_tx.clone();
                std::thread::spawn(move || force_update_check(&check_dir, generation, &check_tx));
            }
            Ok(LibCmd::OpenInstallFolder) => {
                let _ = std::process::Command::new("explorer.exe").arg(&install_dir).spawn();
            }
            Ok(LibCmd::SetInstallDir(selected)) => {
                match validate_install_dir(&selected, &install_dir)
                    .and_then(|validated| persist_install_dir(&validated).map(|()| validated))
                {
                    Ok(validated) => {
                        install_dir = validated;
                        install_dir_generation = install_dir_generation.wrapping_add(1);
                        let _ = msg_tx.send(LibMsg::InstallDirChanged {
                            path: install_dir.clone(),
                            generation: install_dir_generation,
                        });
                        let check_dir = install_dir.clone();
                        let generation = install_dir_generation;
                        let check_ns = ns_host.to_string();
                        let check_tx = msg_tx.clone();
                        std::thread::spawn(move || {
                            worker_checks(&check_dir, &check_ns, generation, &check_tx);
                        });
                    }
                    Err(message) => {
                        let _ = msg_tx.send(LibMsg::InstallDirChangeFailed(message));
                    }
                }
            }
            Ok(LibCmd::VerifyFiles) => {
                let verify_dir = install_dir.clone();
                let generation = install_dir_generation;
                let verify_ns = ns_host.to_string();
                let verify_rt = photon_rt.to_string();
                let verify_voice = photon_voice.to_string();
                let verify_chat = photon_chat.to_string();
                let verify_tx = msg_tx.clone();
                std::thread::spawn(move || {
                    let (message, repaired) = verify_and_repair_core_files(
                        &verify_dir,
                        &verify_ns,
                        &verify_rt,
                        &verify_voice,
                        &verify_chat,
                        &verify_tx,
                    );
                    let _ = verify_tx.send(LibMsg::VerificationFinished {
                        generation,
                        message,
                        repaired,
                    });
                });
            }
            Ok(LibCmd::SetTheme(t)) => {
                save_theme(t);
            }
            Ok(LibCmd::CompleteOnboarding) => {
                save_onboarding_complete();
            }
            Ok(LibCmd::OpenFluxLogin) => {
                crate::launcher::open_url_in_browser(
                    "https://flux.sitey.my/login?next=%2Fflux-rec%3Flink%3Dfluxrec",
                );
            }
            Ok(LibCmd::OpenFluxSignup) => {
                crate::launcher::open_url_in_browser(
                    "https://flux.sitey.my/signup?next=%2Fflux-rec%3Flink%3Dfluxrec",
                );
            }
            Ok(LibCmd::OpenFluxSettings) => {
                crate::launcher::open_url_in_browser("https://flux.sitey.my/flux-rec?link=fluxrec");
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
fn worker_checks(dir: &Path, _ns_host: &str, generation: u64, msg_tx: &Sender<LibMsg>) {
    let _ = msg_tx.send(LibMsg::Checking { generation });

    // Player count is independent and can take several seconds offline; do
    // not hold the install/update readiness state behind that request.
    let player_tx = msg_tx.clone();
    std::thread::spawn(move || {
        let _ = player_tx.send(LibMsg::PlayerCount(fetch_player_count()));
    });

    // If the game was never installed, the library shows "Install" state.
    // The PLAY button will run the full install.
    let game_exe = crate::find_game_exe(dir);
    if game_exe.is_none() {
        let _ = msg_tx.send(LibMsg::InstallRequired { generation });
        return;
    }

    // Update check: skip the network if we checked recently.
    if network_check_fresh(dir) {
        let _ = msg_tx.send(LibMsg::Ready {
            generation,
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
        Err(_) => crate::updater::UpdateDecision::CheckFailed,
    };
    if !matches!(&decision, crate::updater::UpdateDecision::CheckFailed) {
        mark_network_check(dir);
    }

    match decision {
        crate::updater::UpdateDecision::Available { version, .. } => {
            let _ = msg_tx.send(LibMsg::Ready {
                generation,
                update_available: true,
                update_version: version,
            });
        }
        // v1.0.5: tell the UI the check failed so it can warn the user.
        crate::updater::UpdateDecision::CheckFailed => {
            let _ = msg_tx.send(LibMsg::CheckFailed {
                generation,
                message: "Couldn't reach GitHub to check for updates. Check your connection.".to_string(),
            });
        }
        _ => {
            let _ = msg_tx.send(LibMsg::Ready {
                generation,
                update_available: false,
                update_version: String::new(),
            });
        }
    }
}

/// Run a user-requested live update check without the launch-time one-hour cache.
fn force_update_check(dir: &Path, generation: u64, msg_tx: &Sender<LibMsg>) {
    let _ = msg_tx.send(LibMsg::Checking { generation });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let decision = match rt {
        Ok(runtime) => {
            let (progress, _rx) = crate::progress::channel();
            runtime.block_on(crate::updater::check_for_updates(dir, &progress))
        }
        Err(_) => {
            let _ = msg_tx.send(LibMsg::CheckFailed {
                generation,
                message: "Couldn't start the update check. Flux Rec can still be launched.".to_string(),
            });
            return;
        }
    };
    match decision {
        crate::updater::UpdateDecision::Available { version, .. } => {
            let _ = msg_tx.send(LibMsg::Ready {
                generation,
                update_available: true,
                update_version: version,
            });
        }
        crate::updater::UpdateDecision::CheckFailed => {
            let _ = msg_tx.send(LibMsg::CheckFailed {
                generation,
                message: "Couldn't reach GitHub to check for updates. Check your connection.".to_string(),
            });
        }
        _ => {
            let _ = msg_tx.send(LibMsg::Ready {
                generation,
                update_available: false,
                update_version: String::new(),
            });
        }
    }
}

/// v1.0.8: Smart repair — scan the game folder and only fix what's actually
/// broken, instead of nuking everything for one bad file.
/// Returns Ok(true) if the install is healthy, Ok(false) if it needs a full
/// reinstall, Err(msg) if repair failed.
fn smart_repair(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    msg_tx: &Sender<LibMsg>,
) -> Result<bool, String> {
    let send = |percent: u8, stage: &str| {
        let _ = msg_tx.send(LibMsg::Progress {
            percent,
            stage: stage.to_string(),
            detail: String::new(),
        });
    };

    // Count files to gauge install health.
    let file_count = count_files(dir);
    println!("[smart-repair] found {} files in game dir", file_count);

    // If the dir is essentially empty, do a full install.
    if file_count < 100 {
        println!("[smart-repair] dir nearly empty — full install needed");
        return Ok(false);
    }

    // Dir has substance — do targeted repairs.
    send(70, "Scanning game files…");
    let mut repaired = 0;

    // 1. Patch files: restore from embedded (no download).
    {
        const PATCH_DLL: &[u8] = include_bytes!("../assets/2025Patch.dll");
        const INJECTOR: &[u8] = include_bytes!("../assets/Injector.exe");
        let patch_dll = dir.join("2025Patch.dll");
        let injector = dir.join("Injector.exe");
        let patch_ini = dir.join("2025patch.ini");

        let patch_hash = format!("{:x}", md5::compute(PATCH_DLL));
        if !crate::file_ok(&patch_dll, Some(&patch_hash), Some(PATCH_DLL.len() as u64)) {
            println!("[smart-repair] restoring 2025Patch.dll from embedded");
            let _ = std::fs::write(&patch_dll, PATCH_DLL);
            repaired += 1;
        }
        let injector_hash = format!("{:x}", md5::compute(INJECTOR));
        if !crate::file_ok(&injector, Some(&injector_hash), Some(INJECTOR.len() as u64)) {
            println!("[smart-repair] restoring Injector.exe from embedded");
            let _ = std::fs::write(&injector, INJECTOR);
            repaired += 1;
        }
        if !patch_ini.is_file() {
            println!("[smart-repair] restoring 2025patch.ini");
            let _ = std::fs::write(&patch_ini, "[2025Patch]\n");
            repaired += 1;
        }
    }

    // 2. Game exe: check exists and has valid MZ header.
    // v1.0.8: DO NOT delete on validation failure — the icon patch is disabled,
    // and a false positive would nuke 3.6GB. Just warn.
    if let Some(exe) = crate::find_game_exe(dir) {
        if !has_valid_mz(&exe) {
            eprintln!(
                "[smart-repair] WARNING: game exe has invalid header: {}",
                exe.display()
            );
            // Don't delete — let the user decide. Return false to trigger
            // a manual reinstall prompt, not an automatic nuke.
            return Err("Game exe appears corrupted. Please reinstall from Setup.".to_string());
        }
    } else {
        // Exe missing but dir has files — strange, but don't nuke.
        // The full install check at the top of do_play will handle it.
        return Ok(false);
    }

    if repaired > 0 {
        println!("[smart-repair] repaired {} files", repaired);
    } else {
        println!("[smart-repair] all critical files OK");
    }
    Ok(true)
}

/// v1.0.11: Download a single missing game file from the HF mirror.
/// The HF dataset has 6,358 individual game files — no need to re-download
/// the 3.8GB zip for one missing file.
async fn restore_missing_file(
    client: &reqwest::Client,
    dir: &Path,
    relative_path: &str,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    let url = format!(
        "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/{}",
        relative_path
    );
    let dest = dir.join(relative_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    println!("[restore] downloading missing file: {}", relative_path);
    crate::download(client, &url, &dest, None, None, relative_path, None).await
}
fn count_files(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                count += 1;
            } else if path.is_dir() {
                // Don't recurse into huge dirs for speed — just count top 2 levels
                if let Ok(sub) = std::fs::read_dir(&path) {
                    for sub_entry in sub.flatten() {
                        if sub_entry.path().is_file() {
                            count += 1;
                        }
                        if count > 5000 {
                            return count; // Enough to know it's a full install
                        }
                    }
                }
            }
            if count > 5000 {
                return count;
            }
        }
    }
    count
}

/// Check if a file has a valid MZ (DOS) header.
fn has_valid_mz(path: &Path) -> bool {
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; 2];
        if f.read_exact(&mut buf).is_ok() {
            return buf == [b'M', b'Z'];
        }
    }
    false
}

/// Verify the files the launcher can authoritatively restore. Game asset
/// bundles do not have a complete signed manifest in this build, so this
/// deliberately limits itself to known launcher-managed components.
fn verify_and_repair_core_files(
    dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    msg_tx: &Sender<LibMsg>,
) -> (String, bool) {
    if crate::find_game_exe(dir).is_none() {
        return ("Flux Rec is not installed at this location.".to_string(), false);
    }

    const PATCH_DLL: &[u8] = include_bytes!("../assets/2025Patch.dll");
    const INJECTOR: &[u8] = include_bytes!("../assets/Injector.exe");
    let patch_dll_was_healthy = crate::file_ok(
        &dir.join("2025Patch.dll"),
        Some(&format!("{:x}", md5::compute(PATCH_DLL))),
        Some(PATCH_DLL.len() as u64),
    );
    let injector_was_healthy = crate::file_ok(
        &dir.join("Injector.exe"),
        Some(&format!("{:x}", md5::compute(INJECTOR))),
        Some(INJECTOR.len() as u64),
    );
    let patch_ini_was_present = dir.join("2025patch.ini").is_file();
    let bypass_was_healthy = matches!(
        crate::bypass::verify(dir),
        crate::bypass::BypassState::Ok(_)
    );
    let exe = match crate::find_game_exe(dir) {
        Some(path) => path,
        None => return ("Flux Rec game executable is missing.".to_string(), false),
    };
    if !has_valid_mz(&exe) {
        return (
            "The game executable is damaged. Reinstall the game to restore it safely.".to_string(),
            false,
        );
    }

    let file_count = count_files(dir);
    if file_count < 100 {
        return (
            "The install is incomplete. Run the installer to restore the game files.".to_string(),
            false,
        );
    }

    if let Err(error) = smart_repair(dir, ns_host, photon_rt, photon_voice, photon_chat, msg_tx) {
        return (format!("Core file check failed: {error}"), false);
    }
    let progress_tx = msg_tx.clone();
    let (progress, progress_rx) = crate::progress::channel();
    std::thread::spawn(move || {
        for update in progress_rx {
            let _ = progress_tx.send(LibMsg::Progress {
                percent: update.percent,
                stage: update.stage,
                detail: update.detail,
            });
        }
    });

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return (format!("Could not start file repair: {error}"), false);
        }
    };
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(error) => return (format!("Could not prepare file repair: {error}"), false),
    };

    let patch_was_healthy = crate::bepinex::verify_for_launcher(dir).is_ok();
    let mut repaired = !patch_dll_was_healthy || !injector_was_healthy || !patch_ini_was_present;
    if !patch_was_healthy {
        match runtime.block_on(crate::bepinex::repair_for_launcher(
            &client,
            dir,
            ns_host,
            photon_rt,
            photon_voice,
            photon_chat,
            &progress,
        )) {
            Ok(()) => repaired = true,
            Err(error) => return (format!("Could not repair the game patch: {error}"), false),
        }
    }
    if !bypass_was_healthy {
        match runtime.block_on(crate::bypass::repair_bypass(&client, dir, &progress)) {
            Ok(_) => repaired = true,
            Err(error) => return (format!("Could not repair the game launch files: {error}"), false),
        }
    }
    match runtime.block_on(crate::apply_logo_bundle(&client, dir, &progress)) {
        Ok(branding_repaired) => repaired |= branding_repaired,
        Err(error) => {
            return (
                format!("Core files checked, but branding repair failed: {error}"),
                repaired,
            )
        }
    }

    if let Err(error) = crate::verify_install(dir) {
        return (format!("Some required files are still missing: {error}"), repaired);
    }
    let message = if repaired {
        "Launcher-managed core files were repaired and checked. This build cannot verify every game asset because it has no complete asset hash manifest.".to_string()
    } else {
        "Launcher-managed core files passed verification. This build cannot verify every game asset because it has no complete asset hash manifest.".to_string()
    };
    (message, repaired)
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

    // v1.0.8: Smart repair FIRST — scan the folder and fix only what's broken.
    // If the dir has substance (>100 files), do targeted repairs instead of
    // a full 3.6GB reinstall.
    let needs_full_install =
        match smart_repair(dir, ns_host, photon_rt, photon_voice, photon_chat, msg_tx) {
            Ok(healthy) => !healthy,
            Err(e) => {
                send(100, &e);
                std::thread::sleep(Duration::from_secs(6));
                std::process::exit(1);
            }
        };

    // If the game was never installed (or smart repair says it's empty),
    // run the full install.
    if needs_full_install || crate::find_game_exe(dir).is_none() {
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
                    dir,
                    ns_host,
                    photon_rt,
                    photon_voice,
                    photon_chat,
                    &p2,
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
                    &c,
                    dir,
                    ns_host,
                    photon_rt,
                    photon_voice,
                    photon_chat,
                    &p,
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

    // Update check on PLAY: ALWAYS check live (v1.0.7 fix).
    // The 1-hour cache is for the background worker at startup, but when the
    // user explicitly presses PLAY, they expect a fresh check. The old code
    // skipped the check if the worker had run recently, so updates were missed.
    {
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
                    mark_network_check(dir);
                    send(84, &format!("Update {version} found — installing…"));
                    let dest = std::env::temp_dir().join("RipoTeamLauncher-update.exe");
                    let downloaded = {
                        let (p2, rx2) = crate::progress::channel();
                        let msg_tx_c = msg_tx.clone();
                        std::thread::spawn(move || {
                            for msg in rx2 {
                                let source_pct = msg.percent.clamp(8, 80);
                                let percent = 84 + (u16::from(source_pct - 8) * 6 / 72) as u8;
                                let _ = msg_tx_c.send(LibMsg::Progress {
                                    percent,
                                    stage: msg.stage,
                                    detail: msg.detail,
                                });
                            }
                        });
                        r.block_on(crate::updater::download_update(&download_url, &dest, &p2))
                            .is_ok()
                    };
                    if downloaded {
                        // The fresh setup installs and launches the game itself.
                        let dir_s = dir.to_string_lossy().to_string();
                        send(91, "Installing update…");
                        let _ = msg_tx.send(LibMsg::CloseGui);
                        std::thread::sleep(Duration::from_millis(300));
                        let (p3, _rx3) = crate::progress::channel();
                        if crate::launcher::spawn_update(&dest, &dir_s, &p3) {
                            std::process::exit(0);
                        }
                        // Never strand a player because the downloaded setup
                        // could not be started or completed.
                        #[cfg(windows)]
                        crate::message_box(
                            "Flux Rec update",
                            "The update could not be completed. Flux Rec will start using the installed version.",
                            false,
                        );
                    }
                    send(90, "Update failed — launching installed version…");
                }
                crate::updater::UpdateDecision::CheckFailed => {
                    send(
                        83,
                        "Update check unavailable — launching installed version…",
                    );
                }
                crate::updater::UpdateDecision::UpToDate => mark_network_check(dir),
            }
        } else {
            send(
                83,
                "Update check unavailable — launching installed version…",
            );
        }
    }

    // v1.0.6 EMERGENCY: icon patch DISABLED — it was corrupting Recroom_Release.exe
    // on some systems, preventing the game from launching. The taskbar icon
    // fix is postponed until the patch is proven safe.
    // {
    //     let flag = dir.join(".icon_patched_v1");
    //     if !flag.exists() {
    //         if let Some(exe) = crate::find_game_exe(dir) {
    //             const ICON_BYTES: &[u8] = include_bytes!("../assets/fluxrec.ico");
    //             match crate::icon_patch::patch_exe_icon(&exe, ICON_BYTES) {
    //                 Ok(true) => {
    //                     println!("[icon] patched game exe icon.");
    //                     let _ = std::fs::write(&flag, "1");
    //                 }
    //                 Ok(false) => {
    //                     let _ = std::fs::write(&flag, "1");
    //                 }
    //                 Err(e) => eprintln!("[icon] icon patch failed ({}); continuing.", e),
    //             }
    //         }
    //     }
    // }

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
