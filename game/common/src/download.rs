// Parallel file downloader with resume and retries.
//
// - Files download concurrently (default 12) over one shared connection pool.
// - The caller decides which files need fetching; files already trusted on
//   disk are filtered out before this runs, so nothing here re-verifies
//   existing files (re-hashing gigabytes on the check path froze slower PCs).
// - Each download goes to a ".part" temp file first and is renamed into
//   place only after the hash verifies, so a half-written file never looks
//   valid.
// - Progress is reported through a callback after every file.

use crate::manifest::FileEntry;
use futures_util::StreamExt;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

pub struct DownloadOptions {
    pub concurrency: usize,
    pub retries: u32,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            // Hugging Face rate-limits anonymous IPs (HTTP 429). 12 parallel
            // connections was tripping it; 4 stays under the radar.
            concurrency: 4,
            retries: 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    /// None when the manifest carries no sizes.
    pub bytes_total: Option<u64>,
    pub current_file: String,
}

pub fn sha256_of_file(path: &Path) -> Result<String, String> {
    use sha2::Digest as _;
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    // Large read buffer: game files are gigabytes, small reads crawl.
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, f);
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut reader, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hex::encode(hasher.finalize()))
}

fn hash_matches(path: &Path, sha256: &str) -> bool {
    if sha256.is_empty() {
        return path.exists();
    }
    sha256_of_file(path)
        .map(|h| h.eq_ignore_ascii_case(sha256))
        .unwrap_or(false)
}

/// Hash check that never blocks the async runtime: hashing gigabytes on a
/// worker thread starves the whole runtime (network stalls, UI freezes).
async fn hash_matches_blocking(path: &Path, sha256: &str) -> Result<bool, String> {
    let path = path.to_path_buf();
    let sha256 = sha256.to_string();
    tokio::task::spawn_blocking(move || hash_matches(&path, &sha256))
        .await
        .map_err(|e| format!("verify task failed: {e}"))
}

async fn stream_body(
    resp: reqwest::Response,
    tmp: &Path,
    what: &str,
    append: bool,
    mut bytes: u64,
) -> Result<u64, String> {
    let mut out = if append {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(tmp)
            .await
            .map_err(|e| e.to_string())?
    } else {
        tokio::fs::File::create(tmp)
            .await
            .map_err(|e| e.to_string())?
    };
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download failed for {what}: {e}"))?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        bytes += chunk.len() as u64;
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    Ok(bytes)
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    tmp: &Path,
    what: &str,
) -> Result<u64, String> {
    if let Some(parent) = tmp.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    // Resume: if a .part file survived an earlier attempt (or an earlier
    // installer run), ask the server to continue where it stopped instead
    // of starting over. write_all never leaves a torn tail, so the part
    // file is always a clean prefix of the remote file.
    let resume_from: u64 = tokio::fs::metadata(tmp)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if resume_from > 0 {
        let resp = client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={resume_from}-"))
            .send()
            .await
            .map_err(|e| format!("download failed for {what}: {e}"))?;
        if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            return stream_body(resp, tmp, what, true, resume_from).await;
        }
        // Server ignored the range or rejected it (416): the part file is
        // stale, drop it and do a clean full download below.
        let _ = tokio::fs::remove_file(tmp).await;
    }
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download failed for {what}: {e}"))?;
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        // Hugging Face rate limit: wait it out instead of failing fast.
        // The caller retries with its own backoff; we signal with a
        // distinctive message so the log shows what happened.
        return Err(format!(
            "download failed for {what}: HTTP 429 Too Many Requests (rate limited, will retry)"
        ));
    }
    if !resp.status().is_success() {
        return Err(format!(
            "download failed for {what}: HTTP {}",
            resp.status()
        ));
    }
    stream_body(resp, tmp, what, false, 0).await
}

