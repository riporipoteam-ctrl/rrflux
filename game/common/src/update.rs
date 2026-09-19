// Update orchestration used by the bootstrapper (and the installer
// downloader, which is just "update from empty").
//
// Two layers:
//   1. Bootstrap self-update: version.json advertises the newest bootstrapper.
//      If we're older, the caller downloads the new exe and hands it to
//      fluxrec-selfupdate, which swaps it in after we exit.
//   2. Game files: the remote manifest is diffed against the local copy;
//      only changed/new files download (in parallel, resumable), removed
//      files are deleted.

use crate::download::{download_files, DownloadOptions, Progress};
use crate::manifest::{self, Diff, FileEntry, Manifest};
use crate::progress::ProgressWindow;
use crate::sleep::PreventSleep;
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERSION_URL: &str =
    "https://raw.githubusercontent.com/riporipoteam-ctrl/rrflux/main/game/version.json";

#[derive(serde::Deserialize)]
struct VersionInfo {
    bootstrap_version: String,
    bootstrap_url: String,
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

/// Compare dotted versions ("0.3.0"). Returns Ordering of a vs b.
fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<u64> = a.split('.').filter_map(|p| p.parse().ok()).collect();
    let pb: Vec<u64> = b.split('.').filter_map(|p| p.parse().ok()).collect();
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            o => return o,
        }
    }
    std::cmp::Ordering::Equal
}

pub enum BootstrapUpdate {
    UpToDate,
    /// A newer bootstrapper was downloaded to this path. The caller should
    /// spawn fluxrec-selfupdate with it and exit.
    Available { new_exe: PathBuf, url: String },
}

