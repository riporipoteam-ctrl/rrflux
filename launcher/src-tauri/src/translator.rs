// RRFlux local translator — answers the patched 2022 client's Rec Room API
// calls from 127.0.0.1:80. No cloud needed for this: Photon relays
// multiplayer, Firebase holds identity/saves, and this tiny server translates
// between the game and Firebase right on the player's PC.
//
// Why local works:
//   - Windows lets any user-mode app bind 127.0.0.1:80 (no admin needed).
//   - "127.0.0.1" (9 chars) fits the client's <=12-char auth hostname slot.
//   - The patcher downgrades the endpoint scheme https:// -> http://.
//
// Trust model: loopback only, so only local processes can reach it. The
// session is created by the launcher's real Firebase Auth sign-in (verified
// by Google over HTTPS). LoginWithToken answers from that live session —
// if the game passes the token the launcher gave it, it must match.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub uid: String,
    pub id_token: String,
    pub username: String,
}

/// The signed-in player, if any. Set by the `sign_in` Tauri command.
pub type SharedSession = Arc<Mutex<Option<Session>>>;

#[derive(Deserialize)]
#[allow(non_snake_case)] // field names must match the game's query params
struct LoginQuery {
    #[serde(default)]
    loginToken: String,
    #[serde(default)]
    accountId: String,
}

fn j<T: serde::Serialize>(status: StatusCode, v: T) -> (StatusCode, Json<Value>) {
    (status, Json(json!(v)))
}

async fn login_with_token(
    State(session): State<SharedSession>,
    Query(q): Query<LoginQuery>,
) -> (StatusCode, Json<Value>) {
    let guard = session.lock().await;
    let Some(s) = guard.as_ref() else {
        return j(StatusCode::UNAUTHORIZED, json!({"error": "not signed in"}));
    };
    // If the game echoes the token, it must match the live session.
    // If it sends none, local trust applies: answer from the session.
    if !q.loginToken.is_empty() && q.loginToken != s.id_token {
        return j(StatusCode::UNAUTHORIZED, json!({"error": "bad loginToken"}));
    }
    let _ = q.accountId; // accepted for shape-compat; uid comes from session
    j(
        StatusCode::OK,
        json!({
            "playerId": s.uid,
            "username": s.username,
            "authToken": s.id_token,
            "expiresIn": 3600,
        }),
    )
}

async fn versioncheck() -> (StatusCode, Json<Value>) {
    j(StatusCode::OK, json!({"ok": true, "updateRequired": false}))
}

async fn config() -> (StatusCode, Json<Value>) {
    j(StatusCode::OK, json!({}))
}

async fn player_me(
    State(session): State<SharedSession>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let guard = session.lock().await;
    let Some(s) = guard.as_ref() else {
        return j(StatusCode::UNAUTHORIZED, json!({"error": "not signed in"}));
    };
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Accept the session token, or no header at all (local trust).
    if !auth.is_empty() && auth != format!("Bearer {}", s.id_token) {
        return j(StatusCode::UNAUTHORIZED, json!({"error": "auth required"}));
    }
    j(
        StatusCode::OK,
        json!({"playerId": s.uid, "username": s.username}),
    )
}

async fn sanitize(Query(q): Query<std::collections::HashMap<String, String>>) -> (StatusCode, Json<Value>) {
    // Private server: no filtering, pass through.
    let text = q.get("text").cloned().unwrap_or_default();
    j(StatusCode::OK, json!({ "text": text }))
}

async fn telemetry() -> (StatusCode, Json<Value>) {
    j(StatusCode::OK, json!({"ok": true}))
}

async fn game_fallback() -> (StatusCode, Json<Value>) {
    // Graceful 404 for the rest of the game surface (friends, rooms, store,
    // …). Implemented as the client proves it needs them.
    j(
        StatusCode::NOT_FOUND,
        json!({"error": "not implemented in RRFlux v1"}),
    )
}

/// Bind 127.0.0.1:80 and serve forever. Sends on `ready` once the socket
/// is bound; returns Err if the port is taken.
pub async fn serve(
    session: SharedSession,
    ready: tokio::sync::oneshot::Sender<()>,
) -> Result<(), String> {
    let app = Router::new()
        .route("/Account/LoginWithToken", get(login_with_token))
        .route("/api/versioncheck/*rest", get(versioncheck))
        .route("/api/config/*rest", get(config))
        .route("/api/players/v2/me", get(player_me))
        .route("/api/sanitize/*rest", get(sanitize))
        .route("/2/httpapi", get(telemetry).post(telemetry))
        .route("/api/*rest", get(game_fallback).post(game_fallback))
        .fallback(get(game_fallback))
        .with_state(session);

    let addr = SocketAddr::from(([127, 0, 0, 1], 80));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("translator: cannot bind 127.0.0.1:80 ({e})"))?;
    let _ = ready.send(());
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("translator error: {e}"))
}
