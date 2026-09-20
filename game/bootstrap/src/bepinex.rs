// Flux Rec launcher — automatic BepInEx + FluxRec plugin installation.
//
// BepInEx 6 (IL2CPP) gives the 2022 client its runtime patches: TLS trust
// bypass, host redirect to the local backend, EAC stubbing, and optional
// Photon ID overrides (see bepinex-plugin/). The launcher fetches a
// prebuilt bundle from the game mirror and installs it into the game dir
// so the player never does anything by hand:
//
//   game/winhttp.dll + game/doorstop_config.ini   (Doorstop bootstrapper)
//   game/BepInEx/plugins/FluxRec.Plugin.dll       (our plugin)
//   game/BepInEx/config/gg.ripoteam.fluxrec.cfg   (defaults, written once)
//
// Idempotent: a marker file records the installed bundle version and the
// installed plugin's hash; re-install happens only when either changes.
// Fail-soft: if the mirror files aren't uploaded yet (HTTP 404) — or the
// download fails for any other reason — the launcher logs a warning and
// keeps launching the game unblocked. A missing plugin must never block
// playing.

use crate::util;
use fluxrec_common::download::{download_files, sha256_of_file, DownloadOptions, Progress};
use fluxrec_common::manifest::FileEntry;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bump this (and re-upload the bundle) whenever the bundled BepInEx
/// changes. A different value forces a re-install via the marker file.
const BEPINEX_BUNDLE_VERSION: &str = "6.0.0-pre.2";
const BEPINEX_BUNDLE_FILE: &str = "bepinex.zip";
/// sha256 of bepinex.zip on the mirror.
const BEPINEX_BUNDLE_SHA256: &str =
    "3648722ea1a0a042240eec47da4c5b264995ee7f3f95141f62f2749d589fe9c3";

/// Filename of the Flux Rec plugin DLL on the mirror. The uploader decides
/// which game build it targets (November 2022, Showdown, ...); the launcher
/// takes whatever the mirror serves — nothing here is hardcoded to one
/// build.
const PLUGIN_FILE: &str = "FluxRec.Plugin.dll";
/// sha256 of the plugin DLL on the mirror.
const PLUGIN_SHA256: &str =
    "fb814c16497d2eedeb8b4edfd0ad551ad46e8c53241c9b272b1a3869e6f65f95";

/// BepInEx 6 names the config file after the plugin GUID.
const PLUGIN_GUID: &str = "gg.ripoteam.fluxrec";

/// Marker file in the game dir. Contents: bundle version, newline, sha256
/// of the installed plugin DLL.
const MARKER_FILE: &str = ".fluxrec-bepinex-installed";

/// Default plugin config, written ONLY when the file doesn't exist yet.
/// Mirrors the Bind() defaults in bepinex-plugin/src/Plugin.cs.
const DEFAULT_CONFIG: &str = "\
## Flux Rec plugin defaults (written once by the Flux Rec launcher).\n\
## Edit freely — the launcher never overwrites this file.\n\
\n\
[Backend]\n\
\n\
## Base URL of the Flux Rec backend. Requests to https://ns.rec.net are\n\
## rewritten to this host. Keep the https:// scheme.\n\
Host = https://127.0.0.1\n\
\n\
## Replace BestHTTP's certificate verifier with its built-in accept-all\n\
## verifier so the local backend certificate is trusted.\n\
DisableTlsValidation = true\n\
\n\
[EAC]\n\
\n\
## Return an empty EAC challenge response instead of calling the EAC client.\n\
StubChallengeResponse = true\n\
\n\
[Photon]\n\
\n\
## Photon App ID overrides. Empty = keep the build's baked-in values.\n\
AppIdRealtime =\n\
AppIdChat =\n\
AppIdVoice =\n";

