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

/// Append one timestamped line to the update log in the state dir.
/// This is the diagnostic lifeline: if an update ever stalls or fails on
/// a player's PC, this file shows exactly how far it got.
fn log_line(state_dir: &Path, msg: &str) {
    use std::io::Write as _;
    let path = state_dir.join("fluxrec-update.log");
    // Cheap rotation: start fresh past ~1MB.
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 1_000_000 {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

fn http_client() -> Result<reqwest::Client, String> {
    // NOTE: no total request timeout on purpose. The old 120s blanket
    // timeout killed multi-GB game files on slow connections (and the
    // archive host ignores Range, so every retry restarted from zero).
    // The file downloader now detects stalled streams itself (60s with no
    // bytes = dead) and retries; small control-plane requests below set
    // their own per-request timeouts.
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
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

/// Fetch the remote manifest's `version` string (cheap GET of the manifest
/// JSON — no ETag logic, no diff). Used for build switches: if the mirror
/// moved to a new build (e.g. "0.5.0-showdown"), the local game dir must be
/// wiped for a clean install instead of diffing. Errors mean "unknown".
pub async fn remote_manifest_version(manifest_url: &str) -> Result<String, String> {
    let client = http_client()?;
    let bytes = client
        .get(manifest_url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("manifest version check failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("manifest version check failed: {e}"))?;
    let m: Manifest = manifest::parse(&bytes)?;
    m.version
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "manifest has no version field".to_string())
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
    // Download the new exe next to the current one. Generous per-request
    // timeout: the shared client no longer has a total timeout (the game
    // file downloader handles stalls itself), so small downloads like this
    // set their own.
    let bytes = client
        .get(&info.bootstrap_url)
        .timeout(Duration::from_secs(600))
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
///
/// Checking is instant by design: files already on disk are trusted without
/// re-hashing (re-hashing gigabytes froze slower PCs). Fresh downloads are
/// still SHA-256 verified before being put in place.
pub async fn update_game_files(
    manifest_url: &str,
    game_dir: &Path,
    state_dir: &Path,
    window_title: &str,
) -> Result<GameUpdateOutcome, String> {
    let r = update_game_files_inner(manifest_url, game_dir, state_dir, window_title).await;
    if let Err(e) = &r {
        log_line(state_dir, &format!("ERROR: {e}"));
    }
    r
}

async fn update_game_files_inner(
    manifest_url: &str,
    game_dir: &Path,
    state_dir: &Path,
    window_title: &str,
) -> Result<GameUpdateOutcome, String> {
    let _no_sleep = PreventSleep::new();
    let client = http_client()?;
    std::fs::create_dir_all(game_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    log_line(
        state_dir,
        &format!("update start (common v{})", env!("CARGO_PKG_VERSION")),
    );

    // What do we have locally?
    let local = manifest::load_local(state_dir);
    let local_manifest: Option<Manifest> = local
        .as_ref()
        .and_then(|(bytes, _)| manifest::parse(bytes).ok());

    // Fetch the remote manifest (conditional request when we have an ETag).
    // Hugging Face rate-limits anonymous IPs (HTTP 429). Back off hard on
    // 429 instead of hammering: 10s, 30s, 60s, then give up.
    let mut req = client.get(manifest_url);
    if let Some((_, Some(etag))) = &local {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = {
        let mut last_err = String::new();
        let mut result = None;
        for attempt in 0..4 {
            if attempt > 0 {
                let wait = [10, 30, 60][attempt - 1];
                log_line(state_dir, &format!("manifest 429, waiting {wait}s (attempt {attempt}/3)"));
                println!("Rate limited by download server, waiting {wait}s...");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                // Rebuild the request (req was consumed by send()).
                let mut r = client.get(manifest_url);
                if let Some((_, Some(etag))) = &local {
                    r = r.header(reqwest::header::IF_NONE_MATCH, etag);
                }
                req = r;
            }
            match req.send().await {
                Ok(r) => {
                    if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
                        last_err = "HTTP 429 Too Many Requests".to_string();
                        // Need to rebuild req for next iteration; do it at loop top.
                        // Temporarily store a fresh request builder.
                        let mut r2 = client.get(manifest_url);
                        if let Some((_, Some(etag))) = &local {
                            r2 = r2.header(reqwest::header::IF_NONE_MATCH, etag);
                        }
                        req = r2;
                        continue;
                    }
                    result = Some(r);
                    break;
                }
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
            }
        }
        match result {
            Some(r) => r,
            None => return Err(format!("manifest request failed: {last_err} (rate limited, try again later)")),
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
    log_line(
        state_dir,
        &format!(
            "manifest: {} files, local_manifest={}",
            remote.files.len(),
            local_manifest.is_some()
        ),
    );
    let Diff {
        mut to_download,
        to_delete,
    } = manifest::diff(local_manifest.as_ref(), &remote);
    // Visible in the installer log: without this, the log goes silent
    // between "Checking game files..." and the first download progress,
    // which looks exactly like a freeze.
    println!(
        "Manifest: {} files{}.",
        remote.files.len(),
        match &local_manifest {
            Some(_) => " (checking for updates)",
            None => " (first run)",
        }
    );

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

    // First-run migration (no local manifest yet — e.g. files left by the
    // previous installer): those files were hash-verified when originally
    // downloaded, so trust what's on disk and only fetch what's missing.
    // This is what keeps the check instant instead of re-hashing gigabytes.
    // On later runs the local manifest exists, so changed files are always
    // re-downloaded even though they're present.
    let mut trusted = 0usize;
    if local_manifest.is_none() {
        let before = to_download.len();
        to_download.retain(|f| !game_dir.join(&f.path).exists());
        trusted = before - to_download.len();
    }
    log_line(
        state_dir,
        &format!(
            "check done: {} to download, {} trusted as-is",
            to_download.len(),
            trusted
        ),
    );

    let mut outcome = GameUpdateOutcome {
        downloaded: 0,
        deleted: 0,
    };

    if !to_download.is_empty() {
        // A single rate-limited file must not kill a multi-GB install.
        // Retry the whole set in up to 3 passes: finished files exist on
        // disk (skipped by the retain) and interrupted ones resume from
        // their .part files, so each pass only fetches what's still missing.
        let mut last_err = String::new();
        for pass in 1..=3u32 {
            to_download.retain(|f| !game_dir.join(&f.path).exists());
            if to_download.is_empty() {
                last_err.clear();
                break;
            }
            if pass > 1 {
                log_line(
                    state_dir,
                    &format!(
                        "download pass {pass}/3: {} files still missing, waiting 60s before retry",
                        to_download.len()
                    ),
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            let window = ProgressWindow::new(window_title);
            println!("Downloading {} files... (pass {pass}/3)", to_download.len());
            let opts = DownloadOptions::default();
            // The NSIS installer log only shows our stdout, and the separate
            // progress window can end up behind the installer — so also print
            // throttled progress lines here. A silent 7GB download looks
            // exactly like a frozen installer otherwise.
            let print_every = (to_download.len() / 50).max(1);
            let next_print =
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(print_every));
            match download_files(&client, &to_download, game_dir, &opts, move |p: Progress| {
                let frac = if p.files_total > 0 {
                    p.files_done as f64 / p.files_total as f64
                } else {
                    1.0
                };
                let label = match p.bytes_total {
                    Some(bt) if bt > 0 => format!(
                        "Downloading game files... {}/{} files ({:.1}/{:.1} MB)\n{}",
                        p.files_done,
                        p.files_total,
                        p.bytes_done as f64 / 1048576.0,
                        bt as f64 / 1048576.0,
                        p.current_file
                    ),
                    _ => format!(
                        "Downloading game files... {}/{} files\n{}",
                        p.files_done, p.files_total, p.current_file
                    ),
                };
                window.set(frac, &label);
                let threshold = next_print.load(std::sync::atomic::Ordering::SeqCst);
                if p.files_done >= threshold || p.files_done == p.files_total {
                    next_print.store(
                        p.files_done + print_every,
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    println!(
                        "Downloaded {}/{} files ({:.0}%)...",
                        p.files_done,
                        p.files_total,
                        frac * 100.0
                    );
                }
            })
            .await
            {
                Ok((bytes, fetched)) => {
                    outcome.downloaded += fetched;
                    let _ = bytes;
                    last_err.clear();
                    break;
                }
                Err(e) => {
                    log_line(state_dir, &format!("download pass {pass}/3 failed: {e}"));
                    last_err = e;
                }
            }
            // Progress window closes on drop here.
        }
        to_download.retain(|f| !game_dir.join(&f.path).exists());
        if !to_download.is_empty() {
            return Err(last_err);
        }
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
    log_line(
        state_dir,
        &format!(
            "done: {} downloaded, {} deleted",
            outcome.downloaded, outcome.deleted
        ),
    );

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
