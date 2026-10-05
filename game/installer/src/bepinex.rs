//! v0.3.0: 2025 client uses 2025Patch (native DLL injection) instead of BepInEx.
//!
//! This module keeps the public interface the launcher and defender expect
//! (`verify_for_launcher` / `repair_for_launcher` / `sync_plugin_from_mirror`)
//! but operates on the 2025Patch files (`2025Patch.dll`, `Injector.exe`,
//! `2025patch.ini`) instead of the BepInEx layout. There is no BepInEx and no
//! RecNetPlugin in the 2025 client — the patch DLL does the host rewriting
//! natively.

use std::path::Path;

/// 2025Patch files that must exist next to `Recroom_Release.exe`.
const PATCH_FILES: &[&str] = &["2025Patch.dll", "Injector.exe", "2025patch.ini"];

/// Quick 2025Patch health check for the launcher (`--play`).
/// Returns Ok when the patch can boot, Err(reason) when it needs repair.
pub fn verify_for_launcher(game_dir: &Path) -> Result<(), String> {
    for f in PATCH_FILES {
        if !game_dir.join(f).is_file() {
            return Err(format!("missing {f}"));
        }
    }
    Ok(())
}

/// Repair a broken 2025Patch install from the launcher: re-downloads the
/// patch zip and reinstalls it in place (with rollback).
pub async fn repair_for_launcher(
    client: &reqwest::Client,
    game_dir: &Path,
    ns_host: &str,
    _photon_rt: &str,
    _photon_voice: &str,
    _photon_chat: &str,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    let patch_zip = crate::fetch_patch2025_zip(client, progress).await?;
    let mut backups = crate::transaction::BackupSet::new();
    // Protect the existing patch files so a failed reinstall rolls back.
    for f in PATCH_FILES {
        let _ = backups.protect(&game_dir.join(f));
    }
    let res = crate::install_patch2025(game_dir, &patch_zip, ns_host, progress);
    let _ = std::fs::remove_file(&patch_zip);
    match res {
        Ok(()) => {
            backups.commit();
            Ok(())
        }
        Err(e) => {
            backups.rollback();
            Err(e)
        }
    }
}

/// 2025Patch auto-update: copies the embedded fixed 2025Patch.dll (with
/// jsonfix for clothing tab categorization) to the game folder on every
/// --play. This ensures the client gets the tab fix via the installer's
/// auto-update mechanism.
pub async fn sync_plugin_from_mirror(
    game_dir: &Path,
    _progress: &crate::progress::Progress,
) {
    // Embedded fixed DLL (built from patch2025/src with jsonfix)
    const FIXED_PATCH: &[u8] = include_bytes!("../assets/2025Patch.dll");
    let dest = game_dir.join("2025Patch.dll");
    // Only overwrite if the embedded version is different (avoid unnecessary writes)
    let needs_update = match std::fs::read(&dest) {
        Ok(existing) => existing != FIXED_PATCH,
        Err(_) => true,
    };
    if needs_update {
        if let Err(e) = std::fs::write(&dest, FIXED_PATCH) {
            eprintln!("[2025Patch] failed to update: {}", e);
        } else {
            println!("[2025Patch] updated to fixed version (clothing tab fix)");
        }
    }
}
