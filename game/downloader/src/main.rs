// fluxrec-download — installer download helper. Fetches the game manifest
// and brings the target dir up to date: parallel downloads, resume on
// retry, hash verification, and no sleeping mid-download. Progress shows
// in a small window (plus stdout for the installer log).
// Exit 0 on success, 1 with an ERROR line on failure.
//
// Usage: fluxrec-download --manifest <url> --dir <path> [--state-dir <path>]

use fluxrec_common::update::update_game_files;
use fluxrec_common::MANIFEST_URL;
use std::path::PathBuf;

async fn run(manifest_url: &str, dir: &PathBuf, state_dir: &PathBuf) -> Result<(), String> {
    println!("Checking game files...");
    let outcome = update_game_files(manifest_url, dir, state_dir, "Flux Rec Setup").await?;
    println!(
        "Game files up to date ({} downloaded, {} removed).",
        outcome.downloaded, outcome.deleted
    );
    Ok(())
}

fn main() {
    let mut manifest_url = MANIFEST_URL.to_string();
    let mut dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--manifest" => {
                if let Some(v) = args.next() {
                    manifest_url = v;
                }
            }
            "--dir" => dir = args.next().map(PathBuf::from),
            "--state-dir" => state_dir = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    let dir = dir.unwrap_or_else(|| PathBuf::from("game"));
    let state_dir = state_dir.unwrap_or_else(|| {
        dir.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(run(&manifest_url, &dir, &state_dir)) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
    println!("DONE");
}