/// Check whether a newer bootstrapper exists. Fail-soft: any network or
/// parse problem means UpToDate (offline players can still play).
pub async fn check_bootstrap_update(
    current_version: &str,
    dest: &Path,
) -> Result<BootstrapUpdate, String> {
    let client = http_client()?;
    let text = client
        .get(VERSION_URL)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| format!("version check failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("version check failed: {e}"))?;
    let info: VersionInfo =
        serde_json::from_str(&text).map_err(|e| format!("version check failed: {e}"))?;
    if cmp_versions(&info.bootstrap_version, current_version) != std::cmp::Ordering::Greater {
        return Ok(BootstrapUpdate::UpToDate);
    }
    // Download the new exe next to the current one.
    let bytes = client
        .get(&info.bootstrap_url)
        .send()
        .await
        .map_err(|e| format!("bootstrap download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("bootstrap download failed: {e}"))?;
    if bytes.len() < 100_000 {
        return Err("downloaded bootstrapper looks truncated".into());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())?;
    Ok(BootstrapUpdate::Available {
        new_exe: dest.to_path_buf(),
        url: info.bootstrap_url,
    })
}

pub struct GameUpdateOutcome {
    /// Number of files downloaded (0 = already up to date).
    pub downloaded: usize,
    pub deleted: usize,
}

/// Bring the game dir up to date with the remote manifest.
/// Shows `progress` only while files actually need downloading.
/// Returns how many files changed. Errors are fatal for the caller.
pub async fn update_game_files(
    manifest_url: &str,
    game_dir: &Path,
    state_dir: &Path,
    window_title: &str,
) -> Result<GameUpdateOutcome, String> {
    let _no_sleep = PreventSleep::new();
    let client = http_client()?;
    std::fs::create_dir_all(game_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;

    // What do we have locally?
    let local = manifest::load_local(state_dir);
    let local_manifest: Option<Manifest> = local
        .as_ref()
        .and_then(|(bytes, _)| manifest::parse(bytes).ok());

    // Fetch the remote manifest (conditional request when we have an ETag).
    let mut req = client.get(manifest_url);
    if let Some((_, Some(etag))) = &local {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            // Offline but we have game files: carry on; the game itself
            // will report connection problems at sign-in.
            if local_manifest.is_some() {
                return Ok(GameUpdateOutcome {
                    downloaded: 0,
                    deleted: 0,
                });
            }
            return Err(format!("couldn't fetch the game manifest: {e}"));
        }
    };

    let (remote_bytes, etag) = if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        let (bytes, etag) = local.expect("304 with no local manifest");
        (bytes, etag)
    } else {
        if !resp.status().is_success() {
            return Err(format!("manifest request failed: HTTP {}", resp.status()));
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("couldn't read the game manifest: {e}"))?
            .to_vec();
        (bytes, etag)
    };

    let remote: Manifest = manifest::parse(&remote_bytes)?;
    let Diff {
        to_download: mut to_download,
        to_delete,
    } = manifest::diff(local_manifest.as_ref(), &remote);

    // The manifest diff alone can't see local damage (interrupted runs,
    // deleted files): anything in the manifest that's missing on disk or
    // has the wrong size needs fetching too. Unchanged files that are
    // present with the right size are trusted without re-hashing.
    {
        use std::collections::HashSet;
        let wanted: HashSet<&str> = to_download.iter().map(|f| f.path.as_str()).collect();
        let mut extra: Vec<FileEntry> = Vec::new();
        for f in &remote.files {
            if wanted.contains(f.path.as_str()) {
                continue;
            }
            let dest = game_dir.join(&f.path);
            let bad = match std::fs::metadata(&dest) {
                Err(_) => true,
                Ok(md) => f.size.map(|s| md.len() != s).unwrap_or(false),
            };
            if bad {
                extra.push(f.clone());
            }
        }
        to_download.extend(extra);
    }

    let mut outcome = GameUpdateOutcome {
        downloaded: 0,
        deleted: 0,
    };

    if !to_download.is_empty() {
        let window = ProgressWindow::new(window_title);
        let opts = DownloadOptions::default();
        let (bytes, fetched) = download_files(&client, &to_download, game_dir, &opts, move |p: Progress| {
            let frac = if p.files_total > 0 {
                p.files_done as f64 / p.files_total as f64
            } else {
                1.0
            };
            // While nothing has been fetched yet we're still verifying the
            // files already on disk — say so instead of looking frozen.
            let verb = if p.bytes_done == 0 {
                "Checking game files"
            } else {
                "Downloading game files"
            };
            let label = match p.bytes_total {
                Some(bt) if bt > 0 => format!(
                    "{verb}… {}/{} files ({:.1}/{:.1} MB)\n{}",
                    p.files_done,
                    p.files_total,
                    p.bytes_done as f64 / 1048576.0,
                    bt as f64 / 1048576.0,
                    p.current_file
                ),
                _ => format!(
                    "{verb}… {}/{} files\n{}",
                    p.files_done, p.files_total, p.current_file
                ),
            };
            window.set(frac, &label);
        })
        .await?;
        outcome.downloaded = fetched;
        let _ = bytes;
        // Progress window closes on drop here.
    }

    // Delete files the new manifest dropped, then prune empty dirs.
    for rel in &to_delete {
        let p = game_dir.join(rel);
        let _ = std::fs::remove_file(&p);
        // Walk up pruning empty parents, stopping at game_dir.
        let mut dir = p.parent().map(|d| d.to_path_buf());
        while let Some(d) = dir {
            if d == game_dir || !d.starts_with(game_dir) {
                break;
            }
            if std::fs::remove_dir(&d).is_err() {
                break; // not empty (or gone) — stop climbing
            }
            dir = d.parent().map(|d| d.to_path_buf());
        }
        outcome.deleted += 1;
    }

    // Persist the new manifest + ETag so next launch can 304.
    manifest::save_local(state_dir, &remote_bytes, etag.as_deref())?;

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_versions("0.3.0", "0.2.0"), Greater);
        assert_eq!(cmp_versions("0.2.0", "0.3.0"), Less);
        assert_eq!(cmp_versions("0.3.0", "0.3.0"), Equal);
        assert_eq!(cmp_versions("0.3", "0.3.0"), Equal);
        assert_eq!(cmp_versions("0.10.0", "0.9.9"), Greater);
    }
}
