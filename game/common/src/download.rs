// Parallel file downloader with resume, retries, and chunked large-file downloads.
//
// v0.5.2 changes (the installer reliability overhaul):
// - NO blanket per-request timeout. The old 120s total timeout killed the
//   2GB file on any connection slower than ~17MB/s, and because
//   archive.recagain.site ignores HTTP Range, every retry restarted that
//   file from zero — an infinite death loop that looked like a frozen or
//   crashing installer. Now: connect timeout only, plus stall detection
//   (any stream silent for 60s is aborted and retried).
// - Big files (>= 32MB) from Range-capable hosts download as 8 parallel
//   HTTP Range chunks, each independently retried. The 10 big game files
//   now live on Hugging Face's CDN, which honors Range.
// - Per-host Range support is probed once and cached: hosts that ignore
//   Range (RecAgain) fall back to single-stream with stall detection.
// - Progress is now byte-live: the callback fires at most every 500ms with
//   in-flight bytes, so the bar moves during the 2GB file instead of
//   sitting at 8% for an hour.
// - Each download goes to a ".part" temp file first and is renamed into
//   place only after the hash verifies, so a half-written file never looks
//   valid. Chunk temp files (".chunkN") are kept on failure so the next run
//   resumes them.

use crate::manifest::FileEntry;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

pub struct DownloadOptions {
    pub concurrency: usize,
    pub retries: u32,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            // Game files now come from the HF CDN (Range-capable) and the
            // Cloudflare-fronted RecAgain archive; 8 parallel connections
            // is safe on both and much faster for 3,742 small files.
            concurrency: 8,
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

/// How long a stream may go without yielding a single byte before the
/// attempt is declared dead and retried. Replaces the old blanket 120s
/// total-request timeout, which murdered large files on slow links.
const STALL_SECS: u64 = 60;
/// Files at or above this size use parallel chunked download when the
/// host supports HTTP Range.
const CHUNKED_THRESHOLD: u64 = 32 * 1024 * 1024;
const CHUNK_COUNT: u64 = 8;
/// Callback throttle: never fire progress more often than this.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(500);

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

// ---------------------------------------------------------------------------
// Range-support probe (per host, cached for the process lifetime)
// ---------------------------------------------------------------------------

fn host_key(url: &str) -> String {
    // scheme://host — enough to distinguish our two mirrors.
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{host}")
        }
        None => url.to_string(),
    }
}

fn range_cache() -> &'static std::sync::Mutex<HashMap<String, bool>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

async fn host_supports_range(client: &reqwest::Client, url: &str) -> bool {
    let key = host_key(url);
    if let Ok(cache) = range_cache().lock() {
        if let Some(&v) = cache.get(&key) {
            return v;
        }
    }
    // Tiny probe: ask for the first byte only.
    let ok = match tokio::time::timeout(
        Duration::from_secs(20),
        client.get(url).header(reqwest::header::RANGE, "bytes=0-0").send(),
    )
    .await
    {
        Ok(Ok(resp)) => resp.status() == reqwest::StatusCode::PARTIAL_CONTENT,
        _ => false,
    };
    if let Ok(mut cache) = range_cache().lock() {
        cache.insert(key, ok);
    }
    ok
}

// ---------------------------------------------------------------------------
// Live progress state shared by all in-flight downloads
// ---------------------------------------------------------------------------

struct ProgressState {
    files_done: AtomicUsize,
    bytes_done: AtomicU64,
    files_total: usize,
    bytes_total: Option<u64>,
    last_fire: std::sync::Mutex<Instant>,
    fire: Arc<dyn Fn(Progress) + Send + Sync>,
}

impl ProgressState {
    fn add_bytes(&self, n: u64) {
        if n == 0 {
            return;
        }
        self.bytes_done.fetch_add(n, Ordering::Relaxed);
        self.maybe_fire("");
    }

    fn file_done(&self, current_file: &str) {
        let done = self.files_done.fetch_add(1, Ordering::SeqCst) + 1;
        self.fire_now(done, current_file);
    }

