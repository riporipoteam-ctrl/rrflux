// Flux Rec launcher — automatic BepInEx + plugin maintenance.
//
// Two jobs, both idempotent and fail-soft (a missing plugin must never
// block playing):
//
//   1. BepInEx core: if the preloader is missing (fresh dir, Defender
//      ate it, ...), fetch the pinned bepinex.zip bundle from the mirror
//      and extract it. Otherwise hands off — the bundle does NOT
//      re-download every launch.
//   2. RecNetPlugin.dll: the actual redirect plugin is kept in sync with
//      the mirror on EVERY launch. The mirror carries RecNetPlugin.dll
//      plus a RecNetPlugin.dll.sha256 sidecar (hex hash). The launcher
//      fetches the tiny sidecar, compares it with the installed DLL's
//      hash, and downloads the DLL only when it differs. This is the
//      auto-update path for the plugin — no installer re-run needed.
//
// The pre-0.5.9 launcher used to install a fossil stub plugin
// (BepInEx/plugins/FluxRec.Plugin.dll, guid gg.ripoteam.fluxrec) from the
// mirror on the side. That stub is obsolete and is deleted when found so
// it can't load next to the real plugin.

use crate::util;
use fluxrec_common::download::sha256_of_file;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bump this (and re-upload the bundle) whenever the bundled BepInEx
/// changes.
const BEPINEX_BUNDLE_VERSION: &str = "6.0.0-pre.2";
const BEPINEX_BUNDLE_FILE: &str = "bepinex.zip";
/// sha256 of bepinex.zip on the mirror.
const BEPINEX_BUNDLE_SHA256: &str =
    "3648722ea1a0a042240eec47da4c5b264995ee7f3f95141f62f2749d589fe9c3";

/// Plugin DLL filename on the mirror (served next to manifest.json).
const PLUGIN_FILE: &str = "RecNetPlugin.dll";
/// Sidecar on the mirror holding the expected hex sha256 of PLUGIN_FILE.
const PLUGIN_SHA_FILE: &str = "RecNetPlugin.dll.sha256";
/// Where the plugin lives in the game dir.
const PLUGIN_LOCAL_PATH: &str = "BepInEx/plugins/RecNetPlugin.dll";

/// Fossil stub plugin from the old era — deleted on sight.
const FOSSIL_PLUGIN_PATH: &str = "BepInEx/plugins/FluxRec.Plugin.dll";
/// Marker file from the old era — no longer used.
const OLD_MARKER_FILE: &str = ".fluxrec-bepinex-installed";
/// Staging dir from the old era — cleaned up if left behind.
const OLD_STAGING_DIR: &str = ".fluxrec-bepinex";

fn mirror_base() -> String {
    fluxrec_common::MANIFEST_URL
        .trim_end_matches("manifest.json")
        .to_string()
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// Unzip `zip_path` into `dest`, preserving the archive's internal layout.
/// Paths are sanitized: absolute paths and `..` escapes are rejected.
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("invalid BepInEx bundle: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("unsafe path in BepInEx bundle: {}", entry.name()));
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Install the BepInEx bundle from the mirror. Only called when the
/// preloader is missing. Fail-soft: logs and returns on any problem.
async fn install_bundle_if_missing(game_dir: &Path) {
    let preloader = game_dir.join("BepInEx/core/BepInEx.Preloader.dll");
    if preloader.exists() {
        return;
    }
    util::crash_log("bepinex: preloader missing, fetching bundle from mirror");

    let client = match http_client() {
        Ok(c) => c,
        Err(e) => {
            util::crash_log(&format!("bepinex WARNING: no HTTP client ({e}); skipping bundle install"));
            return;
        }
    };
    let url = format!("{}{}", mirror_base(), BEPINEX_BUNDLE_FILE);
    let bytes = match client
        .get(&url)
        .timeout(Duration::from_secs(600))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(e) => {
                util::crash_log(&format!("bepinex WARNING: bundle read failed ({e})"));
                return;
            }
        },
        Ok(r) => {
            util::crash_log(&format!(
                "bepinex WARNING: bundle not on mirror (HTTP {}); game launches without BepInEx",
                r.status()
            ));
            return;
        }
        Err(e) => {
            util::crash_log(&format!("bepinex WARNING: bundle download failed ({e})"));
            return;
        }
    };

    // Verify the pinned hash before extracting.
    {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&bytes);
        let got = hex::encode(h.finalize());
        if !got.eq_ignore_ascii_case(BEPINEX_BUNDLE_SHA256) {
            util::crash_log("bepinex WARNING: bundle hash mismatch; refusing to install");
            return;
        }
    }

    let staging: PathBuf = game_dir.join(".fluxrec-bundle-tmp");
    let _ = std::fs::create_dir_all(&staging);
    let zip_path = staging.join(BEPINEX_BUNDLE_FILE);
    if let Err(e) = std::fs::write(&zip_path, &bytes) {
        util::crash_log(&format!("bepinex WARNING: couldn't stage bundle ({e})"));
        return;
    }
    let game_dir_owned = game_dir.to_path_buf();
    let extract_res = tokio::task::spawn_blocking(move || extract_zip(&zip_path, &game_dir_owned)).await;
    let _ = std::fs::remove_dir_all(&staging);
    match extract_res {
        Ok(Ok(())) => util::crash_log(&format!(
            "bepinex: bundle {BEPINEX_BUNDLE_VERSION} installed"
        )),
        Ok(Err(e)) => util::crash_log(&format!("bepinex WARNING: bundle extract failed ({e})")),
        Err(e) => util::crash_log(&format!("bepinex WARNING: bundle extract task failed ({e})")),
    }
}

