//! FluxLoader installation — the BepInEx replacement (v0.2.0).
//!
//! Layout inside the game directory:
//!
//! ```text
//! <game>/
//!   winhttp.dll                  Doorstop 4.5.0 proxy (embedded, official binary)
//!   doorstop_config.ini          generated: target_assembly=FluxLoader\core\FluxLoader.Bootstrap.dll
//!   dotnet/                      trimmed CoreCLR 6.0.7 (subset-extracted from the BepInEx zip, game root)
//!   FluxLoader/
//!     core/                      FluxLoader.Bootstrap.dll + Il2CppInterop + HarmonyX (embedded)
//!     interop/                   20230414 interop assemblies (embedded zip, hash-gated)
//!     plugins/RecNetPlugin.dll   ported plugin (embedded)
//!     config/net.rec.plugin.cfg  generated (migrates the old BepInEx config once)
//!     logs/                      created at runtime
//! ```
//!
//! The old `BepInEx/` tree is removed only after the new loader verifies.

use std::fs;
use std::path::{Path, PathBuf};

/// GameAssembly.dll MD5 the embedded interop zip was generated from.
/// The interop install refuses to proceed if the client's GameAssembly
/// differs — silently loading stale interop would crash the game.
pub const INTEROP_GAMEASSEMBLY_MD5: &str = "746a8d9b49671f1329c8f7627583b02d";

/// Official Doorstop 4.5.0 x64 proxy binary (embedded).
const WINHTTP_DLL: &[u8] = include_bytes!("../assets/fluxloader/winhttp.dll");

/// Embedded 20230414 interop assemblies (generated from the pinned client).
const INTEROP_ZIP: &[u8] = include_bytes!("../assets/fluxloader-interop-20230414.zip");

/// Ported RecNetPlugin (FluxLoader build, no BepInEx reference).
pub(crate) const RECNET_PLUGIN_DLL: &[u8] =
    include_bytes!("../assets/fluxloader/plugins/RecNetPlugin.dll");

/// FluxLoader core assemblies (embedded). (filename, bytes).
/// Finalized from the real bootstrap build output — the build fails if any
/// file is missing, so this table can never silently drift.
const CORE_DLLS: &[(&str, &[u8])] = &[
    (
        "FluxLoader.Bootstrap.dll",
        include_bytes!("../assets/fluxloader/core/FluxLoader.Bootstrap.dll"),
    ),
    (
        "Il2CppInterop.Runtime.dll",
        include_bytes!("../assets/fluxloader/core/Il2CppInterop.Runtime.dll"),
    ),
    (
        "Il2CppInterop.HarmonySupport.dll",
        include_bytes!("../assets/fluxloader/core/Il2CppInterop.HarmonySupport.dll"),
    ),
    (
        "0Harmony.dll",
        include_bytes!("../assets/fluxloader/core/0Harmony.dll"),
    ),
    (
        "MonoMod.Utils.dll",
        include_bytes!("../assets/fluxloader/core/MonoMod.Utils.dll"),
    ),
    (
        "Mono.Cecil.dll",
        include_bytes!("../assets/fluxloader/core/Mono.Cecil.dll"),
    ),
];

const DOORSTOP_INI: &str = "[UnityDoorstop]\r\n\
    enabled=true\r\n\
    target_assembly=FluxLoader\\core\\FluxLoader.Bootstrap.dll\r\n\
    require_configuration=false\r\n\
    ignore_disable_switch=false\r\n\
    [Il2Cpp]\r\n\
    coreclr_path=dotnet\\coreclr.dll\r\n";

/// Files inside the BepInEx zip we actually need: the winhttp proxy is
/// embedded separately (official Doorstop 4.5.0 binary); from the zip we
/// only subset-extract the trimmed CoreCLR runtime.
/// Prefix inside the BepInEx zip for the trimmed CoreCLR runtime. NOTE: in
/// the BepInEx 6.0.0-pre.2 archive, `dotnet/` sits at the ARCHIVE ROOT
/// (verified 2026-09-27), so it extracts to `<game>/dotnet/` — the exact
/// layout BepInEx itself uses, which Doorstop's `coreclr_path` expects.
const DOTNET_SUBSET_PREFIX: &str = "dotnet/";

