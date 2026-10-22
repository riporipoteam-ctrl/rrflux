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
//!   * FAIL-SOFT: network, parse, or local client setup errors -> `CheckFailed`;
//!     the game launches anyway. An update check must never strand the player.
//!   * No Win32 code: compiles and runs on Linux unchanged.

use futures_util::StreamExt;
use std::path::Path;
use std::time::Duration;

use crate::progress::Progress;

const RELEASES_URL: &str =
    "https://api.github.com/repos/riporipoteam-ctrl/rrflux/releases?per_page=20";
const SETUP_ASSET_NAME: &str = "FluxRec-Setup.exe";
const RELEASE_TAG_PREFIX: &str = "ripo-team-launcher-v";
const LEGACY_RELEASE_TAG_PREFIX: &str = "recflare-installer-v";
/// Hard ceiling for the whole update check (keeps launches snappy).
const CHECK_TIMEOUT_SECS: u64 = 5;

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

/// Parse "v0.1.7" / "0.1.7" / "ripo-team-launcher-v1.0.44" into a tuple.
/// Returns None when no numeric version can be found.
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    // Take the trailing numeric dotted run: "ripo-team-launcher-v1.0.44" -> "1.0.44".
    let tail = tag
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or("");
    let mut parts = tail.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

fn current_version() -> (u64, u64, u64) {
    parse_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// What the update check decided.
pub enum UpdateDecision {
    /// No newer setup: just launch the game.
    UpToDate,
    /// A newer setup is available at `download_url`.
    Available {
        version: String,
        download_url: String,
    },
    /// v1.0.5: the check failed (network error). The UI should tell the
    /// user instead of silently saying "Ready to play".
    CheckFailed,
}

/// Ask GitHub releases for a newer `FluxRec-Setup.exe`.
///
/// Shows "Checking for updates\u{2026}" on the progress handle. The check
/// runs live on every call — there is deliberately no skip-cache: stamping
/// "nothing new" (or a transient failure) must never hide a release that
/// appears minutes later. Returns [`UpdateDecision::CheckFailed`] on network
/// failure, [`UpdateDecision::UpToDate`] if the check succeeded and there's
/// nothing newer.
pub async fn check_for_updates(install_dir: &Path, progress: &Progress) -> UpdateDecision {
    progress.set_status("Checking for updates\u{2026}", 5);

    let client = match reqwest::Client::builder()
        .user_agent("FluxRec-Launcher")
        .timeout(Duration::from_secs(CHECK_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(_) => return UpdateDecision::CheckFailed,
    };

    let decision = check_inner(&client).await;
    // Diagnostic record of the completed check. This is not a gate: the
    // next launch checks live again regardless.
    stamp_cache(install_dir);
    decision
}

async fn check_inner(client: &reqwest::Client) -> UpdateDecision {
    // Try the GitHub API first.
    if let Some(decision) = check_via_api(client).await {
        return decision;
    }
    // v1.0.7: Fallback — if api.github.com is blocked but github.com works,
    // scrape the releases HTML page for the latest installer tag.
    check_via_html(client).await
}

async fn check_via_api(client: &reqwest::Client) -> Option<UpdateDecision> {
    let resp = match client
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return None, // Try HTML fallback
    };
    if !resp.status().is_success() {
        return None; // Try HTML fallback
    }
    let text = match resp.text().await {
        Ok(t) => t,
        Err(_) => return None,
    };
    let releases: Vec<serde_json::Value> = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return None,
    };
    Some(find_update(&releases, current_version()))
}

/// v1.0.7: Fallback when api.github.com is unreachable. Fetches the releases
/// HTML page from github.com and extracts the latest Ripo Team Launcher tag.
async fn check_via_html(client: &reqwest::Client) -> UpdateDecision {
    const HTML_URL: &str = "https://github.com/riporipoteam-ctrl/rrflux/releases";
    let resp = match client.get(HTML_URL).send().await {
        Ok(r) => r,
        Err(_) => return UpdateDecision::CheckFailed,
    };
    if !resp.status().is_success() {
        return UpdateDecision::CheckFailed;
    }
    let html = match resp.text().await {
        Ok(t) => t,
        Err(_) => return UpdateDecision::CheckFailed,
    };
    let mut best: Option<((u64, u64, u64), String)> = None;
    // Read the current product tag plus the legacy tag so installed v1.0.43 and older
    // launchers can cross the branding migration without losing automatic updates.
    for prefix in [RELEASE_TAG_PREFIX, LEGACY_RELEASE_TAG_PREFIX] {
        for part in html.split(prefix).skip(1) {
            let ver_str: String = part
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if ver_str.is_empty() {
                continue;
            }
            if let Some(ver) = parse_version(&format!("{}{}", prefix, ver_str)) {
                let tag = format!("{}{}", prefix, ver_str);
                if best.as_ref().map_or(true, |(bv, _)| ver > *bv) {
                    best = Some((ver, tag));
                }
            }
        }
    }
    let current = current_version();
    if let Some((ver, tag)) = best {
        if ver > current {
            let download_url = format!(
                "https://github.com/riporipoteam-ctrl/rrflux/releases/download/{}/{}",
                tag, SETUP_ASSET_NAME
            );
            return UpdateDecision::Available {
                version: tag
                    .strip_prefix(RELEASE_TAG_PREFIX)
                    .or_else(|| tag.strip_prefix(LEGACY_RELEASE_TAG_PREFIX))
                    .unwrap_or(&tag)
                    .to_string(),
                download_url,
            };
        }
    }
    // No newer version found via HTML — but we can't be sure the check
    // succeeded, so return CheckFailed to be honest.
    UpdateDecision::CheckFailed
}

