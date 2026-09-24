//! Automatic updates for Flux Rec.
//!
//! When the player launches the game, the launcher shows "Checking for
//! updates\u{2026}", asks the GitHub releases API (where new setups are
//! posted) for the newest release carrying a `FluxRec-Setup.exe` asset, and
//! compares it with the running version. If a newer setup exists it is
//! downloaded with progress and spawned with `--updated`; the fresh setup
//! installs over the game dir and then launches the game itself.
//!
//! Design rules:
//!   * The check runs on EVERY launch (no skip-cache): a release published
//!     minutes after the last launch must be offered on the next one, and
//!     the player was promised a visible check each time. A live check has
//!     a 5s timeout so a dead network never stalls a launch.
//!   * FAIL-SOFT: any network/parse error -> `UpToDate`, the game launches
//!     anyway. An update check must never strand the player.
//!   * No Win32 code: compiles and runs on Linux unchanged.

use futures_util::StreamExt;
use std::path::Path;
use std::time::Duration;

use crate::progress::Progress;

const VERSION_URL: &str = "https://api.ripo-ripoteam.workers.dev/api/installer/version";
const SETUP_ASSET_NAME: &str = "FluxRec-Setup.exe";
/// Hard ceiling for the whole update check (keeps launches snappy).
const CHECK_TIMEOUT_SECS: u64 = 15;

