//! Parallel segmented downloading for large files.
//!
//! Splits a download into ~8 parallel HTTP Range segments, then concatenates
//! them. Falls back to a plain single-stream download when the server does
//! not honor Range requests. Same verification contract as `download()` in
//! the crate root (size + MD5), so it is a drop-in accelerator.
//!
//! Robustness rules (learned the hard way):
//! - every segment has a stall timeout: no data for STALL_SECS -> retry;
//! - segments RESUME across retries (Range from the bytes already on disk),
//!   so a flaky connection never restarts a 475MB chunk from zero;
//! - a progress-pump task updates the GUI ~4x/sec from the shared byte
//!   counter, with live MB/s + ETA text — the bar can never look frozen.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

/// Number of parallel segments for large files.
pub const SEGMENTS: usize = 8;
/// Files smaller than this just use a single stream (segments add nothing).
pub const SEGMENT_MIN_SIZE: u64 = 8 * 1024 * 1024;
/// Per-segment attempts before the whole download fails.
const SEGMENT_ATTEMPTS: u32 = 5;
/// No data for this long -> the segment is stalled, abort and retry.
const STALL_SECS: u64 = 60;

/// Compute inclusive byte ranges for `n` segments covering `total` bytes.
/// Pure and unit-tested.
pub fn segment_ranges(total: u64, n: usize) -> Vec<(u64, u64)> {
    if total == 0 || n == 0 {
        return Vec::new();
    }
    let n = n.min(total as usize);
    let base = total / n as u64;
    let mut ranges = Vec::with_capacity(n);
    let mut start = 0u64;
    for i in 0..n {
        let extra = if (i as u64) < total % n as u64 { 1 } else { 0 };
        let end = start + base + extra - 1;
        ranges.push((start, end));
        start = end + 1;
    }
    ranges
}

fn md5_of_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut ctx = md5::Context::new();
    std::io::copy(&mut f, &mut ctx).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", ctx.compute()))
}

fn file_ok(path: &Path, expected_md5: Option<&str>, expected_size: Option<u64>) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if let Some(s) = expected_size {
        if meta.len() != s {
            return false;
        }
    }
    if let Some(h) = expected_md5 {
        match md5_of_file(path) {
            Ok(got) => {
                if !got.eq_ignore_ascii_case(h) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

async fn probe_range(client: &reqwest::Client, url: &str) -> bool {
    let r = client.get(url).header("Range", "bytes=0-0").send().await;
    matches!(r, Ok(resp) if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT)
}

/// Download one byte range into `dest`, with retries and resume.
/// Partial segment files are kept between attempts: a retry resumes via
/// `Range: bytes={already_have}-` instead of restarting the chunk.
/// Reports progress as absolute bytes written into the shared atomic counter.
async fn fetch_segment(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    dest: &Path,
    done: &AtomicU64,
    label: &str,
) -> Result<(), String> {
    let want = end - start + 1;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let have = std::fs::metadata(dest)
            .map(|m| m.len())
            .unwrap_or(0)
            .min(want);
        if have == want {
            return Ok(()); // already complete from an earlier attempt/run
        }
        let resuming = have > 0;
        let range_hdr = if resuming {
            format!("bytes={}-{}", start + have, end)
        } else {
            format!("bytes={start}-{end}")
        };
        let resp = client
            .get(url)
            .header("Range", range_hdr)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            let msg = format!("segment {start}-{end}: HTTP {}", resp.status());
            if resuming {
                // Server ignored our resume offset: restart this chunk clean.
                let _ = std::fs::remove_file(dest);
                done.fetch_sub(have, Ordering::Relaxed);
            }
            if attempt >= SEGMENT_ATTEMPTS {
                return Err(format!("[{label}] {msg}"));
            }
            continue;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(!resuming)
            .append(resuming)
            .open(dest)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = resp.bytes_stream();
        let mut written = 0u64;
        let mut failed: Option<String> = None;
        loop {
            match tokio::time::timeout(Duration::from_secs(STALL_SECS), stream.next()).await {
                Err(_) => {
                    failed = Some(format!(
                        "segment {start}-{end}: stalled {STALL_SECS}s without data"
                    ));
                    break;
                }
                Ok(None) => break, // server closed the stream
                Ok(Some(Err(e))) => {
                    failed = Some(e.to_string());
                    break;
                }
                Ok(Some(Ok(bytes))) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        failed = Some(e.to_string());
                        break;
                    }
                    written += bytes.len() as u64;
                    done.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                }
            }
        }
        let _ = file.flush().await;
        drop(file);
        // Partial file is KEPT: the next attempt resumes from have + written.
        // The shared counter already includes those bytes, so no rewind.
        if failed.is_none() && have + written == want {
            return Ok(());
        }
        let msg = failed.unwrap_or_else(|| {
            format!(
                "segment {start}-{end}: got {}/{} bytes",
                have + written,
                want
            )
        });
        if attempt >= SEGMENT_ATTEMPTS {
            return Err(format!("[{label}] {msg}"));
        }
        println!("[{label}] segment {start}-{end} attempt {attempt}: {msg} — resuming.");
    }
}