fn mirror_base() -> String {
    fluxrec_common::MANIFEST_URL
        .trim_end_matches("manifest.json")
        .to_string()
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

/// Marker value we'd expect if `game_dir` already has a current install.
fn expected_marker(plugin_path: &Path) -> Option<String> {
    let hash = sha256_of_file(plugin_path).ok()?;
    Some(format!("{BEPINEX_BUNDLE_VERSION}\n{hash}"))
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

/// Download the bundle + plugin, install into the game dir, write defaults.
/// Never hard-fails: on any problem it logs and returns so the game can
/// still launch.
pub async fn ensure_bepinex(game_dir: &Path) {
    let plugin_path = game_dir
        .join("BepInEx")
        .join("plugins")
        .join(PLUGIN_FILE);
    let marker_path = game_dir.join(MARKER_FILE);

    // Idempotency: skip when the marker matches the installed state.
    if let Ok(marker) = std::fs::read_to_string(&marker_path) {
        if Some(marker.trim_end().to_string()) == expected_marker(&plugin_path) {
            util::crash_log("bepinex: already installed and current, skipping");
            return;
        }
    }

    let base = mirror_base();
    let entries = vec![
        FileEntry {
            path: BEPINEX_BUNDLE_FILE.to_string(),
            sha256: BEPINEX_BUNDLE_SHA256.to_string(),
            url: format!("{base}{BEPINEX_BUNDLE_FILE}"),
            size: None,
        },
        FileEntry {
            path: PLUGIN_FILE.to_string(),
            sha256: PLUGIN_SHA256.to_string(),
            url: format!("{base}{PLUGIN_FILE}"),
            size: None,
        },
    ];

    let staging: PathBuf = game_dir.join(".fluxrec-bepinex");
    let _ = std::fs::create_dir_all(&staging);
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => {
            util::crash_log(&format!("bepinex WARNING: couldn't build HTTP client ({e}); skipping"));
            return;
        }
    };
    let on_progress = |p: Progress| {
        util::crash_log(&format!(
            "bepinex: downloading {} ({}/{})",
            p.current_file, p.files_done, p.files_total
        ));
    };
    match download_files(&client, &entries, &staging, &DownloadOptions::default(), on_progress).await
    {
        Ok((bytes, files)) => {
            util::crash_log(&format!("bepinex: downloaded {files} file(s), {bytes} bytes"))
        }
        Err(e) => {
            if e.contains("404") {
                util::crash_log(&format!(
                    "bepinex WARNING: bundle/plugin not on the mirror yet (HTTP 404): {e}\n\
                     The game will launch without the plugin until the files are uploaded."
                ));
            } else {
                util::crash_log(&format!(
                    "bepinex WARNING: download failed ({e}); \
                     the game will launch without the plugin for now."
                ));
            }
            let _ = std::fs::remove_dir_all(&staging);
            return;
        }
    }

    // Hash check when a hash is pinned (constants are TODO/empty until the
    // files are uploaded; download_files already enforced them otherwise).
    let bundle_path = staging.join(BEPINEX_BUNDLE_FILE);
    if !BEPINEX_BUNDLE_SHA256.is_empty() {
        match sha256_of_file(&bundle_path) {
            Ok(h) if h.eq_ignore_ascii_case(BEPINEX_BUNDLE_SHA256) => {}
            Ok(h) => {
                util::crash_log(&format!(
                    "bepinex WARNING: bundle hash mismatch (got {h}); refusing to install"
                ));
                let _ = std::fs::remove_dir_all(&staging);
                return;
            }
            Err(e) => {
                util::crash_log(&format!("bepinex WARNING: couldn't hash bundle ({e})"));
                let _ = std::fs::remove_dir_all(&staging);
                return;
            }
        }
    }

    // Extract the bundle into the game dir (winhttp.dll,
    // doorstop_config.ini, BepInEx/ tree). Blocking I/O off the runtime.
    let game_dir_owned = game_dir.to_path_buf();
    let extract_res = tokio::task::spawn_blocking(move || extract_zip(&bundle_path, &game_dir_owned))
        .await
        .map_err(|e| format!("extract task failed: {e}"));
    match extract_res {
        Ok(Ok(())) => util::crash_log("bepinex: bundle extracted into game dir"),
        Ok(Err(e)) | Err(e) => {
            util::crash_log(&format!("bepinex WARNING: extract failed ({e}); skipping"));
            let _ = std::fs::remove_dir_all(&staging);
            return;
        }
    }

    // Copy the plugin DLL into BepInEx/plugins/.
    if let Some(parent) = plugin_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(staging.join(PLUGIN_FILE), &plugin_path) {
        util::crash_log(&format!("bepinex WARNING: couldn't install plugin ({e})"));
        let _ = std::fs::remove_dir_all(&staging);
        return;
    }
    util::crash_log(&format!(
        "bepinex: installed plugin {}",
        plugin_path.to_string_lossy()
    ));

    // Default plugin config — only when the player hasn't made one.
    let config_path = game_dir
        .join("BepInEx")
        .join("config")
        .join(format!("{PLUGIN_GUID}.cfg"));
    if !config_path.exists() {
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&config_path, DEFAULT_CONFIG) {
            Ok(_) => util::crash_log(&format!(
                "bepinex: wrote default plugin config {}",
                config_path.to_string_lossy()
            )),
            Err(e) => util::crash_log(&format!("bepinex WARNING: couldn't write default config ({e})")),
        }
    }

    // Record the install so the next launch skips this whole step.
    if let Some(marker) = expected_marker(&plugin_path) {
        let _ = std::fs::write(&marker_path, marker);
    }
    let _ = std::fs::remove_dir_all(&staging);
    util::crash_log("bepinex: install complete");
}
