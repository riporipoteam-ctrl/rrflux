// RRFlux Launcher backend — download, verify, sign in, translate, launch.
// Build on Windows: `tauri build` (or `cargo tauri build`).
// Requires FIREBASE_WEB_API_KEY in the build environment (GitHub secret).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod translator;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Manager, State, Window};
use tokio::io::AsyncWriteExt;
use translator::SharedSession;

#[derive(Debug, Deserialize)]
struct ManifestFile {
    path: String,
    sha256: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Serialize, Clone)]
struct Progress {
    file: String,
    downloaded: u64,
    total: u64,
    files_done: usize,
    files_total: usize,
}

fn game_dir() -> Result<PathBuf, String> {
    let base = dirs_data_dir().ok_or("could not resolve app data dir")?;
    Ok(base.join("RRFlux").join("game"))
}

fn dirs_data_dir() -> Option<PathBuf> {
    std::env::var("PROGRAMDATA")
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("APPDATA").map(PathBuf::from).ok())
}

async fn sha256_of(path: &PathBuf) -> Result<String, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        use tokio::io::AsyncReadExt;
        let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[tauri::command]
async fn fetch_manifest(window: Window, manifest_url: String) -> Result<String, String> {
    let text = reqwest::get(&manifest_url)
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    window
        .emit("manifest", serde_json::json!({
            "version": manifest.version,
            "files": manifest.files.len(),
        }))
        .map_err(|e| e.to_string())?;
    Ok(text)
}

#[tauri::command]
async fn download_game(window: Window, manifest_json: String) -> Result<(), String> {
    let manifest: Manifest =
        serde_json::from_str(&manifest_json).map_err(|e| e.to_string())?;
    let dir = game_dir()?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;

    let total_files = manifest.files.len();
    for (i, f) in manifest.files.iter().enumerate() {
        let dest = dir.join(&f.path);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
        }
        // skip files that already verify
        if dest.exists() {
            if let Ok(h) = sha256_of(&dest).await {
                if h.eq_ignore_ascii_case(&f.sha256) {
                    window
                        .emit("progress", Progress {
                            file: f.path.clone(), downloaded: 0, total: 0,
                            files_done: i + 1, files_total: total_files,
                        })
                        .map_err(|e| e.to_string())?;
                    continue;
                }
            }
        }
        let resp = reqwest::get(&f.url).await.map_err(|e| e.to_string())?;
        let total = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();
        let mut out = tokio::fs::File::create(&dest)
            .await
            .map_err(|e| e.to_string())?;
        let mut downloaded: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            out.write_all(&chunk).await.map_err(|e| e.to_string())?;
            downloaded += chunk.len() as u64;
            window
                .emit("progress", Progress {
                    file: f.path.clone(), downloaded, total,
                    files_done: i, files_total: total_files,
                })
                .map_err(|e| e.to_string())?;
        }
        out.flush().await.map_err(|e| e.to_string())?;
        drop(out);
        let h = sha256_of(&dest).await?;
        if !h.eq_ignore_ascii_case(&f.sha256) {
            return Err(format!("hash mismatch: {}", f.path));
        }
        window
            .emit("progress", Progress {
                file: f.path.clone(), downloaded: total, total,
                files_done: i + 1, files_total: total_files,
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn launch_game(extra_args: Vec<String>) -> Result<(), String> {
    let exe = game_dir()?.join("RecRoom.exe");
    if !exe.exists() {
        return Err("game not installed yet".into());
    }
    std::process::Command::new(&exe)
        .args(&extra_args)
        .current_dir(game_dir()?)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn game_installed() -> bool {
    game_dir()
        .map(|d| d.join("RecRoom.exe").exists())
        .unwrap_or(false)
}

/// Shared launcher state: the signed-in session and translator health.
struct AppState {
    session: SharedSession,
    translator_ok: AtomicBool,
}

#[tauri::command]
async fn translator_status(state: State<'_, AppState>) -> bool {
    state.translator_ok.load(Ordering::SeqCst)
}

/// Real Firebase Auth sign-in (email/password) via the public Identity
/// Toolkit REST API. The returned ID token feeds the local translator and
/// (later) Firestore calls made as the player.
#[tauri::command]
async fn sign_in(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<serde_json::Value, String> {
    // Baked in at build time from the FIREBASE_WEB_API_KEY secret. This key
    // is public by design (it ships in every Firebase web app); the account
    // itself is protected by Auth + Firestore rules.
    let api_key = env!("FIREBASE_WEB_API_KEY");
    let url = format!(
        "https://identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key={api_key}"
    );
    let body: serde_json::Value = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "email": email,
            "password": password,
            "returnSecureToken": true,
        }))
        .send()
        .await
        .map_err(|e| format!("auth request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("auth response unreadable: {e}"))?;
    if let Some(msg) = body
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(match msg {
            "EMAIL_NOT_FOUND" | "INVALID_PASSWORD" | "INVALID_LOGIN_CREDENTIALS" => {
                "wrong email or password".to_string()
            }
            "USER_DISABLED" => "this account is disabled".to_string(),
            _ => format!("sign-in failed: {msg}"),
        });
    }
    let uid = body["localId"].as_str().unwrap_or("").to_string();
    let id_token = body["idToken"].as_str().unwrap_or("").to_string();
    let username = body
        .get("displayName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            email
                .split('@')
                .next()
                .unwrap_or("player")
                .to_string()
        });
    if uid.is_empty() || id_token.is_empty() {
        return Err("auth returned an incomplete session".into());
    }
    *state.session.lock().await = Some(translator::Session {
        uid: uid.clone(),
        id_token,
        username: username.clone(),
    });
    Ok(serde_json::json!({ "uid": uid, "username": username }))
}

fn main() {
    let session: SharedSession = Default::default();
    let state = AppState {
        session: session.clone(),
        translator_ok: AtomicBool::new(false),
    };
    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            // Start the local translator (127.0.0.1:80) in the background.
            // It answers the game's Rec Room API calls using the live
            // Firebase session — no cloud server involved.
            let handle = app.handle().clone();
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                if let Err(e) = translator::serve(session, ready_tx).await {
                    eprintln!("{e}");
                }
            });
            tokio::spawn(async move {
                let ok = ready_rx.await.is_ok();
                if let Some(s) = handle.try_state::<AppState>() {
                    s.translator_ok.store(ok, Ordering::SeqCst);
                }
                if !ok {
                    eprintln!("translator failed to start (is port 80 busy?)");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            fetch_manifest,
            download_game,
            launch_game,
            game_installed,
            sign_in,
            translator_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running RRFlux launcher");
}
