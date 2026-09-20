// Flux Rec local translator — answers the patched 2022 client's Rec Room API
// calls over HTTPS from 127.0.0.1:443 (plus plain HTTP on 127.0.0.1:80 for
// stray calls). No cloud needed for this: Photon relays multiplayer,
// Firebase holds identity/saves, and this tiny server translates between
// the game and Firebase right on the player's PC.
//
// Why local HTTPS works:
//   - Windows lets any user-mode app bind 127.0.0.1:443 (no admin needed).
//   - The client keeps the original https:// scheme and only the hostname
//     is patched to `localhost` (the game's HTTP stack rejects plain
//     http:// URLs with "Invalid URI scheme").
//   - On first run the bootstrap generates a local CA + localhost server
//     cert and installs the CA into the *current user's* Trusted Root
//     store (no admin needed). The game then trusts https://localhost.
//
// Trust model: loopback only, so only local processes can reach it. The
// session is created by a silent anonymous Firebase Auth sign-up (verified
// by Google over HTTPS). LoginWithToken answers from that live session —
// if the game passes the token the bootstrap gave it, it must match.

use axum::{
    extract::{FromRef, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{Json, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Instant,
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub uid: String,
    pub id_token: String,
    pub refresh_token: String,
    pub username: String,
}

/// The signed-in player, if any. Set by the bootstrap's silent auth.
pub type SharedSession = Arc<Mutex<Option<Session>>>;

/// One observed game request, kept for diagnostics. The path is stored
/// WITHOUT the query string — tokens must never be logged or persisted.
#[derive(Clone, Debug)]
pub struct ReqStamp {
    pub method: String,
    pub path: String,
    pub at_epoch: u64,
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Live state of the persistent backend, served on /health so the
/// launcher can verify version, ports, TLS trust, and request activity.
pub struct BackendState {
    pub version: &'static str,
    pub exe_path: PathBuf,
    pub started: Instant,
    pub http_bound: AtomicBool,
    pub https_bound: AtomicBool,
    pub ca_trusted: AtomicBool,
    pub request_count: AtomicU64,
    pub first_request: StdMutex<Option<ReqStamp>>,
    pub last_request: StdMutex<Option<ReqStamp>>,
}

impl BackendState {
    pub fn new(exe_path: PathBuf) -> Self {
        BackendState {
            version: env!("CARGO_PKG_VERSION"),
            exe_path,
            started: Instant::now(),
            http_bound: AtomicBool::new(false),
            https_bound: AtomicBool::new(false),
            ca_trusted: AtomicBool::new(false),
            request_count: AtomicU64::new(0),
            first_request: StdMutex::new(None),
            last_request: StdMutex::new(None),
        }
    }

    fn record_request(&self, method: &str, path: &str) {
        let stamp = ReqStamp {
            method: method.to_string(),
            path: path.to_string(),
            at_epoch: epoch_now(),
        };
        self.request_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut first) = self.first_request.lock() {
            if first.is_none() {
                *first = Some(stamp.clone());
            }
        }
        if let Ok(mut last) = self.last_request.lock() {
            *last = Some(stamp);
        }
    }
}

/// Router state: the auth session plus the backend diagnostics state.
#[derive(Clone)]
struct AppState {
    session: SharedSession,
    backend: Arc<BackendState>,
}

impl FromRef<AppState> for SharedSession {
    fn from_ref(s: &AppState) -> Self {
        s.session.clone()
    }
}

impl FromRef<AppState> for Arc<BackendState> {
    fn from_ref(s: &AppState) -> Self {
        s.backend.clone()
    }
}

// Where request logs go (%LOCALAPPDATA%\FluxRec\translator.log), set by the
// bootstrap. Lets us see exactly which endpoints the game client calls,
// which is how new game-surface endpoints get implemented.
//
// Privacy: only "METHOD /path" is logged — never the query string, so
// login tokens and other credentials can't end up in the log.
static LOG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_log_dir(dir: PathBuf) {
    let _ = LOG_DIR.set(dir);
}

fn log_req(method: &str, path: &str) {
    if let Some(dir) = LOG_DIR.get() {
        use std::io::Write as _;
        let line = format!("{method} {path}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("translator.log"))
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

async fn log_middleware(
    State(backend): State<Arc<BackendState>>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    // Sanitized: path only, never path_and_query (tokens live in queries).
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    backend.record_request(&method, &path);
    log_req(&method, &path);
    next.run(req).await
}

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

async fn health(State(backend): State<Arc<BackendState>>) -> (StatusCode, Json<Value>) {
    let stamp = |o: &StdMutex<Option<ReqStamp>>| {
        o.lock().ok().and_then(|g| {
            g.as_ref().map(|s| {
                json!({"method": s.method, "path": s.path, "at_epoch": s.at_epoch})
            })
        })
    };
    j(
        StatusCode::OK,
        json!({
            "ok": true,
            "app": "fluxrec",
            "version": backend.version,
            "pid": std::process::id(),
            "exe": backend.exe_path.to_string_lossy(),
            "uptime_secs": backend.started.elapsed().as_secs(),
            "ports": {
                "http_80": backend.http_bound.load(Ordering::Relaxed),
                "https_443": backend.https_bound.load(Ordering::Relaxed),
            },
            "tls": {
                // Local CA installed into the current user's Trusted Root
                // store, so https://localhost validates.
                "local_ca_trusted": backend.ca_trusted.load(Ordering::Relaxed),
            },
            "requests": {
                "count": backend.request_count.load(Ordering::Relaxed),
                "first": stamp(&backend.first_request),
                "last": stamp(&backend.last_request),
            },
        }),
    )
}

/// POST /shutdown — loopback-only. Lets the launcher retire an outdated
/// backend before starting a new one. Responds first, exits shortly after.
async fn shutdown() -> (StatusCode, Json<Value>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        std::process::exit(0);
    });
    j(StatusCode::OK, json!({"ok": true, "shutting_down": true}))
}

async fn login_with_token(
    State(session): State<SharedSession>,
    Query(q): Query<LoginQuery>,
) -> (StatusCode, Json<Value>) {
    // HANDCRAFTED / UNVERIFIED SCHEMA: the exact November 2022
    // Account/LoginWithToken response shape is unknown (no reference
    // capture exists). This returns a plausible typed object
    // (playerId/username/authToken/expiresIn); verify against a real
    // client capture and correct it if the game misbehaves here.
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

async fn versioncheck(uri: axum::http::Uri) -> (StatusCode, Json<Value>) {
    // /api/versioncheck/islandedversions -> []
    if uri.path().ends_with("islandedversions") {
        return j(StatusCode::OK, json!([]));
    }
    // Rec Room 2022 expects PascalCase fields. VersionStatus: 0 = current.
    j(StatusCode::OK, json!({
        "VersionStatus": 0,
        "UpdateNotificationStage": 0,
        "IsVersionIslanded": false,
        "IsCrossPlayDisabled": false
    }))
}

async fn config(uri: axum::http::Uri) -> (StatusCode, Json<Value>) {
    // Route by specific config endpoint. The client crashes on {} for these.
    let path = uri.path();
    let body = if path.ends_with("v1/amplitude") {
        json!({
            "AmplitudeKey": "",
            "UseRudderStack": false,
            "RudderStackKey": "",
            "UseStatSig": false,
            "StatSigKey": "",
            "StatSigEnvironment": 0
        })
    } else if path.ends_with("v1/azurespeech") {
        json!({
            "Key": "",
            "Region": "eastus",
            "Enabled": false
        })
    } else if path.ends_with("v1/backtrace") {
        json!({
            "ReportBudget": 125,
            "FilterType": 0,
            "SampleRate": 1,
            "LogLineCount": 50,
            "CaptureNativeCrashes": 0,
            "AMRThresholdMS": 0,
            "MessageCount": 1000,
            "MessageRegex": "^.*$",
            "VersionRegex": ".*"
        })
    } else if path.ends_with("v2") {
        json!({
            "LevelProgressionMaps": [{"Level": 0, "RequiredXp": 0, "GiftRarity": -1}],
            "DailyObjectives": [],
            "ServerMaintenance": {"StartsInMinutes": 0},
            "AutoMicMutingConfig": {
                "MicSpamVolumeThreshold": 0.75,
                "MicVolumeSampleInterval": 0.25,
                "MicVolumeSampleRollingWindowLength": 10,
                "MicSpamSamplePercentageForWarning": 0.8,
                "MicSpamSamplePercentageForWarningToEnd": 0.2,
                "MicSpamSamplePercentageForForceMute": 0.8,
                "MicSpamSamplePercentageForForceMuteToEnd": 0.2,
                "MicSpamWarningStateVolumeMultiplier": 0.25
            },
            "ShareBaseUrl": "https://localhost/{0}",
            "StorefrontConfig": {
                "MinPlayerLevelForGifting": 5,
                "LatestStoreBadgeDateTime": "2020-01-01T00:00:00Z"
            },
            "ConfigTable": [],
            "PhotonConfig": {
                "CloudRegion": "us",
                "CrcCheckEnabled": false,
                "EnableServerTracingAfterDisconnect": false
            }
        })
    } else {
        json!({})
    };
    j(StatusCode::OK, body)
}

async fn gameconfigs() -> (StatusCode, Json<Value>) {
    j(StatusCode::OK, json!({}))
}

async fn statsig() -> (StatusCode, Json<Value>) {
    j(StatusCode::OK, json!({"success": true}))
}

async fn voice_config() -> (StatusCode, Json<Value>) {
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
    // Return 200 with empty object instead of 404. The 2022 client may
    // crash on unexpected 404s; empty 200 is safer for unimplemented
    // endpoints (friends, rooms, store, etc.).
    j(StatusCode::OK, json!({}))
}

/// Bind 127.0.0.1:443 (HTTPS) and 127.0.0.1:80 (HTTP) and serve forever.
/// Sends Ok(()) on `ready` once both sockets are bound, or Err with the
/// bind/cert failure. `data_dir` is %LOCALAPPDATA%\FluxRec — certs live in
/// `<data_dir>/certs` so the CA stays stable across runs.
/// Bound ports and CA trust are recorded into `state` for /health.
pub async fn serve(
    session: SharedSession,
    data_dir: PathBuf,
    state: Arc<BackendState>,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) {
    // Rustls 0.23 needs an explicit crypto provider. Install ring as the
    // process default before any TLS usage (axum-server's RustlsConfig
    // panics without this).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let app_state = AppState {
        session,
        backend: state.clone(),
    };
    // NOTE: axum 0.7 wildcard syntax is `/*rest` (`{*rest}` is 0.8+ and
    // panics here at startup, which used to kill the local server before
    // it could signal ready).
    let app = Router::new()
        .route("/health", get(health))
        .route("/shutdown", post(shutdown))
        .route("/Account/LoginWithToken", get(login_with_token))
        .route("/api/versioncheck/*rest", get(versioncheck))
        .route("/api/config/*rest", get(config))
        .route("/api/gameconfigs/v1/all", get(gameconfigs))
        .route("/statsigUserProperties", post(statsig))
        .route("/voice/config", get(voice_config))
        .route("/api/players/v2/me", get(player_me))
        .route("/api/sanitize/*rest", get(sanitize))
        // Telemetry sink: the patched client sends Amplitude traffic to
        // https://localhost/httpapi and /identify (harmless either way).
        .route("/httpapi", get(telemetry).post(telemetry))
        .route("/identify", get(telemetry).post(telemetry))
        .route("/api/*rest", get(game_fallback).post(game_fallback))
        .fallback(get(game_fallback))
        .layer(axum::middleware::from_fn_with_state(
            app_state.backend.clone(),
            log_middleware,
        ))
        .with_state(app_state);

    // TLS certs first — without them there is no https://localhost.
    let cert_dir = data_dir.join("certs");
    let (cert_pem, key_pem) = match ensure_certs(&cert_dir) {
        Ok(p) => p,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    state.ca_trusted.store(true, Ordering::Relaxed);

    // Plain HTTP on :80 for stray calls. Fail-soft: the game only needs
    // HTTPS, so a squatted port 80 just means http_80=false in /health.
    let http_app = app.clone();
    match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 80))).await {
        Ok(l) => {
            state.http_bound.store(true, Ordering::Relaxed);
            tokio::spawn(async move {
                let _ = axum::serve(l, http_app).await;
            });
        }
        Err(e) => {
            log_req("WARN", &format!("cannot bind 127.0.0.1:80 ({e})"));
        }
    }

    // HTTPS on :443 — this is what the game actually uses. `localhost`
    // resolves to ::1 first on most systems, so listen on both IPv4 and
    // IPv6 loopback. Bind the std listeners first so a squatted port
    // surfaces as an error here.
    let tls = match axum_server::tls_rustls::RustlsConfig::from_pem(cert_pem, key_pem).await {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(format!("invalid TLS cert ({e})")));
            return;
        }
    };
    let mut tls_tasks = Vec::new();
    let mut bound_any = false;
    for addr in [
        SocketAddr::from(([127, 0, 0, 1], 443)),
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 443)),
    ] {
        match std::net::TcpListener::bind(addr) {
            Ok(l) => {
                // The explicit ::1 bind covers systems where the socket
                // isn't dual-stack.
                let tls = tls.clone();
                let app = app.clone();
                tls_tasks.push(tokio::spawn(async move {
                    let server = axum_server::tls_rustls::from_tcp_rustls(l, tls);
                    let _ = server.serve(app.into_make_service()).await;
                }));
                bound_any = true;
            }
            Err(e) => {
                // IPv6 loopback may not exist; IPv4 is the one that matters.
                log_req("WARN", &format!("cannot bind {addr} ({e})"));
            }
        }
    }
    if !bound_any {
        let _ = ready.send(Err("cannot bind 127.0.0.1:443 (in use?)".into()));
        return;
    }
    state.https_bound.store(true, Ordering::Relaxed);
    let _ = ready.send(Ok(()));
    for t in tls_tasks {
        let _ = t.await;
    }
}

