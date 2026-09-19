// Flux Rec launcher backend — download, verify, sign in, translate, launch.
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

/// Where crash diagnostics go: %LOCALAPPDATA%\FluxRec\crash.log
/// (falls back to the temp dir). The launcher runs with
/// windows_subsystem, so panics are silent — this file is how
/// we see them.
fn crash_log_path() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("FluxRec")
        .join("crash.log")
}

fn append_crash_log(line: &str) {
    let path = crash_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// Install a panic hook that writes every panic to the crash log,
/// then mark startup so we can tell "crashed" apart from "killed
/// externally (antivirus?)".
fn init_crash_logging() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    append_crash_log(&format!("--- Flux Rec launcher start (epoch {ts}) ---"));
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "?".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        append_crash_log(&format!("PANIC at {loc}: {payload}"));
        eprintln!("Flux Rec launcher crashed: {payload} ({loc})");
    }));
}

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
    Ok(base.join("FluxRec").join("game"))
}

fn dirs_data_dir() -> Option<PathBuf> {
    // Prefer the user's own profile: a per-machine install runs the app
    // as a standard user, who cannot write to PROGRAMDATA.
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("APPDATA").map(PathBuf::from).ok())
        .or_else(|| std::env::var("PROGRAMDATA").map(PathBuf::from).ok())
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
    let mut child = std::process::Command::new(&exe)
        .args(&extra_args)
        .current_dir(game_dir()?)
        .spawn()
        .map_err(|e| e.to_string())?;
    // If the game dies within 3s, report it instead of pretending it launched.
    // (Suspects for an instant exit: the EAC stub DLL, Steam checks, translator.)
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    match child.try_wait().map_err(|e| e.to_string())? {
        Some(status) => {
            let msg = format!("game exited immediately (status: {status})");
            append_crash_log(&msg);
            Err(msg)
        }
        None => Ok(()),
    }
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
async fn translator_status(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.translator_ok.load(Ordering::SeqCst))
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
    let refresh_token = body["refreshToken"].as_str().unwrap_or("").to_string();
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
        refresh_token,
        username: username.clone(),
    });
    Ok(serde_json::json!({ "uid": uid, "username": username }))
}

/// Firebase ID tokens expire after 1 hour. This loop wakes every 50 minutes
/// and swaps the stored refresh token for a fresh ID token, so long
/// play sessions (and the translator's Firestore calls) keep working.
/// Failures are silent — worst case the user signs in again.
async fn refresh_loop(session: SharedSession) {
    let api_key = env!("FIREBASE_WEB_API_KEY");
    let url = format!("https://securetoken.googleapis.com/v1/token?key={api_key}");
    let client = reqwest::Client::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(50 * 60)).await;
        let rt = session
            .lock()
            .await
            .as_ref()
            .map(|s| s.refresh_token.clone())
            .unwrap_or_default();
        if rt.is_empty() {
            continue;
        }
        let form = serde_urlencoded::to_string([
            ("grant_type", "refresh_token"),
            ("refresh_token", rt.as_str()),
        ]);
        let Ok(form) = form else { continue };
        let Ok(resp) = client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
        else {
            continue;
        };
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        if let (Some(id), Some(new_rt)) = (
            body.get("id_token").and_then(|v| v.as_str()),
            body.get("refresh_token").and_then(|v| v.as_str()),
        ) {
            if let Some(s) = session.lock().await.as_mut() {
                s.id_token = id.to_string();
                s.refresh_token = new_rt.to_string();
            }
        }
    }
}

fn main() {
    init_crash_logging();
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
            // Clone before the first spawn moves `session`.
            let session_for_refresh = session.clone();
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
            // Keep the Firebase ID token fresh (it expires hourly).
            tokio::spawn(async move {
                refresh_loop(session_for_refresh).await;
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
        .expect("error while running Flux Rec launcher");
}