    fn maybe_fire(&self, current_file: &str) {
        let should = match self.last_fire.lock() {
            Ok(mut t) => {
                if t.elapsed() >= PROGRESS_MIN_INTERVAL {
                    *t = Instant::now();
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        };
        if should {
            let done = self.files_done.load(Ordering::SeqCst);
            self.fire_now(done, current_file);
        }
    }

    fn fire_now(&self, done: usize, current_file: &str) {
        (self.fire)(Progress {
            files_done: done,
            files_total: self.files_total,
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total,
            current_file: current_file.to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// Streaming with stall detection (no total timeout)
// ---------------------------------------------------------------------------

/// Read one stream item, failing if no bytes arrive within STALL_SECS.
/// Returns Ok(None) on clean end-of-stream.
async fn next_chunk(
    stream: &mut futures_util::stream::BoxStream<
        '_,
        Result<bytes::Bytes, reqwest::Error>,
    >,
    what: &str,
) -> Result<Option<bytes::Bytes>, String> {
    match tokio::time::timeout(Duration::from_secs(STALL_SECS), stream.next()).await {
        Err(_) => Err(format!(
            "download stalled for {what}: no data for {STALL_SECS}s (connection dropped?)"
        )),
        Ok(None) => Ok(None),
        Ok(Some(Err(e))) => Err(format!("download failed for {what}: {e}")),
        Ok(Some(Ok(b))) => Ok(Some(b)),
    }
}

async fn stream_body(
    resp: reqwest::Response,
    tmp: &Path,
    what: &str,
    append: bool,
    progress: &ProgressState,
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
    let mut stream = resp.bytes_stream().boxed();
    let mut bytes: u64 = 0;
    loop {
        match next_chunk(&mut stream, what).await? {
            None => break,
            Some(chunk) => {
                out.write_all(&chunk).await.map_err(|e| e.to_string())?;
                bytes += chunk.len() as u64;
                progress.add_bytes(chunk.len() as u64);
            }
        }
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Single-stream download (small files, no-Range hosts, legacy .part resume)
// ---------------------------------------------------------------------------

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    tmp: &Path,
    what: &str,
    range_ok: bool,
    progress: &ProgressState,
) -> Result<u64, String> {
    if let Some(parent) = tmp.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    if range_ok {
        // Resume: if a .part file survived an earlier attempt (or an earlier
        // installer run), ask the server to continue where it stopped instead
        // of starting over. write_all never leaves a torn tail, so the part
        // file is always a clean prefix of the remote file.
        let resume_from: u64 = tokio::fs::metadata(tmp)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if resume_from > 0 {
            progress.add_bytes(resume_from); // count what's already on disk
            let resp = client
                .get(url)
                .header(reqwest::header::RANGE, format!("bytes={resume_from}-"))
                .send()
                .await
                .map_err(|e| format!("download failed for {what}: {e}"))?;
            if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                let got = stream_body(resp, tmp, what, true, progress).await?;
                return Ok(resume_from + got);
            }
            // Server ignored the range or rejected it (416): the part file is
            // stale, drop it and do a clean full download below.
            let _ = tokio::fs::remove_file(tmp).await;
        }
    } else if tokio::fs::metadata(tmp).await.is_ok() {
        // Host can't resume: a stale .part can only ever restart from zero,
        // so drop it now instead of appending garbage to it later.
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
    stream_body(resp, tmp, what, false, progress).await
}

// ---------------------------------------------------------------------------
// Chunked parallel download (large files on Range-capable hosts)
// ---------------------------------------------------------------------------

async fn fetch_range(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64, // exclusive
    chunk_path: &Path,
    what: &str,
    progress: &ProgressState,
) -> Result<(), String> {
    let expected = end - start;
    let resp = client
        .get(url)
        .header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", start, end - 1),
        )
        .send()
        .await
        .map_err(|e| format!("download failed for {what}: {e}"))?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(format!(
            "download failed for {what}: range request rejected (HTTP {})",
            resp.status()
        ));
    }
    let mut out = tokio::fs::File::create(chunk_path)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream().boxed();
    let mut got: u64 = 0;
    loop {
        match next_chunk(&mut stream, what).await? {
            None => break,
            Some(chunk) => {
                out.write_all(&chunk).await.map_err(|e| e.to_string())?;
                got += chunk.len() as u64;
                progress.add_bytes(chunk.len() as u64);
            }
        }
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    if got != expected {
        let _ = tokio::fs::remove_file(chunk_path).await;
        return Err(format!(
            "download failed for {what}: short chunk ({got}/{expected} bytes)"
        ));
    }
    Ok(())
}

async fn download_chunk(
    client: reqwest::Client,
    url: String,
    start: u64,
    end: u64,
    chunk_path: std::path::PathBuf,
    what: String,
    progress: Arc<ProgressState>,
) -> Result<(), String> {
    let expected = end - start;
    // Resume at chunk granularity: a finished chunk file is kept as-is.
    if let Ok(md) = tokio::fs::metadata(&chunk_path).await {
        if md.len() == expected {
            progress.add_bytes(expected);
            return Ok(());
        }
        let _ = tokio::fs::remove_file(&chunk_path).await;
    }
    let mut attempt: u32 = 0;
    loop {
        match fetch_range(&client, &url, start, end, &chunk_path, &what, &progress).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempt >= 5 {
                    return Err(format!("{e} (chunk gave up after 6 tries)"));
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(1u64 << attempt.min(4))).await;
            }
        }
    }
}

async fn download_chunked(
    client: &reqwest::Client,
    entry: &FileEntry,
    dir: &Path,
    progress: &Arc<ProgressState>,
) -> Result<u64, String> {
    let total = entry.size.unwrap_or(0);
    let dest = dir.join(&entry.path);
    let tmp = dir.join(format!("{}.part", entry.path));
    if total == 0 {
        return Err(format!("cannot chunk {}: unknown size", entry.path));
    }
    // Legacy .part from an older installer run: with Range support we can
    // resume it single-stream instead of chunking; simpler and just as fast
    // on the CDN. (Fresh downloads go through the chunked path below.)
    if tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0) > 0 {
        let got = fetch_one(client, &entry.url, &tmp, &entry.path, true, progress).await?;
        return finalize_tmp(&tmp, &dest, entry, got).await;
    }

    let chunk_size = (total + CHUNK_COUNT - 1) / CHUNK_COUNT;
    let n = ((total + chunk_size - 1) / chunk_size) as usize;
    let mut set = tokio::task::JoinSet::new();
    for i in 0..n {
        let start = i as u64 * chunk_size;
        let end = ((i as u64 + 1) * chunk_size).min(total);
        let client = client.clone();
        let url = entry.url.clone();
        let what = format!("{}[chunk {}/{}]", entry.path, i + 1, n);
        let chunk_path = dir.join(format!("{}.chunk{i}", entry.path));
        let progress = progress.clone();
        set.spawn(async move {
            download_chunk(client, url, start, end, chunk_path, what, progress).await
        });
    }
    let mut first_err: Option<String> = None;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(format!("download task crashed: {e}"));
                }
            }
        }
    }
    if let Some(e) = first_err {
        // Chunk files are deliberately kept: the next run resumes the
        // finished ones instead of starting over.
        return Err(e);
    }
    // Assemble in order.
    let mut part = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| e.to_string())?;
    for i in 0..n {
        let chunk_path = dir.join(format!("{}.chunk{i}", entry.path));
        let mut cf = tokio::fs::File::open(&chunk_path)
            .await
            .map_err(|e| format!("missing chunk file {}: {e}", chunk_path.display()))?;
        tokio::io::copy(&mut cf, &mut part)
            .await
            .map_err(|e| e.to_string())?;
        drop(cf);
        let _ = tokio::fs::remove_file(&chunk_path).await;
    }
    part.flush().await.map_err(|e| e.to_string())?;
    drop(part);
    finalize_tmp(&tmp, &dest, entry, total).await
}

