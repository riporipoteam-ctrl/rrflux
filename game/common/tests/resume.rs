// Resume test: needs the range-capable server from /tmp/resume_test/range_server.py
// running on 127.0.0.1:8472 serving /tmp/resume_test/srv/big.bin (5 MiB).
// Run: python3 /tmp/resume_test/range_server.py &  then  cargo test --test resume
use fluxrec_common::download::{download_files, DownloadOptions};
use fluxrec_common::manifest::FileEntry;

fn sha256_file(p: &std::path::Path) -> String {
    use sha2::Digest as _;
    let f = std::fs::File::open(p).unwrap();
    let mut r = std::io::BufReader::new(f);
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut r, &mut h).unwrap();
    hex::encode(h.finalize())
}

#[tokio::test]
async fn resume_continues_partial_file() {
    let srv = std::path::Path::new("/tmp/resume_test/srv/big.bin");
    if !srv.exists() {
        eprintln!("SKIP: range server fixture missing");
        return;
    }
    let dir = std::path::PathBuf::from("/tmp/resume_test/dl");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Simulate an interrupted download: first 2 MiB already on disk as .part.
    let full = std::fs::read(srv).unwrap();
    let part = dir.join("big.bin.part");
    std::fs::write(&part, &full[..2 * 1024 * 1024]).unwrap();

    let entry = FileEntry {
        path: "big.bin".to_string(),
        sha256: sha256_file(srv),
        url: "http://127.0.0.1:8472/big.bin".to_string(),
        size: Some(full.len() as u64),
    };
    let client = reqwest::Client::new();
    let (bytes, fetched) = download_files(
        &client,
        &[entry],
        &dir,
        &DownloadOptions::default(),
        |_| {},
    )
    .await
    .expect("download with resume failed");

    assert_eq!(fetched, 1);
    assert_eq!(bytes, full.len() as u64, "byte count should be the full file");
    let got = std::fs::read(dir.join("big.bin")).unwrap();
    assert_eq!(got, full, "resumed file must be byte-identical");
    assert!(!part.exists(), ".part must be renamed away on success");
}

#[tokio::test]
async fn full_download_when_no_part_exists() {    let srv = std::path::Path::new("/tmp/resume_test/srv/big.bin");
    if !srv.exists() {
        eprintln!("SKIP: range server fixture missing");
        return;
    }
    let dir = std::path::PathBuf::from("/tmp/resume_test/dl2");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let full = std::fs::read(srv).unwrap();
    let entry = FileEntry {
        path: "big.bin".to_string(),
        sha256: sha256_file(srv),
        url: "http://127.0.0.1:8472/big.bin".to_string(),
        size: Some(full.len() as u64),
    };
    let client = reqwest::Client::new();
    download_files(&client, &[entry], &dir, &DownloadOptions::default(), |_| {})
        .await
        .expect("clean download failed");
    let got = std::fs::read(dir.join("big.bin")).unwrap();
    assert_eq!(got, full);
}

#[tokio::test]
async fn restarts_clean_when_server_ignores_range() {
    // python3 -m http.server answers 200 (not 206) to Range requests: the
    // downloader must drop the stale part and download whole, never append
    // a full body onto the part.
    let srv = std::path::Path::new("/tmp/resume_test/srv/big.bin");
    if !srv.exists() {
        eprintln!("SKIP: fixture missing");
        return;
    }
    let dir = std::path::PathBuf::from("/tmp/resume_test/dl3");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let full = std::fs::read(srv).unwrap();
    std::fs::write(dir.join("big.bin.part"), &full[..1024]).unwrap(); // stale part

    let entry = FileEntry {
        path: "big.bin".to_string(),
        sha256: sha256_file(srv),
        url: "http://127.0.0.1:8471/big.bin".to_string(), // no Range support
        size: Some(full.len() as u64),
    };
    let client = reqwest::Client::new();
    download_files(&client, &[entry], &dir, &DownloadOptions::default(), |_| {})
        .await
        .expect("fallback download failed");
    let got = std::fs::read(dir.join("big.bin")).unwrap();
    assert_eq!(got, full, "must be the whole file, not part+whole appended");
}
