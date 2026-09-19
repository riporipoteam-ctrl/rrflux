// Silent auth for Flux Rec: no login screen. On first run the bootstrap
// creates an anonymous Firebase account (one HTTPS call to Google), caches
// its refresh token in %LOCALAPPDATA%\FluxRec\session.json, and refreshes
// the hourly ID token in the background. The player never sees any of this.
//
// NOTE: the Firebase console must have the Anonymous sign-in provider
// enabled (Authentication -> Sign-in method -> Anonymous).

use crate::translator::{Session, SharedSession};
use std::path::{Path, PathBuf};

// Baked in at build time from the FIREBASE_WEB_API_KEY secret. This key
// is public by design (it ships in every Firebase web app); the account
// itself is protected by Auth + Firestore rules.
const API_KEY: &str = env!("FIREBASE_WEB_API_KEY");

#[derive(serde::Deserialize)]
struct AuthResp {
    #[serde(rename = "localId", default)]
    local_id: String,
    #[serde(rename = "idToken", default)]
    id_token: String,
    #[serde(rename = "refreshToken", default)]
    refresh_token: String,
}

#[derive(serde::Deserialize)]
struct RefreshResp {
    #[serde(rename = "id_token", default)]
    id_token: String,
    #[serde(rename = "refresh_token", default)]
    refresh_token: String,
    #[serde(rename = "user_id", default)]
    user_id: String,
}

fn api_error(body: &serde_json::Value) -> Option<String> {
    body.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

fn username_for(uid: &str) -> String {
    let tag: String = uid.chars().take(6).collect::<String>().to_uppercase();
    format!("Player-{tag}")
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .map_err(|e| e.to_string())
}

/// Anonymous sign-up: POST accounts:signUp with no credentials creates an
/// anonymous Firebase user and returns its tokens.
async fn signup_anonymous(c: &reqwest::Client) -> Result<Session, String> {
    let url = format!("https://identitytoolkit.googleapis.com/v1/accounts:signUp?key={API_KEY}");
    let body: serde_json::Value = c
        .post(&url)
        .json(&serde_json::json!({ "returnSecureToken": true }))
        .send()
        .await
        .map_err(|e| format!("auth unreachable: {e}"))?
        .json()
        .await
        .map_err(|e| format!("auth gave an unreadable response: {e}"))?;
    if let Some(m) = api_error(&body) {
        return Err(if m == "OPERATION_NOT_ALLOWED" {
            "anonymous sign-in is switched off in the Firebase console \
             (Authentication -> Sign-in method -> Anonymous -> Enable)"
                .to_string()
        } else {
            format!("sign-up failed: {m}")
        });
    }
    let r: AuthResp = serde_json::from_value(body).map_err(|e| e.to_string())?;
    if r.local_id.is_empty() || r.id_token.is_empty() || r.refresh_token.is_empty() {
        return Err("auth returned an incomplete session".into());
    }
    Ok(Session {
        username: username_for(&r.local_id),
        uid: r.local_id,
        id_token: r.id_token,
        refresh_token: r.refresh_token,
    })
}

async fn refresh(c: &reqwest::Client, rt: &str) -> Result<Session, String> {
    let url = format!("https://securetoken.googleapis.com/v1/token?key={API_KEY}");
    let body: serde_json::Value = c
        .post(&url)
        .form(&[("grant_type", "refresh_token"), ("refresh_token", rt)])
        .send()
        .await
        .map_err(|e| format!("auth unreachable: {e}"))?
        .json()
        .await
        .map_err(|e| format!("auth gave an unreadable response: {e}"))?;
    if let Some(m) = api_error(&body) {
        return Err(format!("token refresh failed: {m}"));
    }
    let r: RefreshResp = serde_json::from_value(body).map_err(|e| e.to_string())?;
    if r.user_id.is_empty() || r.id_token.is_empty() || r.refresh_token.is_empty() {
        return Err("refresh returned an incomplete session".into());
    }
    Ok(Session {
        username: username_for(&r.user_id),
        uid: r.user_id,
        id_token: r.id_token,
        refresh_token: r.refresh_token,
    })
}

fn save_cache(data_dir: &Path, s: &Session) {
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(
        data_dir.join("session.json"),
        serde_json::json!({ "refresh_token": s.refresh_token }).to_string(),
    );
}

/// Get a working session: reuse the cached account when possible, otherwise
/// create a fresh anonymous one. Never shows UI.
pub async fn ensure_session(data_dir: &Path) -> Result<Session, String> {
    let c = client()?;
    if let Ok(raw) = std::fs::read_to_string(data_dir.join("session.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(rt) = v.get("refresh_token").and_then(|x| x.as_str()) {
                if let Ok(s) = refresh(&c, rt).await {
                    save_cache(data_dir, &s);
                    return Ok(s);
                }
                // cached token dead (revoked/expired) — fall through to a
                // fresh anonymous account
            }
        }
    }
    let s = signup_anonymous(&c).await?;
    save_cache(data_dir, &s);
    Ok(s)
}

/// Firebase ID tokens expire after 1 hour. Wakes every 50 minutes and swaps
/// the stored refresh token for a fresh ID token, so long play sessions
/// keep working. Failures are silent — worst case the next launch re-signs.
pub async fn refresh_loop(session: SharedSession, data_dir: PathBuf) {
    let Ok(c) = client() else { return };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(50 * 60)).await;
        let rt = session
            .lock()
            .await
            .as_ref()
            .map(|s| s.refresh_token.clone());
        if let Some(rt) = rt {
            if let Ok(s) = refresh(&c, &rt).await {
                save_cache(&data_dir, &s);
                *session.lock().await = Some(s);
            }
        }
    }
}