/// Keep BepInEx/plugins/RecNetPlugin.dll in sync with the mirror.
/// Fail-soft: any problem keeps the installed plugin and logs.
async fn ensure_plugin_current(game_dir: &Path) {
    let plugin_path = game_dir.join(PLUGIN_LOCAL_PATH);
    let base = mirror_base();
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => {
            util::crash_log(&format!("plugin WARNING: no HTTP client ({e}); keeping installed plugin"));
            return;
        }
    };

    // The sidecar is one tiny GET per launch.
    let want = match client
        .get(format!("{base}{PLUGIN_SHA_FILE}"))
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t.trim().to_string(),
            Err(_) => {
                util::crash_log("plugin WARNING: couldn't read version marker; keeping installed plugin");
                return;
            }
        },
        _ => {
            util::crash_log("plugin: version marker not on mirror; keeping installed plugin");
            return;
        }
    };
    if want.is_empty() {
        util::crash_log("plugin WARNING: empty version marker; keeping installed plugin");
        return;
    }

    let have = sha256_of_file(&plugin_path).unwrap_or_default();
    if have.eq_ignore_ascii_case(&want) {
        return; // already current — the common case, no download
    }
    util::crash_log("plugin: new version on mirror, downloading RecNetPlugin.dll");

    let bytes = match client
        .get(format!("{base}{PLUGIN_FILE}"))
        .timeout(Duration::from_secs(300))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(e) => {
                util::crash_log(&format!("plugin WARNING: download read failed ({e}); keeping installed plugin"));
                return;
            }
        },
        Ok(r) => {
            util::crash_log(&format!(
                "plugin WARNING: plugin not on mirror (HTTP {}); keeping installed plugin",
                r.status()
            ));
            return;
        }
        Err(e) => {
            util::crash_log(&format!("plugin WARNING: download failed ({e}); keeping installed plugin"));
            return;
        }
    };
    if bytes.len() < 10_000 {
        util::crash_log("plugin WARNING: downloaded plugin looks truncated; keeping installed plugin");
        return;
    }
    {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&bytes);
        let got = hex::encode(h.finalize());
        if !got.eq_ignore_ascii_case(&want) {
            util::crash_log("plugin WARNING: downloaded hash mismatch; keeping installed plugin");
            return;
        }
    }
    if let Some(parent) = plugin_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&plugin_path, &bytes) {
        Ok(()) => util::crash_log("plugin: RecNetPlugin.dll updated from mirror"),
        Err(e) => util::crash_log(&format!("plugin WARNING: couldn't write plugin ({e})")),
    }
}

/// Launcher step 0e: BepInEx core repair + plugin auto-update.
/// Never hard-fails: on any problem it logs and the game still launches.
pub async fn ensure_bepinex(game_dir: &Path) {
    // Drop the fossil stub plugin from the old era so it can't load
    // next to the real one, and clean up the old marker/staging files.
    let fossil = game_dir.join(FOSSIL_PLUGIN_PATH);
    if fossil.exists() {
        match std::fs::remove_file(&fossil) {
            Ok(()) => util::crash_log("bepinex: removed obsolete FluxRec.Plugin.dll stub"),
            Err(e) => util::crash_log(&format!("bepinex WARNING: couldn't remove fossil plugin ({e})")),
        }
    }
    let _ = std::fs::remove_file(game_dir.join(OLD_MARKER_FILE));
    let _ = std::fs::remove_dir_all(game_dir.join(OLD_STAGING_DIR));

    install_bundle_if_missing(game_dir).await;
    ensure_plugin_current(game_dir).await;
}
