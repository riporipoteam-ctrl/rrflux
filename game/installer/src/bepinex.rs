//! BepInEx installation — the proven loader (v0.2.6 restores it).
//!
//! Layout inside the game directory (BepInEx 6.0.0-pre.2, Unity IL2CPP win-x64):
//!
//! ```text
//! <game>/
//!   winhttp.dll                  Doorstop proxy (from the BepInEx zip)
//!   doorstop_config.ini          from the BepInEx zip (targets BepInEx preloader)
//!   dotnet/                      trimmed CoreCLR 6.0.7 (from the BepInEx zip, game root)
//!   BepInEx/
//!     core/                      preloader + runtime
//!     plugins/RecNetPlugin.dll   RecFlare redirect plugin (embedded BepInEx build)
//!     config/net.rec.plugin.cfg  generated (ns host + Photon App IDs)
//! ```
//!
//! v0.2.6: FluxLoader is removed entirely — its bootstrap never ran on real
//! Windows (no fluxloader.log in any proof run), so the game could never
//! reach the RecFlare backend. BepInEx is the proven loader.

use std::fs;
use std::path::{Path, PathBuf};

/// RecFlare redirect plugin, BepInEx build (embedded at compile time).
/// This is the real RecFlare plugin — the game cannot reach the private
/// backend without it.
pub(crate) const RECNET_PLUGIN_DLL: &[u8] = include_bytes!("../assets/RecNetPlugin.dll");
/// RRUI fix plugin — forces the new Watch UI (Store crash fix).
/// Loaded alongside the stock RecNet plugin.
pub(crate) const RRUI_FIX_DLL: &[u8] = include_bytes!("../assets/FluxRec.RruiFix.dll");

/// Install (or refresh) BepInEx into `game_dir`.
///
/// * `bepinex_zip` — path to the downloaded BepInEx 6.0.0-pre.2 zip.
/// * `backups` — every file overwritten is protected for rollback.
/// * `progress` — stage is expected to be "Installing BepInEx…".
///
/// Hard-fails on any error: a half-installed loader is worse than none.
pub fn install_bepinex(
    game_dir: &Path,
    bepinex_zip: &Path,
    backups: &mut crate::transaction::BackupSet,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    let bepinex_dir = game_dir.join("BepInEx");
    let plugins_dir = bepinex_dir.join("plugins");
    let config_dir = bepinex_dir.join("config");
    for d in [&bepinex_dir, &plugins_dir, &config_dir] {
        fs::create_dir_all(d).map_err(|e| format!("bepinex dir {}: {e}", d.display()))?;
    }
    progress.set_fraction(0.1);

    // 1. Extract the BepInEx zip (winhttp.dll, doorstop_config.ini,
    // BepInEx/, dotnet/). Protect the Doorstop files for rollback since a
    // v0.2.x install left FluxLoader's versions behind.
    for rel in ["winhttp.dll", "doorstop_config.ini"] {
        let p = game_dir.join(rel);
        backups.protect(&p).map_err(|e| e.to_string())?;
    }
    progress.set_detail("Extracting BepInEx…".to_string());
    crate::extract_zip(bepinex_zip, game_dir, "bepinex")?;
    crate::defender::unblock_file(&game_dir.join("winhttp.dll"));
    println!("[bepinex] BepInEx 6.0.0-pre.2 extracted.");
    progress.set_fraction(0.5);

    // 2. RecFlare redirect plugin (embedded BepInEx build).
    let plugin_path = plugins_dir.join("RecNetPlugin.dll");
    backups.protect(&plugin_path).map_err(|e| e.to_string())?;
    fs::write(&plugin_path, RECNET_PLUGIN_DLL).map_err(|e| format!("RecNetPlugin.dll: {e}"))?;
    let rrui_path = plugins_dir.join("FluxRec.RruiFix.dll");
    fs::write(&rrui_path, RRUI_FIX_DLL).map_err(|e| format!("FluxRec.RruiFix.dll: {e}"))?;
    println!("[bepinex] wrote FluxRec.RruiFix.dll (RRUI Store fix).");
    crate::defender::unblock_file(&plugin_path);
    // Delete the broken PlayButtonFix.dll from v0.1.48/49/50 if it somehow
    // still exists in the old location.
    let _ = fs::remove_file(plugins_dir.join("PlayButtonFix.dll"));
    println!("[bepinex] RecNetPlugin.dll (BepInEx build) written.");
    progress.set_fraction(0.7);

    // 3. Plugin config (ns host + Photon IDs baked at packaging time).
    install_config(
        &config_dir,
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        backups,
    )?;
    progress.set_fraction(0.85);

    // 4. Keep the BepInEx console window off at game launch. BepInEx
    // 6.0.0-pre.2 defaults Logging.Console/Enabled to true and AllocConsoles
    // its own window inside the game process — this is the real kill switch.
    crate::stealth::ensure_bepinex_console_disabled(game_dir);

    // 5. Strict verification — every piece must be in place.
    verify_bepinex(game_dir)?;

    // 6. Only now: remove the stale FluxLoader tree (left by v0.2.x).
    remove_stale_fluxloader(game_dir);

    println!("[bepinex] install verified.");
    progress.set_fraction(1.0);
    Ok(())
}