/// Install (or refresh) FluxLoader into `game_dir`.
///
/// * `bepinex_zip` — path to the downloaded BepInEx zip, used ONLY as the
///   source of the trimmed CoreCLR runtime (`dotnet/` at the archive root).
///   `None` skips the dotnet step (caller guarantees it already exists).
/// * `backups` — every file overwritten is protected for rollback.
/// * `progress` — stage is expected to be "Installing FluxLoader…".
///
/// Hard-fails on any error: a half-installed loader is worse than none.
pub fn install_fluxloader(
    game_dir: &Path,
    bepinex_zip: Option<&Path>,
    backups: &mut crate::transaction::BackupSet,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    let flux = game_dir.join("FluxLoader");
    let core_dir = flux.join("core");
    let plugins_dir = flux.join("plugins");
    let config_dir = flux.join("config");
    let interop_dir = flux.join("interop");
    for d in [&core_dir, &plugins_dir, &config_dir, &interop_dir] {
        fs::create_dir_all(d).map_err(|e| format!("fluxloader dir {}: {e}", d.display()))?;
    }
    progress.set_fraction(0.05);

    // 1. Doorstop proxy + config (protected for rollback).
    let winhttp = game_dir.join("winhttp.dll");
    backups.protect(&winhttp).map_err(|e| e.to_string())?;
    fs::write(&winhttp, WINHTTP_DLL).map_err(|e| format!("winhttp.dll: {e}"))?;
    crate::defender::unblock_file(&winhttp);
    let ini_path = game_dir.join("doorstop_config.ini");
    backups.protect(&ini_path).map_err(|e| e.to_string())?;
    fs::write(&ini_path, DOORSTOP_INI).map_err(|e| format!("doorstop_config.ini: {e}"))?;
    println!("[fluxloader] doorstop proxy + config written.");
    progress.set_fraction(0.15);

    // 2. Trimmed CoreCLR runtime. Subset-extract from the BepInEx zip only
    // when missing — never re-download 34MB on every upgrade.
    // v0.2.0: `dotnet/` lives at the GAME ROOT (BepInEx's own layout).
    let dotnet_dir = game_dir.join("dotnet");
    if dotnet_dir.join("coreclr.dll").exists() {
        println!("[fluxloader] CoreCLR runtime already present; skipping.");
    } else if let Some(zip) = bepinex_zip {
        println!("[fluxloader] extracting CoreCLR runtime from BepInEx zip…");
        progress.set_detail("Extracting CoreCLR runtime…".to_string());
        extract_zip_subset(zip, game_dir, DOTNET_SUBSET_PREFIX)?;
    } else {
        return Err("CoreCLR runtime missing and no BepInEx zip provided".to_string());
    }
    // The BepInEx zip also contains doorstop/ + winhttp.dll at its root —
    // those stay in the zip. But guard against a stale BepInEx doorstop
    // proxy shadowing ours: remove BepInEx/winhttp.dll if some old step
    // extracted it.
    let _ = fs::remove_file(game_dir.join("BepInEx").join("winhttp.dll"));
    progress.set_fraction(0.35);

    // 3. Core assemblies (embedded).
    for (name, bytes) in CORE_DLLS {
        let dest = core_dir.join(name);
        backups.protect(&dest).map_err(|e| e.to_string())?;
        fs::write(&dest, bytes).map_err(|e| format!("core/{name}: {e}"))?;
        crate::defender::unblock_file(&dest);
    }
    println!("[fluxloader] {} core assemblies written.", CORE_DLLS.len());
    progress.set_fraction(0.5);

    // 4. Interop assemblies (embedded zip, hash-gated on GameAssembly).
    install_interop(game_dir, &interop_dir, progress)?;
    progress.set_fraction(0.7);

    // 5. Ported plugin (embedded).
    let plugin_path = plugins_dir.join("RecNetPlugin.dll");
    backups.protect(&plugin_path).map_err(|e| e.to_string())?;
    fs::write(&plugin_path, RECNET_PLUGIN_DLL).map_err(|e| format!("RecNetPlugin.dll: {e}"))?;
    crate::defender::unblock_file(&plugin_path);
    // Delete the broken PlayButtonFix.dll from v0.1.48/49/50 if it somehow
    // still exists in the old location.
    let _ = fs::remove_file(game_dir.join("BepInEx").join("plugins").join("PlayButtonFix.dll"));
    println!("[fluxloader] RecNetPlugin.dll (FluxLoader build) written.");
    progress.set_fraction(0.8);

    // 6. Config: migrate the old BepInEx config once, else write fresh.
    install_config(&config_dir, game_dir, ns_host, photon_rt, photon_voice, photon_chat, backups)?;
    progress.set_fraction(0.9);

    // 7. Strict verification — every piece must be in place.
    verify_fluxloader(game_dir)?;

    // 8. Only now: remove the stale BepInEx tree (config + plugins + cache),
    // keeping BepInEx/dotnet (the shared CoreCLR runtime).
    remove_stale_bepinex(game_dir);

    println!("[fluxloader] install verified.");
    progress.set_fraction(1.0);
    Ok(())
}