/// Diagnostic record of the last completed check. Written after every
/// check; never read to skip one — every launch checks live.
fn cache_path(install_dir: &Path) -> std::path::PathBuf {
    install_dir.join(".fluxrec_last_update_check")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Record when the last check completed (diagnostic only; never gates a
/// future check).
fn stamp_cache(install_dir: &Path) {
    let _ = std::fs::write(cache_path(install_dir), now_secs().to_string());
}

/// Parse "v0.1.7" / "0.1.7" / "recflare-installer-v0.1.1" into (0,1,7).
/// Returns None when no numeric version can be found.
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    // Take the trailing numeric dotted run: "recflare-installer-v0.1.1" -> "0.1.1".
    let tail = tag
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or("");
    let mut parts = tail.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

fn current_version() -> (u64, u64, u64) {
    parse_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// What the update check decided.
pub enum UpdateDecision {
    /// No newer setup, or the check failed: just launch the game.
    UpToDate,
    /// A newer setup is available at `download_url`.
    Available { version: String, download_url: String },
}

/// Ask GitHub releases for a newer `FluxRec-Setup.exe`.
///
/// Shows "Checking for updates\u{2026}" on the progress handle. The check
/// runs live on every call — there is deliberately no skip-cache: stamping
/// "nothing new" (or a transient failure) must never hide a release that
/// appears minutes later. Returns [`UpdateDecision::UpToDate`] on any
/// failure — never an error.
pub async fn check_for_updates(install_dir: &Path, progress: &Progress) -> UpdateDecision {
    progress.set_status("Checking for updates\u{2026}", 5);

    let client = match reqwest::Client::builder()
        .user_agent("FluxRec-Launcher")
        .timeout(Duration::from_secs(CHECK_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(_) => return UpdateDecision::UpToDate,
    };

    let decision = check_inner(&client).await;
    // Diagnostic record of the completed check. This is not a gate: the
    // next launch checks live again regardless.
    stamp_cache(install_dir);
    decision
}

async fn check_inner(client: &reqwest::Client) -> UpdateDecision {
    let resp = match client.get(VERSION_URL).send().await {
        Ok(r) => r,
        Err(_) => return UpdateDecision::UpToDate,
    };
    if !resp.status().is_success() {
        return UpdateDecision::UpToDate;
    }
    let text = match resp.text().await {
        Ok(t) => t,
        Err(_) => return UpdateDecision::UpToDate,
    };
    // New format: {"latest": "0.1.13", "download_url": "https://..."}
    // Served from our own domain to avoid GitHub API rate limits.
    let info: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return UpdateDecision::UpToDate,
    };
    let latest = info.get("latest").and_then(|v| v.as_str()).unwrap_or("");
    let download_url = info
        .get("download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if latest.is_empty() || download_url.is_empty() {
        return UpdateDecision::UpToDate;
    }
    let ver = match parse_version(&format!("recflare-installer-v{}", latest)) {
        Some(v) => v,
        None => return UpdateDecision::UpToDate,
    };
    if ver <= current_version() {
        return UpdateDecision::UpToDate;
    }
    UpdateDecision::Available {
        version: latest.to_string(),
        download_url: download_url.to_string(),
    }
}

/// Pure update decision over a releases list (newest first, as the GitHub
/// API returns it). Split out from the network fetch so the product-line
/// filter and version comparison are unit-testable.
fn find_update(releases: &[serde_json::Value], current: (u64, u64, u64)) -> UpdateDecision {
    // The API returns newest first: the first release carrying our asset
    // that is newer than us wins.
    for rel in releases {
        let tag = rel.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
        // Only our own installer line. The v0.5.x bootstrap/NSIS line also
        // ships a FluxRec-Setup.exe asset but is a different product — its
        // higher version number must never pull us across product lines.
        if !tag.starts_with("recflare-installer-") {
            continue;
        }
        let ver = match parse_version(tag) {
            Some(v) => v,
            None => continue,
        };
        if ver <= current {
            continue;
        }
        let assets = rel.get("assets").and_then(|v| v.as_array());
        let asset = assets.into_iter().flatten().find(|a| {
            a.get("name").and_then(|n| n.as_str()) == Some(SETUP_ASSET_NAME)
        });
        if let Some(a) = asset {
            if let Some(url) = a
                .get("browser_download_url")
                .and_then(|u| u.as_str())
            {
                return UpdateDecision::Available {
                    version: tag.to_string(),
                    download_url: url.to_string(),
                };
            }
        }
    }
    UpdateDecision::UpToDate
}

/// Download the update asset to `dest` with progress ("Downloading
/// update\u{2026}"). Verifies a non-empty file; the caller spawns it.
pub async fn download_update(
    url: &str,
    dest: &Path,
    progress: &Progress,
) -> Result<(), String> {
    progress.set_status("Downloading update\u{2026}", 8);
    let client = reqwest::Client::builder()
        .user_agent("FluxRec-Launcher")
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("update download: HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        use tokio::io::AsyncWriteExt as _;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        if total > 0 {
            let pct = 8 + (done as f64 / total as f64 * 72.0) as u8;
            progress.set_status("Downloading update\u{2026}", pct.min(80));
        }
    }
    drop(file);
    let meta = std::fs::metadata(dest).map_err(|e| e.to_string())?;
    if meta.len() == 0 {
        let _ = std::fs::remove_file(dest);
        return Err("update download: empty file".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v0.1.7"), Some((0, 1, 7)));
        assert_eq!(parse_version("0.1.6"), Some((0, 1, 6)));
        assert_eq!(parse_version("recflare-installer-v0.1.1"), Some((0, 1, 1)));
        assert_eq!(
            parse_version("recflare-installer-v0.1.11"),
            Some((0, 1, 11))
        );
        assert_eq!(parse_version("v10.2"), Some((10, 2, 0)));
        assert_eq!(parse_version("nope"), None);
        assert!((0, 1, 7) > (0, 1, 6));
        assert!((0, 1, 11) > (0, 1, 10));
    }

    fn rel(tag: &str, asset_url: Option<&str>) -> serde_json::Value {
        let assets = match asset_url {
            Some(url) => serde_json::json!([{
                "name": "FluxRec-Setup.exe",
                "browser_download_url": url,
            }]),
            None => serde_json::json!([]),
        };
        serde_json::json!({ "tag_name": tag, "assets": assets })
    }

    #[test]
    fn find_update_picks_newest_newer_release_with_asset() {
        // Newest-first, as the API returns them.
        let releases = vec![
            rel("recflare-installer-v0.1.11", Some("https://dl/0.1.11.exe")),
            rel("recflare-installer-v0.1.10", Some("https://dl/0.1.10.exe")),
        ];
        match find_update(&releases, (0, 1, 10)) {
            UpdateDecision::Available { version, download_url } => {
                assert_eq!(version, "recflare-installer-v0.1.11");
                assert_eq!(download_url, "https://dl/0.1.11.exe");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
        }
    }

    #[test]
    fn find_update_ignores_same_or_older_versions() {
        let releases = vec![
            rel("recflare-installer-v0.1.10", Some("https://dl/0.1.10.exe")),
            rel("recflare-installer-v0.1.9", Some("https://dl/0.1.9.exe")),
        ];
        assert!(matches!(
            find_update(&releases, (0, 1, 10)),
            UpdateDecision::UpToDate
        ));
    }

    #[test]
    fn find_update_ignores_other_product_lines() {
        // The v0.5.x bootstrap line also ships a FluxRec-Setup.exe asset
        // but must never pull the recflare line across products — even
        // though 0.5.8 > 0.1.10 numerically.
        let releases = vec![
            rel("v0.5.8", Some("https://dl/bootstrap.exe")),
            rel("recflare-installer-v0.1.10", Some("https://dl/0.1.10.exe")),
        ];
        assert!(matches!(
            find_update(&releases, (0, 1, 10)),
            UpdateDecision::UpToDate
        ));
    }

    #[test]
    fn find_update_skips_releases_without_usable_asset() {
        // Newer tag but no asset / no download URL: keep scanning, don't
        // offer a broken update.
        let releases = vec![
            rel("recflare-installer-v0.1.12", None),
            rel("recflare-installer-v0.1.11", Some("https://dl/0.1.11.exe")),
        ];
        match find_update(&releases, (0, 1, 10)) {
            UpdateDecision::Available { version, .. } => {
                assert_eq!(version, "recflare-installer-v0.1.11");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
        }
    }

    #[test]
    fn find_update_ignores_unparseable_tags() {
        let releases = vec![
            rel("recflare-installer-latest", Some("https://dl/x.exe")),
            rel("recflare-installer-v0.1.11", Some("https://dl/0.1.11.exe")),
        ];
        match find_update(&releases, (0, 1, 10)) {
            UpdateDecision::Available { version, .. } => {
                assert_eq!(version, "recflare-installer-v0.1.11");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
        }
    }

    #[test]
    fn last_check_record_is_written_and_parseable() {
        let d = std::env::temp_dir().join("fluxrec-upd-test");
        let _ = std::fs::create_dir_all(&d);
        stamp_cache(&d);
        let txt = std::fs::read_to_string(cache_path(&d)).unwrap();
        let then: u64 = txt.trim().parse().expect("timestamp must parse");
        assert!(then > 0 && then <= now_secs());
        let _ = std::fs::remove_dir_all(&d);
    }
}