/// Write `BepInEx/config/net.rec.plugin.cfg` with baked values.
/// Preserves an existing config (upgrade path keeps the user's settings).
fn install_config(
    config_dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    backups: &mut crate::transaction::BackupSet,
) -> Result<(), String> {
    let dest = config_dir.join("net.rec.plugin.cfg");
    if dest.exists() {
        println!("[bepinex] plugin config already present; keeping.");
        return Ok(());
    }
    backups.protect(&dest).map_err(|e| e.to_string())?;
    let cfg = format!(
        "## Flux Rec — RecNet plugin config (plugin GUID net.rec.plugin, v1.0.0)\n\
         ## Values baked in at packaging time; override at install time with\n\
         ## --ns-host / --photon-rt / --photon-voice / --photon-chat or the\n\
         ## FLUXREC_NS_HOST / FLUXREC_PHOTON_RT / FLUXREC_PHOTON_VOICE /\n\
         ## FLUXREC_PHOTON_CHAT env vars.\n\
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
    println!("[bepinex] wrote BepInEx/config/net.rec.plugin.cfg (ns host: {ns_host}).");
    Ok(())
}

/// Every file the game needs to boot through BepInEx must exist.
fn verify_bepinex(game_dir: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    // BepInEx core: preloader is what doorstop_config.ini targets.
    let bepinex = game_dir.join("BepInEx");
    for rel in [
        "winhttp.dll",
        "doorstop_config.ini",
        "BepInEx/core/BepInEx.Unity.IL2CPP.dll",
        "BepInEx/core/BepInEx.Core.dll",
        "BepInEx/plugins/RecNetPlugin.dll",
        "BepInEx/config/net.rec.plugin.cfg",
    ] {
        if !game_dir.join(rel).exists() {
            missing.push(rel);
        }
    }
    // The preloader DLL name varies by BepInEx build; accept either.
    if !bepinex
        .join("core")
        .join("BepInEx.Unity.IL2CPP.dll")
        .exists()
        && !bepinex.join("core").join("BepInEx.Preloader.dll").exists()
    {
        missing.push("BepInEx preloader (BepInEx.Unity.IL2CPP.dll or BepInEx.Preloader.dll)");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "BepInEx verification failed, missing: {}",
            missing.join(", ")
        ))
    }
}

/// Remove the stale FluxLoader tree left by v0.2.x after BepInEx verifies.
fn remove_stale_fluxloader(game_dir: &Path) {
    let flux = game_dir.join("FluxLoader");
    if flux.exists() {
        if fs::remove_dir_all(&flux).is_ok() {
            println!("[bepinex] removed stale FluxLoader tree.");
        } else {
            eprintln!("[bepinex] WARNING: could not remove stale FluxLoader tree.");
        }
    }
    // FluxLoader's interop dir is gone with the tree; nothing else to clean.
    // NOTE: dotnet/ at the game root is shared — BepInEx uses the same
    // layout, so it stays.
}

/// Quick BepInEx health check for the launcher (`--play`).
/// Returns Ok when the loader can boot, Err(reason) when it needs repair.
pub fn verify_for_launcher(game_dir: &Path) -> Result<(), String> {
    verify_bepinex(game_dir)
}

/// Repair a broken BepInEx install from the launcher: re-runs the install
/// step in place (with rollback). Downloads the BepInEx zip fresh.
pub async fn repair_for_launcher(
    client: &reqwest::Client,
    game_dir: &Path,
    ns_host: &str,
    photon_rt: &str,
    photon_voice: &str,
    photon_chat: &str,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    let bepinex_zip = crate::fetch_bepinex_zip(client, progress).await?;
    let mut backups = crate::transaction::BackupSet::new();
    let res = install_bepinex(
        game_dir,
        &bepinex_zip,
        &mut backups,
        ns_host,
        photon_rt,
        photon_voice,
        photon_chat,
        progress,
    );
    let _ = fs::remove_file(&bepinex_zip);
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
pub fn bepinex_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("BepInEx")
}

