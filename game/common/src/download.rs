// Parallel file downloader with resume and retries.
//
// - Files download concurrently (default 12) over one shared connection pool.
// - A file already on disk with a matching sha256 is skipped, so an
//   interrupted run (sleep, crash, closed laptop) resumes where it left off.
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
            concurrency: 12,
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

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    tmp: &Path,
    what: &str,
) -> Result<u64, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download failed for {what}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "download failed for {what}: HTTP {}",
            resp.status()
        ));
    }
    if let Some(parent) = tmp.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut out = tokio::fs::File::create(tmp)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut bytes: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download failed for {what}: {e}"))?;
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        bytes += chunk.len() as u64;
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    Ok(bytes)
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
    // Resume: already valid on disk (checked off the async runtime so
    // hashing big files can't stall downloads or the progress window).
    if hash_matches_blocking(&dest, &entry.sha256).await? {
        return Ok((0, false));
    }
    let tmp = dir.join(format!("{}.part", entry.path));
    let mut last_err = String::new();
    for attempt in 0..=opts.retries {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1 << attempt.min(4))).await;
        }
        match fetch_one(&client, &entry.url, &tmp, &entry.path).await {
            Ok(bytes) => {
                if !entry.sha256.is_empty()
                    && !hash_matches_blocking(&tmp, &entry.sha256).await?
                {
                    last_err = format!("hash mismatch: {}", entry.path);
                    let _ = tokio::fs::remove_file(&tmp).await;
                    continue;
                }
                tokio::fs::rename(&tmp, &dest)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok((bytes, true));
            }
            Err(e) => last_err = e,
        }
    }
    let _ = tokio::fs::remove_file(&tmp).await;
    Err(format!("{} (after {} tries)", last_err, opts.retries + 1))
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