/// Hash-verify tmp and move it into place. Used by both download paths.
async fn finalize_tmp(
    tmp: &Path,
    dest: &Path,
    entry: &FileEntry,
    bytes: u64,
) -> Result<u64, String> {
    if !entry.sha256.is_empty() && !hash_matches_blocking(tmp, &entry.sha256).await? {
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(format!("hash mismatch: {}", entry.path));
    }
    tokio::fs::rename(tmp, dest)
        .await
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// One file: pick chunked vs single-stream, retry, verify
// ---------------------------------------------------------------------------

async fn download_one(
    client: reqwest::Client,
    entry: FileEntry,
    dir: &Path,
    opts: &DownloadOptions,
    range_ok: bool,
    progress: Arc<ProgressState>,
) -> Result<(u64, bool), String> {
    let dest = dir.join(&entry.path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // NOTE: no existence/hash check here — the caller (update_game_files)
    // filters out files already trusted on disk. Everything reaching this
    // point is downloaded fresh and hash-verified below.
    let tmp = dir.join(format!("{}.part", entry.path));
    let chunked = range_ok && entry.size.unwrap_or(0) >= CHUNKED_THRESHOLD;
    let mut last_err = String::new();
    // Rate limits (HTTP 429 from Hugging Face) get their own generous retry
    // budget: anonymous IPs can be throttled for several minutes, and a
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
        let r = if chunked {
            download_chunked(&client, &entry, dir, &progress).await
        } else {
            match fetch_one(&client, &entry.url, &tmp, &entry.path, range_ok, &progress).await {
                Ok(bytes) => finalize_tmp(&tmp, &dest, &entry, bytes).await,
                Err(e) => Err(e),
            }
        };
        match r {
            Ok(bytes) => {
                progress.file_done(&entry.path);
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
    // NOTE: on final failure the .part/.chunk files are deliberately KEPT
    // (not deleted): the next installer run resumes them via HTTP Range
    // instead of downloading from scratch. The installer dialog already
    // promises "it resumes where it left off" — this makes it true.
    Err(format!(
        "{} (after {} tries; partial files kept, re-run to resume)",
        last_err,
        attempt + 1
    ))
}

/// Download `files` into `dir`, calling `on_progress` as bytes arrive
/// (throttled) and after each file.
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
        if all_known {
            Some(sum)
        } else {
            None
        }
    };

    // Probe Range support once per host up front, so every file knows which
    // strategy to use without each task probing on its own.
    let mut range_by_host: HashMap<String, bool> = HashMap::new();
    {
        let mut seen: Vec<String> = Vec::new();
        for f in files {
            let k = host_key(&f.url);
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        for k in seen {
            let probe_url = files
                .iter()
                .find(|f| host_key(&f.url) == k)
                .map(|f| f.url.clone())
                .unwrap();
            range_by_host.insert(k, host_supports_range(client, &probe_url).await);
        }
    }

    let state = Arc::new(ProgressState {
        files_done: AtomicUsize::new(0),
        bytes_done: AtomicU64::new(0),
        files_total: total,
        bytes_total,
        last_fire: std::sync::Mutex::new(Instant::now()),
        fire: Arc::new(on_progress),
    });

    let sem = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let failed = Arc::new(AtomicBool::new(false));

    let mut set = tokio::task::JoinSet::new();
    for entry in files.iter().cloned() {
        if failed.load(Ordering::SeqCst) {
            break;
        }
        let client = client.clone();
        let dir = dir.to_path_buf();
        let sem = sem.clone();
        let failed = failed.clone();
        let progress = state.clone();
        let range_ok = *range_by_host
            .get(&host_key(&entry.url))
            .unwrap_or(&false);
        let opts = DownloadOptions {
            concurrency: opts.concurrency,
            retries: opts.retries,
        };
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.map_err(|e| e.to_string())?;
            if failed.load(Ordering::SeqCst) {
                return Ok((0u64, false));
            }
            download_one(client, entry, &dir, &opts, range_ok, progress).await
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