/// HF mirror base for plugin auto-updates.
const PLUGIN_MIRROR_BASE: &str = "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/";
const PLUGIN_FILE: &str = "RecNetPlugin.dll";
const PLUGIN_SHA_FILE: &str = "RecNetPlugin.dll.sha256";

/// Compute SHA-256 hex of a file. Empty string on any error.
fn sha256_of_file(path: &Path) -> String {
    use sha2::Digest as _;
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    let mut h = sha2::Sha256::new();
    h.update(&bytes);
    hex::encode(h.finalize())
}

/// Remove the temporary `127.0.0.1 huggingface.co` hosts entry added during
/// Store debugging. The plugin mirror lives on HuggingFace, so the block
/// breaks plugin auto-updates. Fail-soft: if we can't write (no admin),
/// the mirror sync below just fails soft and the game still launches.
fn remove_hf_hosts_block() {
    #[cfg(windows)]
    {
        let hosts_path = std::path::Path::new(
            r"C:\Windows\System32\drivers\etc\hosts",
        );
        let content = match fs::read_to_string(hosts_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        // Drop lines that block huggingface.co via loopback.
        let filtered: Vec<&str> = content
            .lines()
            .filter(|line| {
                let t = line.trim();
                // Skip comments and empty lines (keep them).
                if t.is_empty() || t.starts_with('#') {
                    return true;
                }
                // Remove if it maps huggingface.co to loopback.
                let lower = t.to_lowercase();
                !(lower.contains("huggingface.co")
                    && (lower.starts_with("127.")
                        || lower.starts_with("::1")
                        || lower.starts_with("0.0.0.0")))
            })
            .collect();
        if filtered.len() == content.lines().count() {
            return; // no block found — nothing to do
        }
        let new_content = filtered.join("\r\n") + "\r\n";
        // Backup first, then write. Fail-soft on any error.
        let backup = hosts_path.with_extension("fluxrec.bak");
        let _ = fs::copy(hosts_path, &backup);
        if fs::write(hosts_path, new_content).is_ok() {
            println!("[bepinex] removed huggingface.co hosts block");
        }
    }
    #[cfg(not(windows))]
    {
        // Non-Windows: nothing to do.
    }
}

/// Keep BepInEx/plugins/RecNetPlugin.dll in sync with the HF mirror.
/// Called on every --play launch. Fail-soft: any problem keeps the installed
/// plugin and the game still launches.
pub async fn sync_plugin_from_mirror(game_dir: &Path, progress: &crate::progress::Progress) {
    // Remove the temporary huggingface.co hosts block (added during Store
    // debugging). The plugin mirror lives on HF, so the block breaks updates.
    remove_hf_hosts_block();

    let plugins_dir = game_dir.join("BepInEx").join("plugins");
    let plugin_path = plugins_dir.join(PLUGIN_FILE);

    // Remove the fossil stub from the old era.
    let _ = fs::remove_file(plugins_dir.join("FluxRec.Plugin.dll"));

    let client = match reqwest::Client::builder()
        .user_agent("FluxRec-Setup/0.2.22")
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(_) => return, // offline — keep installed
    };

    // Fetch the tiny sidecar (one GET per launch).
    let want = match client
        .get(format!("{PLUGIN_MIRROR_BASE}{PLUGIN_SHA_FILE}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t.trim().to_string(),
            Err(_) => return,
        },
        _ => return, // mirror unreachable — keep installed
    };
    if want.is_empty() {
        return;
    }

    let have = sha256_of_file(&plugin_path);
    if have.eq_ignore_ascii_case(&want) {
        return; // already current — the common case
    }

    println!("[bepinex] plugin out of date, downloading from mirror…");
    progress.set_status("Updating plugin…", 10);

    let bytes = match client
        .get(format!("{PLUGIN_MIRROR_BASE}{PLUGIN_FILE}"))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(_) => return,
        },
        _ => return,
    };
    if bytes.len() < 10_000 {
        return; // truncated — keep installed
    }
    // Verify hash before writing.
    {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&bytes);
        if !hex::encode(h.finalize()).eq_ignore_ascii_case(&want) {
            return; // hash mismatch — keep installed
        }
    }
    let _ = fs::create_dir_all(&plugins_dir);
    if fs::write(&plugin_path, &bytes).is_ok() {
        println!("[bepinex] RecNetPlugin.dll updated from mirror.");
        crate::defender::unblock_file(&plugin_path);
    }
}