/// Ensure `<cert_dir>/ca.pem` (local CA) + `server.pem`/`server-key.pem`
/// exist, installing the CA into the current user's Trusted Root store on
/// first creation. Returns (server_cert_pem, server_key_pem).
fn ensure_certs(cert_dir: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    let ca_pem_path = cert_dir.join("ca.pem");
    let srv_pem_path = cert_dir.join("server.pem");
    let srv_key_path = cert_dir.join("server-key.pem");
    let installed_marker = cert_dir.join("ca-installed");

    // If certs exist, ensure the CA is trusted (marker may be missing if
    // a previous run generated certs but failed to install trust).
    if ca_pem_path.exists() && srv_pem_path.exists() && srv_key_path.exists() {
        if !installed_marker.exists() {
            install_ca_trust(&ca_pem_path)?;
            let _ = std::fs::write(&installed_marker, b"1");
        }
        let cert_pem = std::fs::read(&srv_pem_path).map_err(|e| format!("read server.pem: {e}"))?;
        let key_pem = std::fs::read(&srv_key_path).map_err(|e| format!("read server-key.pem: {e}"))?;
        return Ok((cert_pem, key_pem));
    }
    std::fs::create_dir_all(cert_dir).map_err(|e| format!("create cert dir: {e}"))?;

    let (ca_pem, srv_pem, srv_key_pem) = generate_certs().map_err(|e| format!("generate certs: {e}"))?;
    std::fs::write(&ca_pem_path, &ca_pem).map_err(|e| format!("write ca.pem: {e}"))?;
    std::fs::write(&srv_pem_path, &srv_pem).map_err(|e| format!("write server.pem: {e}"))?;
    std::fs::write(&srv_key_path, &srv_key_pem).map_err(|e| format!("write server-key.pem: {e}"))?;

    if !installed_marker.exists() {
        install_ca_trust(&ca_pem_path)?;
        let _ = std::fs::write(&installed_marker, b"1");
    }
    Ok((srv_pem.into_bytes(), srv_key_pem.into_bytes()))
}