/// Pure update decision over a releases list (newest first, as the GitHub
/// API returns it). Split out from the network fetch so the product-line
/// filter and version comparison are unit-testable.
fn find_update(releases: &[serde_json::Value], current: (u64, u64, u64)) -> UpdateDecision {
    // GitHub's release list is ordered by release creation time, which can
    // differ from semantic version order when a release is rebuilt.  Select
    // the highest valid newer version rather than trusting list order.
    let mut candidate: Option<((u64, u64, u64), String, String)> = None;
    for rel in releases {
        let tag = rel.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
        // Only our own installer line. The v0.5.x bootstrap/NSIS line also
        // ships a FluxRec-Setup.exe asset but is a different product — its
        // higher version number must never pull us across product lines.
        if !tag.starts_with(RELEASE_TAG_PREFIX) && !tag.starts_with(LEGACY_RELEASE_TAG_PREFIX) {
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
        let asset = assets
            .into_iter()
            .flatten()
            .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(SETUP_ASSET_NAME));
        let Some(a) = asset else { continue };
        let Some(url) = a.get("browser_download_url").and_then(|u| u.as_str()) else {
            continue;
        };
        let is_better = match candidate.as_ref() {
            Some((best, _, _)) => ver > *best,
            None => true,
        };
        if is_better {
            candidate = Some((ver, tag.to_string(), url.to_string()));
        }
    }
    match candidate {
        Some((_, version, download_url)) => UpdateDecision::Available { version, download_url },
        None => UpdateDecision::UpToDate,
    }
}

/// Download the update asset to `dest` with progress ("Downloading
/// update…"). Verifies the file is a plausible installer: non-empty, sane
/// size, and a real PE executable (MZ header) — a truncated or HTML error
/// page download can never be spawned. The caller spawns it.
pub async fn download_update(url: &str, dest: &Path, progress: &Progress) -> Result<(), String> {
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
            // v0.2.0: live MB/s + ETA on the detail line.
            progress.set_throughput("Downloading update", done, total);
        }
    }
    drop(file);
    // v0.2.0: structural verification — the old check was "non-empty" only.
    let meta = std::fs::metadata(dest).map_err(|e| e.to_string())?;
    if meta.len() < 5_000_000 {
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "update download: file too small ({} bytes) — truncated?",
            meta.len()
        ));
    }
    {
        use std::io::Read as _;
        let mut f = std::fs::File::open(dest).map_err(|e| e.to_string())?;
        let mut magic = [0u8; 2];
        f.read_exact(&mut magic).map_err(|e| e.to_string())?;
        if magic != [b'M', b'Z'] {
            let _ = std::fs::remove_file(dest);
            return Err("update download: not a Windows executable (bad MZ header)".to_string());
        }
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
        assert_eq!(parse_version("ripo-team-launcher-v1.0.44"), Some((1, 0, 44)));
        // Existing installations must still understand releases from before the rename.
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
            rel("ripo-team-launcher-v0.1.11", Some("https://dl/0.1.11.exe")),
            rel("recflare-installer-v0.1.10", Some("https://dl/0.1.10.exe")),
        ];
        match find_update(&releases, (0, 1, 10)) {
            UpdateDecision::Available {
                version,
                download_url,
            } => {
                assert_eq!(version, "ripo-team-launcher-v0.1.11");
                assert_eq!(download_url, "https://dl/0.1.11.exe");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
            UpdateDecision::CheckFailed => panic!("update check unexpectedly failed"),
        }
    }

    #[test]
    fn find_update_uses_highest_version_when_release_order_is_not_semver_order() {
        // GitHub can return a rebuilt older release before a newer tag.
        let releases = vec![
            rel("ripo-team-launcher-v1.0.47", Some("https://dl/1.0.47.exe")),
            rel("ripo-team-launcher-v1.0.49", Some("https://dl/1.0.49.exe")),
            rel("ripo-team-launcher-v1.0.48", Some("https://dl/1.0.48.exe")),
        ];
        match find_update(&releases, (1, 0, 46)) {
            UpdateDecision::Available {
                version,
                download_url,
            } => {
                assert_eq!(version, "ripo-team-launcher-v1.0.49");
                assert_eq!(download_url, "https://dl/1.0.49.exe");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
            UpdateDecision::CheckFailed => panic!("update check unexpectedly failed"),
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
        // but must never pull the Ripo Team Launcher line across products — even
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
            rel("ripo-team-launcher-v0.1.12", None),
            rel("ripo-team-launcher-v0.1.11", Some("https://dl/0.1.11.exe")),
        ];
        match find_update(&releases, (0, 1, 10)) {
            UpdateDecision::Available { version, .. } => {
                assert_eq!(version, "ripo-team-launcher-v0.1.11");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
            UpdateDecision::CheckFailed => panic!("update check unexpectedly failed"),
        }
    }

    #[test]
    fn find_update_ignores_unparseable_tags() {
        let releases = vec![
            rel("ripo-team-launcher-latest", Some("https://dl/x.exe")),
            rel("ripo-team-launcher-v0.1.11", Some("https://dl/0.1.11.exe")),
        ];
        match find_update(&releases, (0, 1, 10)) {
            UpdateDecision::Available { version, .. } => {
                assert_eq!(version, "ripo-team-launcher-v0.1.11");
            }
            UpdateDecision::UpToDate => panic!("expected an update"),
            UpdateDecision::CheckFailed => panic!("update check unexpectedly failed"),
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