/// Single-stream fallback with progress + verification.
async fn fetch_single(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_md5: Option<&str>,
    expected_size: Option<u64>,
    label: &str,
    progress: Option<(&crate::progress::Progress, &'static str)>,
) -> Result<(), String> {
    if let Some((p, stage)) = progress {
        p.set_stage(stage);
    }
    let part = dest.with_extension("part");
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("[{label}] HTTP {}", resp.status()));
    }
    let total = expected_size.unwrap_or_else(|| resp.content_length().unwrap_or(0));
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| e.to_string())?;
        file.write_all(&bytes).await.map_err(|e| e.to_string())?;
        written += bytes.len() as u64;
        if let (Some((p, _)), true) = (progress, total > 0) {
            p.set_fraction(written as f64 / total as f64);
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
    if !file_ok(dest, expected_md5, expected_size) {
        let _ = std::fs::remove_file(dest);
        return Err(format!("[{label}] verification failed after download"));
    }
    Ok(())
}

fn fmt_eta(secs: u64) -> String {
    if secs < 90 {
        format!("~{secs}s left")
    } else {
        format!("~{}m {}s left", secs / 60, secs % 60)
    }
}

/// Segmented download with the same contract as `crate::download`.
/// Uses ~8 parallel Range segments when the server honors them and the
/// file is large enough; otherwise a single stream.
pub async fn download_segmented(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_md5: Option<&str>,
    expected_size: Option<u64>,
    label: &str,
    progress: Option<(&crate::progress::Progress, &'static str)>,
) -> Result<(), String> {
    if file_ok(dest, expected_md5, expected_size) {
        println!("[{label}] already present and verified, skipping.");
        if let Some((p, stage)) = progress {
            p.set_stage(stage);
            p.set_fraction(1.0);
        }
        return Ok(());
    }
    if dest.exists() {
        println!("[{label}] existing file failed verification, re-downloading.");
        std::fs::remove_file(dest).map_err(|e| e.to_string())?;
    }
    if let Some((p, stage)) = progress {
        p.set_stage(stage);
    }

    let big_enough = expected_size.map(|s| s >= SEGMENT_MIN_SIZE).unwrap_or(true);
    if !big_enough || !probe_range(client, url).await {
        if big_enough {
            println!("[{label}] host does not honor Range — single stream.");
        }
        return fetch_single(client, url, dest, expected_md5, expected_size, label, progress)
            .await;
    }

    // Total size: prefer the known expected size, else the probe's
    // Content-Range. Without a size we cannot segment — fall back.
    let total: u64 = match expected_size {
        Some(s) => s,
        None => {
            let r = client
                .get(url)
                .header("Range", "bytes=0-0")
                .send()
                .await
                .map_err(|e| e.to_string())?;
            match r.headers().get("content-range").and_then(|v| v.to_str().ok()) {
                Some(cr) => cr
                    .rsplit('/')
                    .next()
                    .and_then(|t| t.parse::<u64>().ok())
                    .unwrap_or(0),
                None => 0,
            }
        }
    };
    if total < SEGMENT_MIN_SIZE {
        return fetch_single(client, url, dest, expected_md5, expected_size, label, progress)
            .await;
    }

    println!("[{label}] segmented download: {SEGMENTS} parallel ranges ({total} bytes).");
    let ranges = segment_ranges(total, SEGMENTS);
    let seg_paths: Vec<PathBuf> = (0..ranges.len())
        .map(|i| {
            let mut p = dest.as_os_str().to_owned();
            p.push(format!(".seg{i}"));
            PathBuf::from(p)
        })
        .collect();
    // Bytes already on disk from an earlier interrupted run count from the start.
    let initial: u64 = seg_paths
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();
    let done = Arc::new(AtomicU64::new(initial));

    // Progress pump: smooth bar updates ~4x/sec plus live MB/s + ETA text,
    // so the GUI can never look frozen while bytes are arriving.
    let pump_progress = progress.map(|(p, _)| p.clone());
    let pump_done = done.clone();
    let pump = tokio::spawn(async move {
        let started = Instant::now();
        let mut last_bytes = initial;
        let mut last_tick = started;
        let mut last_text = Instant::now() - Duration::from_secs(10);
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let d = pump_done.load(Ordering::Relaxed);
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            if let Some(ref p) = pump_progress {
                p.set_fraction((d as f64 / total as f64).min(1.0));
                if last_text.elapsed() >= Duration::from_secs(2) {
                    last_text = Instant::now();
                    let tick_dt = last_tick.elapsed().as_secs_f64().max(0.001);
                    let mbps = d.saturating_sub(last_bytes) as f64 / tick_dt / 1_048_576.0;
                    last_bytes = d;
                    last_tick = Instant::now();
                    let avg_mbps = d as f64 / elapsed / 1_048_576.0;
                    let eta = if avg_mbps > 0.05 {
                        let s = ((total.saturating_sub(d)) as f64
                            / (avg_mbps * 1_048_576.0))
                            as u64;
                        format!(" — {}", fmt_eta(s))
                    } else {
                        String::new()
                    };
                    let have_mb = d as f64 / 1_048_576.0;
                    let total_mb = total as f64 / 1_048_576.0;
                    p.set_text(format!(
                        "Downloading game files… {have_mb:.0}/{total_mb:.0} MB ({mbps:.1} MB/s{eta})"
                    ));
                }
            }
        }
    });

    let mut tasks = Vec::with_capacity(ranges.len());
    for (i, (start, end)) in ranges.into_iter().enumerate() {
        let c = client.clone();
        let u = url.to_string();
        let d = seg_paths[i].clone();
        let done_c = done.clone();
        let label_s = label.to_string();
        tasks.push(tokio::spawn(async move {
            fetch_segment(&c, &u, start, end, &d, &done_c, &label_s).await
        }));
    }
    let mut failed: Option<String> = None;
    for t in tasks {
        match t.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                failed = Some(e);
                break;
            }
            Err(e) => {
                failed = Some(format!("segment task panicked: {e}"));
                break;
            }
        }
    }
    pump.abort();
    // Restore the plain stage text for whatever comes next.
    if let Some((p, stage)) = progress {
        p.set_stage(stage);
    }
    if let Some(e) = failed {
        for p in &seg_paths {
            let _ = std::fs::remove_file(p);
        }
        return Err(e);
    }

    // Concatenate in order, then verify.
    {
        let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
        for p in &seg_paths {
            let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
        }
        out.sync_all().map_err(|e| e.to_string())?;
    }
    for p in &seg_paths {
        let _ = std::fs::remove_file(p);
    }
    if let Some((p, _)) = progress {
        p.set_fraction(1.0);
    }
    if !file_ok(dest, expected_md5, expected_size) {
        let _ = std::fs::remove_file(dest);
        return Err(format!("[{label}] verification failed after segmented download"));
    }
    println!("[{label}] segmented download complete + verified.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_split_evenly() {
        assert_eq!(
            segment_ranges(100, 4),
            vec![(0, 24), (25, 49), (50, 74), (75, 99)]
        );
    }

    #[test]
    fn ranges_spread_remainder() {
        // 10 bytes over 3 segments: 4,3,3.
        assert_eq!(segment_ranges(10, 3), vec![(0, 3), (4, 6), (7, 9)]);
    }

    #[test]
    fn ranges_cover_every_byte_exactly_once() {
        for total in [1u64, 7, 8, 9, 100, 1000, 1 << 20] {
            let rs = segment_ranges(total, SEGMENTS);
            assert_eq!(rs.len(), SEGMENTS.min(total as usize));
            let mut expect = 0u64;
            for (s, e) in &rs {
                assert_eq!(*s, expect);
                assert!(*e >= *s);
                expect = e + 1;
            }
            assert_eq!(expect, total);
        }
    }

    #[test]
    fn ranges_clamp_when_smaller_than_segment_count() {
        let rs = segment_ranges(3, 8);
        assert_eq!(rs, vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn ranges_empty_on_zero() {
        assert!(segment_ranges(0, 8).is_empty());
        assert!(segment_ranges(100, 0).is_empty());
    }

    #[test]
    fn file_ok_checks_size_and_md5() {
        let d = std::env::temp_dir().join("fluxrec-seg-test");
        let _ = std::fs::create_dir_all(&d);
        let f = d.join("f.bin");
        std::fs::write(&f, b"hello").unwrap();
        assert!(file_ok(&f, None, None));
        assert!(file_ok(&f, None, Some(5)));
        assert!(!file_ok(&f, None, Some(6)));
        assert!(file_ok(&f, Some("5d41402abc4b2a76b9719d911017c592"), None)); // md5("hello")
        assert!(!file_ok(&f, Some("00000000000000000000000000000000"), None));
        assert!(!file_ok(&d.join("missing"), None, None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fmt_eta_reads_sane() {
        assert_eq!(fmt_eta(45), "~45s left");
        assert_eq!(fmt_eta(150), "~2m 30s left");
    }
}