/// Extract the embedded interop zip, gated on the client's GameAssembly MD5.
fn install_interop(
    game_dir: &Path,
    interop_dir: &Path,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    // Hash gate: the interop was generated from one exact GameAssembly.
    let ga = game_dir.join("GameAssembly.dll");
    let md5 = crate::md5_of_file(&ga)?;
    if md5 != INTEROP_GAMEASSEMBLY_MD5 {
        return Err(format!(
            "GameAssembly.dll MD5 mismatch (got {md5}, need {INTEROP_GAMEASSEMBLY_MD5}): \
             the embedded interop assemblies were generated from a different client build. \
             Refusing to install stale interop."
        ));
    }
    // Skip if the exact expected assembly is already there.
    let probe = interop_dir.join("Assembly-CSharp.dll");
    if probe.exists() {
        println!("[fluxloader] interop assemblies already present; skipping.");
        return Ok(());
    }
    println!("[fluxloader] extracting interop assemblies (283 DLLs)…");
    progress.set_detail("Extracting interop assemblies…".to_string());
    let cursor = std::io::Cursor::new(INTEROP_ZIP);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("interop zip: {e}"))?;
    archive
        .extract(interop_dir)
        .map_err(|e| format!("interop extract: {e}"))?;
    if !probe.exists() {
        return Err("interop extract finished but Assembly-CSharp.dll is missing".to_string());
    }
    println!("[fluxloader] interop assemblies extracted.");
    Ok(())
}

/// Write `FluxLoader/config/net.rec.plugin.cfg`. If an old BepInEx config
/// exists, migrate it once (preserving the user's settings); otherwise
/// write the fresh config with baked values.
fn install_config(
    config_dir: &Path,
    game_dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    backups: &mut crate::transaction::BackupSet,
) -> Result<(), String> {
    let dest = config_dir.join("net.rec.plugin.cfg");
    let legacy = game_dir
        .join("BepInEx")
        .join("config")
        .join("net.rec.plugin.cfg");
    if legacy.exists() && !dest.exists() {
        backups.protect(&dest).map_err(|e| e.to_string())?;
        fs::copy(&legacy, &dest).map_err(|e| format!("config migrate: {e}"))?;
        println!("[fluxloader] migrated existing plugin config from BepInEx.");
        return Ok(());
    }
    if dest.exists() {
        println!("[fluxloader] plugin config already present; keeping.");
        return Ok(());
    }
    backups.protect(&dest).map_err(|e| e.to_string())?;
    let cfg = format!(
        "## Flux Rec — RecNet plugin config (plugin GUID net.rec.plugin, v1.0.0)\n\
         ## FluxLoader build — values baked in at packaging time; override at\n\
         ## install time with --ns-host / --photon-rt / --photon-voice /\n\
         ## --photon-chat or the FLUXREC_NS_HOST / FLUXREC_PHOTON_RT /\n\
         ## FLUXREC_PHOTON_VOICE / FLUXREC_PHOTON_CHAT env vars.\n\
         \n\
         [Server]\n\
         RecNet NameServer Host = {ns_host}\n\
         \n\
         [Photon]\n\
         App Id Realtime = {photon_rt}\n\
         App Id Voice = {photon_voice}\n\
         App Id Chat = {photon_chat}\n\
         \n\
         [Advanced]\n\
         Enabled Advanced Settings = false\n\
         Suppress DUID Mismatch = true\n\
         Debug = false\n\
         \n\
         [Watch]\n\
         Force New Watch UI = true\n\
         \n\
         [Graphics]\n\
         Enable Ultra Graphics = true\n\
         \n\
         [Presence]\n\
         Fix Appear Online To Mapping = false\n\
         \n\
         [Signing]\n\
         Disable Signature Verification = true\n\
         \n\
         [Analytics]\n\
         Disable Telemetry = true\n"
    );
    fs::write(&dest, cfg).map_err(|e| format!("plugin config: {e}"))?;
    println!("[fluxloader] wrote FluxLoader/config/net.rec.plugin.cfg (ns host: {ns_host}).");
    Ok(())
}