/// Generate a local CA and a localhost server cert signed by it.
/// Returns (ca_pem, server_pem, server_key_pem).
fn generate_certs() -> Result<(String, String, String), String> {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, SanType};
    use std::net::IpAddr;

    let ca_key = KeyPair::generate().map_err(|e| format!("ca key: {e}"))?;
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Flux Rec Local CA");
    ca_params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    ca_params.not_after = rcgen::date_time_ymd(2046, 1, 1);
    let ca = ca_params
        .self_signed(&ca_key)
        .map_err(|e| format!("ca: {e}"))?;

    let srv_key = KeyPair::generate().map_err(|e| format!("server key: {e}"))?;
    let mut srv_params = CertificateParams::new(vec!["localhost".to_string()])
        .map_err(|e| format!("server params: {e}"))?;
    srv_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    srv_params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::from([127, 0, 0, 1])));
    srv_params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    srv_params.not_after = rcgen::date_time_ymd(2046, 1, 1);
    let srv = srv_params
        .signed_by(&srv_key, &ca, &ca_key)
        .map_err(|e| format!("server: {e}"))?;

    Ok((ca.pem(), srv.pem(), srv_key.serialize_pem()))
}

/// Install the CA into the current user's Trusted Root store (no admin
/// needed on Windows). Non-Windows builds skip this — the test harness
/// installs the CA into the Wine prefix manually.
#[cfg(windows)]
fn install_ca_trust(ca_pem: &Path) -> Result<(), String> {
    let out = std::process::Command::new("certutil")
        .args([
            "-user",
            "-addstore",
            "-f",
            "Root",
            &ca_pem.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("couldn't run certutil: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Flux Rec couldn't trust its local game server.\n\n\
             certutil said: {}\n\n\
             Try running Flux Rec once as administrator, then normally.",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn install_ca_trust(_ca_pem: &Path) -> Result<(), String> {
    Ok(())
}
