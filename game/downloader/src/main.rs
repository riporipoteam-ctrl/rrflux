// fluxrec-download — headless installer helper. Fetches the game manifest,
// downloads every file into the target dir, skips files that already verify
// by sha256, and verifies each download. Progress goes to stdout so the
// installer can show it. Exit 0 on success, 1 with an ERROR line on failure.
//
// Usage: fluxrec-download --manifest <url> --dir <path>

use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const DEFAULT_MANIFEST_URL: &str = "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/manifest.json";
const MANIFEST_URL: &str = match option_env!("FLUXREC_MANIFEST_URL") {
    Some(u) => u,
    None => DEFAULT_MANIFEST_URL,
};

#[derive(serde::Deserialize)]
struct Manifest {
    files: Vec<FileEntry>,
}

#[derive(serde::Deserialize)]
struct FileEntry {
    path: String,
    sha256: String,
    url: String,
}

fn sha256_of(path: &Path) -> Result<String, String> {
    use sha2::Digest as _;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut f, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hex::encode(hasher.finalize()))
}

async fn run(manifest_url: &str, dir: &Path) -> Result<(), String> {
    println!("Fetching game manifest...");
    let text = reqwest::get(manifest_url)
        .await
        .map_err(|e| format!("manifest download failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("manifest unreadable: {e}"))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("bad manifest: {e}"))?;
    println!("{} files to check", manifest.files.len());
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let total = manifest.files.len();
    let mut done = 0usize;
    let mut downloaded_bytes: u64 = 0;
    for (i, f) in manifest.files.iter().enumerate() {
        let dest = dir.join(&f.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // skip files that already verify
        if dest.exists() && !f.sha256.is_empty() {
            if let Ok(h) = sha256_of(&dest) {
                if h.eq_ignore_ascii_case(&f.sha256) {
                    done += 1;
                    continue;
                }
            }
        }
        println!("[{}/{}] {}", i + 1, total, f.path);
        let resp = client
            .get(&f.url)
            .send()
            .await
            .map_err(|e| format!("download failed for {}: {e}", f.path))?;
        if !resp.status().is_success() {
            return Err(format!("download failed for {}: HTTP {}", f.path, resp.status()));
        }
        let mut out = tokio::fs::File::create(&dest)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = resp.bytes_stream();
        let mut next_mark: u64 = 64 * 1024 * 1024;
        let mut file_bytes: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("download failed for {}: {e}", f.path))?;
            out.write_all(&chunk)
                .await
                .map_err(|e| e.to_string())?;
            file_bytes += chunk.len() as u64;
            if file_bytes >= next_mark {
                println!("      ... {:.1} MB", file_bytes as f64 / 1048576.0);
                next_mark += 64 * 1024 * 1024;
            }
        }
        out.flush().await.map_err(|e| e.to_string())?;
        drop(out);
        downloaded_bytes += file_bytes;
        if !f.sha256.is_empty() {
            let h = sha256_of(&dest)?;
            if !h.eq_ignore_ascii_case(&f.sha256) {
                return Err(format!("hash mismatch: {}", f.path));
            }
        }
        done += 1;
    }
    println!(
        "Verified {done}/{total} files ({:.1} GB downloaded this run)",
        downloaded_bytes as f64 / 1073741824.0
    );
    Ok(())
}

fn main() {
    let mut manifest_url = MANIFEST_URL.to_string();
    let mut dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--manifest" => {
                if let Some(v) = args.next() {
                    manifest_url = v;
                }
            }
            "--dir" => dir = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    let dir = dir.unwrap_or_else(|| PathBuf::from("game"));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(run(&manifest_url, &dir)) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
    println!("DONE");
}