/// Every file the game needs to boot through FluxLoader must exist.
fn verify_fluxloader(game_dir: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    let flux = game_dir.join("FluxLoader");
    for rel in [
        "winhttp.dll",
        "doorstop_config.ini",
        "dotnet/coreclr.dll",
        "FluxLoader/core/FluxLoader.Bootstrap.dll",
        "FluxLoader/interop/Assembly-CSharp.dll",
        "FluxLoader/plugins/RecNetPlugin.dll",
        "FluxLoader/config/net.rec.plugin.cfg",
    ] {
        if !game_dir.join(rel).exists() {
            missing.push(rel);
        }
    }
    // All embedded core DLLs must be present, not just the bootstrap.
    for (name, _) in CORE_DLLS {
        if !flux.join("core").join(name).exists() {
            missing.push(name);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("FluxLoader verification failed, missing: {}", missing.join(", ")))
    }
}

/// Remove the stale BepInEx tree after FluxLoader verifies. v0.2.0: the
/// CoreCLR runtime lives at the game root (`dotnet/`), NOT under BepInEx/,
/// so the entire BepInEx directory is stale and goes.
fn remove_stale_bepinex(game_dir: &Path) {
    let bepinex = game_dir.join("BepInEx");
    if !bepinex.exists() {
        return;
    }
    if fs::remove_dir_all(&bepinex).is_ok() {
        println!("[fluxloader] removed stale BepInEx tree.");
    } else {
        eprintln!("[fluxloader] WARNING: could not remove stale BepInEx tree.");
    }
}

/// Extract only zip entries under `prefix` into `dest_dir`.
/// Used to pull `dotnet/` out of the BepInEx zip without the rest.
pub fn extract_zip_subset(
    zip_path: &Path,
    dest_dir: &Path,
    prefix: &str,
) -> Result<(), String> {
    println!("[subset] extracting {prefix}* …");
    let f = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut count = 0u32;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        if !name.starts_with(prefix) {
            continue;
        }
        // Zip-slip guard: never let an entry escape dest_dir.
        let safe = entry
            .enclosed_name()
            .ok_or_else(|| format!("unsafe zip entry: {name}"))?;
        let out = dest_dir.join(safe);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out_f = fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out_f).map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    println!("[subset] extracted {count} files under {prefix}.");
    if count == 0 {
        return Err(format!("zip subset '{prefix}' matched no files"));
    }
    Ok(())
}

/// Quick FluxLoader health check for the launcher (`--play`).
/// Returns Ok when the loader can boot, Err(reason) when it needs repair.
pub fn verify_for_launcher(game_dir: &Path) -> Result<(), String> {
    verify_fluxloader(game_dir)
}

/// Repair a broken FluxLoader install from the launcher: re-runs the
/// install step in place (with rollback). Downloads the BepInEx zip only
/// when the CoreCLR runtime is missing.
pub async fn repair_for_launcher(
    client: &reqwest::Client,
    game_dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    let need_dotnet = !game_dir.join("dotnet").join("coreclr.dll").exists();
    let bepinex_zip: Option<PathBuf> = if need_dotnet {
        Some(crate::fetch_bepinex_zip(client, progress).await?)
    } else {
        None
    };
    let mut backups = crate::transaction::BackupSet::new();
    let res = install_fluxloader(
        game_dir,
        bepinex_zip.as_deref(),
        &mut backups,
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        progress,
    );
    if let Some(zip) = bepinex_zip {
        let _ = fs::remove_file(zip);
    }
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

/// Absolute path helper for tests.
#[allow(dead_code)]
pub fn fluxloader_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("FluxLoader")
}
