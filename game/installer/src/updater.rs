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
//!   * FAST when there is nothing to do: a 24h check cache means repeat
//!     launches skip the network entirely; a live check has an 8s timeout.
//!   * FAIL-SOFT: any network/parse error -> `UpToDate`, the game launches
//!     anyway. An update check must never strand the player.
//!   * No Win32 code: compiles and runs on Linux unchanged.

use futures_util::StreamExt;
use std::path::Path;
use std::time::Duration;

use crate::progress::Progress;

const RELEASES_URL: &str =
    "https://api.github.com/repos/riporipoteam-ctrl/rrflux/releases?per_page=20";
const SETUP_ASSET_NAME: &str = "FluxRec-Setup.exe";
/// Skip the network check if the last one completed more recently than this.
const CHECK_CACHE_SECS: u64 = 24 * 3600;
/// Hard ceiling for the whole update check (keeps launches snappy).
const CHECK_TIMEOUT_SECS: u64 = 8;

fn cache_path(install_dir: &Path) -> std::path::PathBuf {
    install_dir.join(".fluxrec_last_update_check")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_fresh(install_dir: &Path) -> bool {
    let p = cache_path(install_dir);
    let txt = std::fs::read_to_string(p).unwrap_or_default();
    let then: u64 = txt.trim().parse().unwrap_or(0);
    let now = now_secs();
    then > 0 && now >= then && now - then < CHECK_CACHE_SECS
}

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
    /// No newer setup, or the check failed/ was skipped: just launch the game.
    UpToDate,
    /// A newer setup is available at `download_url`.
    Available { version: String, download_url: String },
}

/// Ask GitHub releases for a newer `FluxRec-Setup.exe`.
///
/// Shows "Checking for updates\u{2026}" on the progress handle. Returns
/// [`UpdateDecision::UpToDate`] on any failure — never an error.
pub async fn check_for_updates(install_dir: &Path, progress: &Progress) -> UpdateDecision {
    progress.set_status("Checking for updates\u{2026}", 5);

    if cache_fresh(install_dir) {
        return UpdateDecision::UpToDate;
    }

    let client = match reqwest::Client::builder()
        .user_agent("FluxRec-Launcher")
        .timeout(Duration::from_secs(CHECK_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(_) => return UpdateDecision::UpToDate,
    };

    let decision = check_inner(&client).await;
    // Stamp the cache whenever a check *completed* (even a failed one —
    // hammering a broken network every launch helps nobody).
    stamp_cache(install_dir);
    decision
}

async fn check_inner(client: &reqwest::Client) -> UpdateDecision {
    let resp = match client
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
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
    let body: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return UpdateDecision::UpToDate,
    };
    let releases = match body.as_array() {
        Some(a) => a,
        None => return UpdateDecision::UpToDate,
    };
    let current = current_version();
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
        assert_eq!(parse_version("v10.2"), Some((10, 2, 0)));
        assert_eq!(parse_version("nope"), None);
        assert!((0, 1, 7) > (0, 1, 6));
    }

    #[test]
    fn stale_cache_is_not_fresh() {
        let d = std::env::temp_dir().join("fluxrec-upd-test");
        let _ = std::fs::create_dir_all(&d);
        assert!(!cache_fresh(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