async fn download_one(
    client: reqwest::Client,
    entry: FileEntry,
    dir: &Path,
    opts: &DownloadOptions,
) -> Result<(u64, bool), String> {
    let dest = dir.join(&entry.path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // NOTE: no existence/hash check here — the caller (update_game_files)
    // filters out files already trusted on disk. Everything reaching this
    // point is downloaded fresh and hash-verified below.
    let tmp = dir.join(format!("{}.part", entry.path));
    let mut last_err = String::new();
    // Rate limits (HTTP 429 from Hugging Face) get their own generous retry
    // budget: anonymous CI IPs can be throttled for several minutes, and a
    // single 429 must never kill a multi-GB install. Normal errors keep the
    // caller's smaller budget.
    const RATE_LIMIT_RETRIES: u32 = 6; // 15s,30s,60s,120s,240s,240s ≈ 12 min worst case
    let mut attempt: u32 = 0;
    loop {
        if attempt > 0 {
            // Longer wait for rate limits (429): 15s, 30s, 60s, 120s, 240s...
            // Normal errors: 2s, 4s, 8s, 16s.
            let is_rate_limited = last_err.contains("429");
            let wait_secs = if is_rate_limited {
                (15u64 << (attempt - 1).min(4)).min(240)
            } else {
                1u64 << attempt.min(4)
            };
            tokio::time::sleep(Duration::from_secs(wait_secs)).await;
        }
        match fetch_one(&client, &entry.url, &tmp, &entry.path).await {
            Ok(bytes) => {
                if !entry.sha256.is_empty()
                    && !hash_matches_blocking(&tmp, &entry.sha256).await?
                {
                    last_err = format!("hash mismatch: {}", entry.path);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    if attempt >= opts.retries {
                        break;
                    }
                    attempt += 1;
                    continue;
                }
                tokio::fs::rename(&tmp, &dest)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok((bytes, true));
            }
            Err(e) => {
                let budget = if e.contains("429") {
                    RATE_LIMIT_RETRIES
                } else {
                    opts.retries
                };
                last_err = e;
                if attempt >= budget {
                    break;
                }
                attempt += 1;
            }
        }
    }
    // NOTE: on final failure the .part file is deliberately KEPT (not
    // deleted): the next installer run resumes it via HTTP Range instead
    // of downloading from scratch. The installer dialog already promises
    // "it resumes where it left off" — this makes it true.
    Err(format!(
        "{} (after {} tries; partial file kept, re-run to resume)",
        last_err,
        attempt + 1
    ))
}

/// Download `files` into `dir`, calling `on_progress` after each file.
/// Returns (bytes downloaded this run, files actually downloaded).
/// Stops at the first error.
pub async fn download_files<F>(
    client: &reqwest::Client,
    files: &[FileEntry],
    dir: &Path,
    opts: &DownloadOptions,
    on_progress: F,
) -> Result<(u64, usize), String>
where
    F: Fn(Progress) + Send + Sync + 'static,
{
    let total = files.len();
    let bytes_total: Option<u64> = {
        let mut sum = 0u64;
        let mut all_known = true;
        for f in files {
            match f.size {
                Some(s) => sum += s,
                None => {
                    all_known = false;
                    break;
                }
            }
        }
        if all_known { Some(sum) } else { None }
    };

    let sem = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let files_done = Arc::new(AtomicUsize::new(0));
    let bytes_done = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    let on_progress = Arc::new(on_progress);

    let mut set = tokio::task::JoinSet::new();
    for entry in files.iter().cloned() {
        if failed.load(Ordering::SeqCst) {
            break;
        }
        let client = client.clone();
        let dir = dir.to_path_buf();
        let sem = sem.clone();
        let files_done = files_done.clone();
        let bytes_done = bytes_done.clone();
        let failed = failed.clone();
        let on_progress = on_progress.clone();
        let opts = DownloadOptions {
            concurrency: opts.concurrency,
            retries: opts.retries,
        };
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.map_err(|e| e.to_string())?;
            if failed.load(Ordering::SeqCst) {
                return Ok((0u64, false));
            }
            let (bytes, fetched) = download_one(client, entry.clone(), &dir, &opts).await?;
            let done = files_done.fetch_add(1, Ordering::SeqCst) + 1;
            let bdone = bytes_done.fetch_add(bytes, Ordering::SeqCst) + bytes;
            on_progress(Progress {
                files_done: done,
                files_total: total,
                bytes_done: bdone,
                bytes_total,
                current_file: entry.path.clone(),
            });
            Ok::<(u64, bool), String>((bytes, fetched))
        });
    }

    let mut downloaded: u64 = 0;
    let mut fetched_count: usize = 0;
    let mut first_err: Option<String> = None;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok((b, fetched))) => {
                downloaded += b;
                if fetched {
                    fetched_count += 1;
                }
            }
            Ok(Err(e)) => {
                failed.store(true, Ordering::SeqCst);
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
            Err(e) => {
                failed.store(true, Ordering::SeqCst);
                if first_err.is_none() {
                    first_err = Some(format!("download task crashed: {e}"));
                }
            }
        }
    }
    if let Some(e) = first_err {
        return Err(e);
    }
    Ok((downloaded, fetched_count))
}
