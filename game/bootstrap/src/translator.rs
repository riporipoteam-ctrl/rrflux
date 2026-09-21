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
    /// DER-encoded CRL for the local CA, served at GET /crl.pem.
    pub crl_der: StdMutex<Option<Vec<u8>>>,
    /// Concise `certutil -verify` verdict (Windows) for /health.
    pub tls_chain_diag: StdMutex<Option<String>>,
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
            crl_der: StdMutex::new(None),
            tls_chain_diag: StdMutex::new(None),
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
                // Concise `certutil -verify` verdict on the leaf cert
                // (Windows), e.g. "chain_ok" / "untrusted_root" /
                // "revocation_unknown". Set shortly after startup.
                "chain_diag": backend
                    .tls_chain_diag
                    .lock()
                    .ok()
                    .and_then(|g| g.clone()),
            },
            "requests": {
                "count": backend.request_count.load(Ordering::Relaxed),
                "first": stamp(&backend.first_request),
                "last": stamp(&backend.last_request),
            },
        }),
    )
}

/// GET /crl.pem — the (empty) CRL for the local CA, DER-encoded. Served on
/// both :80 and :443; the server cert's CRL Distribution Point extension
/// points at http://localhost/crl.pem so Windows chain validation can check
/// revocation instead of failing with RevocationStatusUnknown.
async fn crl_pem(State(backend): State<Arc<BackendState>>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "application/pkix-crl".parse().unwrap(),
    );
    match backend.crl_der.lock().ok().and_then(|g| g.clone()) {
        Some(der) => (StatusCode::OK, headers, der),
        None => (StatusCode::NOT_FOUND, headers, Vec::new()),
    }
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
    // The client deserializes this response into List<GameConfig> — it
    // MUST be a JSON array. Returning {} fails the parse, the client
    // retries the whole connection sequence, then stalls on black.
    j(StatusCode::OK, json!([]))
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

/// Name-server service-discovery document.
/// The patched 2022 client was pointed at https://localhost (bare host),
/// so it issues `GET /` to discover every service URL. Answer with the
/// flat {ServiceName: "https://localhost"} map (36 labels, Dorknet-style).
/// Without this the client has no service URLs and hangs at
/// "Connecting to RecNet".
async fn nameserver() -> (StatusCode, Json<Value>) {
    j(
        StatusCode::OK,
        json!({
            "WWW": "https://localhost",
            "API": "https://localhost",
            "Accounts": "https://localhost",
            "Auth": "https://localhost",
            "BugReporting": "https://localhost",
            "Cards": "https://localhost",
            "CDN": "https://localhost",
            "Chat": "https://localhost",
            "Clubs": "https://localhost",
            "CMS": "https://localhost",
            "Commerce": "https://localhost",
            "Data": "https://localhost",
            "DataCollection": "https://localhost",
            "Discovery": "https://localhost",
            "Econ": "https://localhost",
            "GameLogs": "https://localhost",
            "Geo": "https://localhost",
            "Images": "https://localhost",
            "Leaderboard": "https://localhost",
            "Link": "https://localhost",
            "Lists": "https://localhost",
            "Matchmaking": "https://localhost",
            "Moderation": "https://localhost",
            "NameServer": "https://localhost",
            "Notifications": "https://localhost",
            "PlatformNotifications": "https://localhost",
            "PlayerSettings": "https://localhost",
            "RoomComments": "https://localhost",
            "RoomieIntegrations": "https://localhost",
            "Rooms": "https://localhost",
            "Storage": "https://localhost",
            "Strings": "https://localhost",
            "StringsCDN": "https://localhost",
            "Studio": "https://localhost",
            "Thorn": "https://localhost",
            "Videos": "https://localhost"
        }),
    )
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
        // Name-server discovery: the client does GET https://localhost/
        // (bare host) to learn every service URL. Must come before the
        // fallback so `/` isn't answered with an empty object.
        .route("/", get(nameserver))
        // CRL for the local CA (DER). The server cert's CRL Distribution
        // Point extension points here so Windows revocation checking
        // succeeds instead of failing with RevocationStatusUnknown.
        .route("/crl.pem", get(crl_pem))
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
    let bundle = match ensure_certs(&cert_dir) {
        Ok(b) => b,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // Record the *verified* trust state, not an assumption.
    state.ca_trusted.store(bundle.ca_trusted, Ordering::Relaxed);
    if let Ok(mut g) = state.crl_der.lock() {
        *g = Some(bundle.crl_der);
    }
    // The Showdown client's nameserver query resolves ns.rec.net itself
    // (bypassing the plugin's URL rewrite); without a hosts entry it hangs
    // at "Connecting to server..." on machines where that hostname is dead.
    ensure_hosts_entry();

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
    let tls = match axum_server::tls_rustls::RustlsConfig::from_pem(
        bundle.server_cert_pem,
        bundle.server_key_pem,
    )
    .await
    {
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
    // Windows chain self-diagnostic: `certutil -urlfetch -verify` performs
    // the same chain build (root trust + CRL revocation fetch) the game's
    // TLS stack performs, and its verdict lands in the log and /health.
    // Detached so a slow certutil can never stall serving.
    {
        let diag_state = state.clone();
        let srv_der = cert_dir.join("server.der");
        tokio::spawn(async move {
            run_tls_chain_diag(&srv_der, &diag_state).await;
        });
    }
    for t in tls_tasks {
        let _ = t.await;
    }
}

/// What `ensure_certs` produced for this backend run.
struct CertBundle {
    server_cert_pem: Vec<u8>,
    server_key_pem: Vec<u8>,
    crl_der: Vec<u8>,
    /// Actually verified against the Windows trust store (not assumed).
    ca_trusted: bool,
}

/// Bump when the on-disk cert layout changes; older layouts are
/// regenerated (a new CA key means re-installing trust).
const CERT_FORMAT: &str = "3";

/// Ensure `<cert_dir>/ca.pem` (local CA) + `server.pem`/`server.der`/
/// `server-key.pem` + `crl.der` exist, installing the CA into the current
/// user's Trusted Root store when needed. Returns the bundle plus whether
/// the CA was actually found in the trust store afterwards.
fn ensure_certs(cert_dir: &Path) -> Result<CertBundle, String> {
    let ca_pem_path = cert_dir.join("ca.pem");
    let srv_pem_path = cert_dir.join("server.pem");
    let srv_der_path = cert_dir.join("server.der");
    let srv_key_path = cert_dir.join("server-key.pem");
    let crl_der_path = cert_dir.join("crl.der");
    let format_marker = cert_dir.join("cert-format");
    let installed_marker = cert_dir.join("ca-installed");

    let format_ok = std::fs::read_to_string(&format_marker)
        .map(|s| s.trim() == CERT_FORMAT)
        .unwrap_or(false);
    let all_present = ca_pem_path.exists()
        && srv_pem_path.exists()
        && srv_der_path.exists()
        && srv_key_path.exists()
        && crl_der_path.exists();

    // Reuse existing certs when the layout is current — but VERIFY the CA
    // is really in the trust store instead of trusting the marker. The
    // v0.3.14-era marker could lie after a store wipe or a certutil that
    // silently failed, which is exactly how the game ended up rejecting
    // our cert.
    if format_ok && all_present {
        let ca_trusted = if verify_ca_trust(&ca_pem_path) {
            log_req("TLS", "local CA already trusted in Windows store");
            true
        } else {
            log_req("TLS", "local CA missing from Windows store; installing");
            if let Err(e) = install_ca_trust(&ca_pem_path) {
                log_req("TLS", &format!("CA trust install failed: {e}"));
                return Err(e);
            }
            let _ = std::fs::write(&installed_marker, b"1");
            let ok = verify_ca_trust(&ca_pem_path);
            log_req("TLS", &format!("CA trust installed; verified in store: {ok}"));
            ok
        };
        // The game builds its chain in a machine context, which can't see
        // the user store — also install there (UAC prompt, once per CA).
        ensure_machine_ca_trust(&ca_pem_path, cert_dir);
        let server_cert_pem =
            std::fs::read(&srv_pem_path).map_err(|e| format!("read server.pem: {e}"))?;
        let server_key_pem =
            std::fs::read(&srv_key_path).map_err(|e| format!("read server-key.pem: {e}"))?;
        let crl_der = std::fs::read(&crl_der_path).map_err(|e| format!("read crl.der: {e}"))?;
        return Ok(CertBundle {
            server_cert_pem,
            server_key_pem,
            crl_der,
            ca_trusted,
        });
    }

    // (Re)generate everything. The CA key is not persisted, so a new leaf
    // always comes with a new CA, and trust is re-installed. The leaf
    // carries a CRL Distribution Point (http://localhost/crl.pem) so the
    // Windows revocation check succeeds instead of failing unknown.
    std::fs::create_dir_all(cert_dir).map_err(|e| format!("create cert dir: {e}"))?;

    let (ca_pem, srv_pem, srv_der, srv_key_pem, crl_der) =
        generate_certs().map_err(|e| format!("generate certs: {e}"))?;
    std::fs::write(&ca_pem_path, &ca_pem).map_err(|e| format!("write ca.pem: {e}"))?;
    std::fs::write(&srv_pem_path, &srv_pem).map_err(|e| format!("write server.pem: {e}"))?;
    std::fs::write(&srv_der_path, &srv_der).map_err(|e| format!("write server.der: {e}"))?;
    std::fs::write(&srv_key_path, &srv_key_pem).map_err(|e| format!("write server-key.pem: {e}"))?;
    std::fs::write(&crl_der_path, &crl_der).map_err(|e| format!("write crl.der: {e}"))?;
    let _ = std::fs::write(&format_marker, CERT_FORMAT);

    log_req(
        "TLS",
        "generated new local CA + server cert (with CRL distribution point); installing trust",
    );
    if let Err(e) = install_ca_trust(&ca_pem_path) {
        log_req("TLS", &format!("CA trust install failed: {e}"));
        return Err(e);
    }
    let _ = std::fs::write(&installed_marker, b"1");
    let ca_trusted = verify_ca_trust(&ca_pem_path);
    log_req("TLS", &format!("new CA installed; verified in store: {ca_trusted}"));
    // Same machine-store install for the fresh CA (see reuse branch above).
    ensure_machine_ca_trust(&ca_pem_path, cert_dir);
    Ok(CertBundle {
        server_cert_pem: srv_pem.into_bytes(),
        server_key_pem: srv_key_pem.into_bytes(),
        crl_der,
        ca_trusted,
    })
}

/// Generate a local CA, a localhost server cert signed by it (with a CRL
/// Distribution Point extension), and an empty CRL signed by the CA.
/// Returns (ca_pem, server_pem, server_der, server_key_pem, crl_der).
fn generate_certs() -> Result<(String, String, Vec<u8>, String, Vec<u8>), String> {
    use rcgen::{BasicConstraints, CertificateParams, CrlDistributionPoint, DnType, IsCa, KeyPair, SanType};
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
    // DNS SANs: `localhost` for the plugin-rewritten API calls, plus
    // `ns.rec.net` for the game's nameserver query, which resolves that
    // hostname itself (via the hosts-file entry) and validates TLS against
    // it on stacks that honor the Windows trust store.
    let mut srv_params =
        CertificateParams::new(vec!["localhost".to_string(), "ns.rec.net".to_string()])
            .map_err(|e| format!("server params: {e}"))?;
    srv_params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    srv_params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::from([127, 0, 0, 1])));
    // Revocation: Windows chain validation fails a cert whose issuer
    // publishes no CRL/OCSP as RevocationStatusUnknown. Point it at the
    // empty CRL this backend serves on :80.
    srv_params.crl_distribution_points = vec![CrlDistributionPoint {
        uris: vec!["http://localhost/crl.pem".to_string()],
    }];
    srv_params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    srv_params.not_after = rcgen::date_time_ymd(2046, 1, 1);
    let srv = srv_params
        .signed_by(&srv_key, &ca, &ca_key)
        .map_err(|e| format!("server: {e}"))?;

    let crl_der = generate_crl(&ca, &ca_key)?;

    Ok((
        ca.pem(),
        srv.pem(),
        srv.der().to_vec(),
        srv_key.serialize_pem(),
        crl_der,
    ))
}

/// Build an empty CRL signed by our CA (DER). Nothing is ever revoked;
/// its only job is to exist so revocation checks can succeed.
fn generate_crl(ca: &rcgen::Certificate, ca_key: &rcgen::KeyPair) -> Result<Vec<u8>, String> {
    let params = rcgen::CertificateRevocationListParams {
        this_update: rcgen::date_time_ymd(2026, 1, 1),
        next_update: rcgen::date_time_ymd(2046, 1, 1),
        crl_number: rcgen::SerialNumber::from_slice(&[1]),
        issuing_distribution_point: None,
        revoked_certs: Vec::new(),
        key_identifier_method: rcgen::KeyIdMethod::Sha256,
    };
    let crl = params
        .signed_by(ca, ca_key)
        .map_err(|e| format!("crl: {e}"))?;
    Ok(crl.der().to_vec())
}

/// Check the CA cert is really in the current user's Trusted Root store by
/// matching its SHA-1 thumbprint via certutil. Non-Windows builds skip
/// this — the test harness installs the CA into the Wine prefix manually.
#[cfg(windows)]
fn verify_ca_trust(ca_pem: &Path) -> bool {
    let hash = match ca_sha1_thumbprint(ca_pem) {
        Some(h) => h,
        None => return false,
    };
    match std::process::Command::new("certutil")
        .args(["-user", "-store", "Root", &hash])
        .output()
    {
        Ok(o) => {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .to_uppercase()
                    .replace([' ', ':'], "")
                    .contains(&hash)
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn verify_ca_trust(_ca_pem: &Path) -> bool {
    true
}

/// SHA-1 thumbprint of a PEM cert, via `certutil -dump` (no extra crates).
#[cfg(windows)]
fn ca_sha1_thumbprint(ca_pem: &Path) -> Option<String> {
    let out = std::process::Command::new("certutil")
        .args(["-dump", &ca_pem.to_string_lossy()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if line.contains("Cert Hash(sha1)") {
            let hex: String = line
                .split(':')
                .nth(1)?
                .chars()
                .filter(|c| c.is_ascii_hexdigit())
                .collect();
            if !hex.is_empty() {
                return Some(hex.to_uppercase());
            }
        }
    }
    None
}

/// Windows self-diagnostic: run `certutil -urlfetch -verify` on the leaf
/// cert — the same chain build (root trust + CRL revocation fetch over
/// :80) the game's TLS stack performs — and record a concise verdict in
/// the backend log and /health. Runs detached; never blocks serving.
#[cfg(windows)]
async fn run_tls_chain_diag(server_der: &Path, state: &Arc<BackendState>) {
    let path = server_der.to_path_buf();
    let summary = tokio::time::timeout(std::time::Duration::from_secs(45), async move {
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("certutil")
                .args(["-urlfetch", "-verify", &path.to_string_lossy()])
                .output()
        })
        .await
    })
    .await;
    let verdict = match summary {
        Err(_) => "diag_timed_out".to_string(),
        Ok(Err(e)) => format!("diag_spawn_failed: {e}"),
        Ok(Ok(Err(e))) => format!("diag_exec_failed: {e}"),
        Ok(Ok(Ok(out))) => {
            let body = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            // Full output for forensics (bounded), then the one-line verdict.
            let clipped: String = body.chars().take(4000).collect();
            log_req("TLS", &format!("certutil -verify output:\n{clipped}"));
            summarize_chain_output(&body, out.status.success())
        }
    };
    log_req("TLS", &format!("Windows chain verdict: {verdict}"));
    if let Ok(mut g) = state.tls_chain_diag.lock() {
        *g = Some(verdict);
    }

    // Companion check: what does a .NET-style consumer see in the USER
    // context? The game rejected our cert while the machine-context chain
    // also failed; this separates the two contexts for the next diagnosis.
    let dotnet_path = server_der.to_path_buf();
    let dotnet = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
        tokio::task::spawn_blocking(move || {
            let der = dotnet_path.to_string_lossy().replace('\'', "''");
            let script = format!(
                "$c=New-Object Security.Cryptography.X509Certificates.X509Certificate2('{der}'); \
                 $ch=New-Object Security.Cryptography.X509Certificates.X509Chain; \
                 $r=$ch.Build($c); \
                 'dotnet_chain_build=' + $r; \
                 $ch.ChainStatus | % {{ '  status=' + $_.Status }}"
            );
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .output()
        })
        .await
    })
    .await;
    match dotnet {
        Err(_) => log_req("TLS", "dotnet user-context chain: diag_timed_out"),
        Ok(Err(e)) => log_req("TLS", &format!("dotnet user-context chain: spawn failed: {e}")),
        Ok(Ok(Err(e))) => log_req("TLS", &format!("dotnet user-context chain: exec failed: {e}")),
        Ok(Ok(Ok(out))) => {
            let body = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let clipped: String = body.chars().take(2000).collect();
            log_req("TLS", &format!("dotnet user-context chain:\n{clipped}"));
        }
    }
}

#[cfg(not(windows))]
async fn run_tls_chain_diag(_server_der: &Path, state: &Arc<BackendState>) {
    if let Ok(mut g) = state.tls_chain_diag.lock() {
        *g = Some("skipped_non_windows".to_string());
    }
}

/// Reduce `certutil -verify` output to the verdict that matters:
/// untrusted_root vs revocation failure vs chain_ok.
fn summarize_chain_output(body: &str, success: bool) -> String {
    let upper = body.to_uppercase();
    let verdict = if upper.contains("CERT_TRUST_IS_UNTRUSTED_ROOT") {
        "untrusted_root"
    } else if upper.contains("CERT_TRUST_IS_REVOKED") || upper.contains("WAS REVOKED") {
        "revoked"
    } else if upper.contains("CERT_TRUST_REVOCATION_STATUS_UNKNOWN") {
        "revocation_unknown"
    } else if upper.contains("UNABLE TO CHECK REVOCATION") || upper.contains("REVOCATION FUNCTION")
    {
        "revocation_check_failed"
    } else if upper.contains("A CERTIFICATE CHAIN COULD NOT BE BUILT") {
        "chain_build_failed"
    } else if upper.contains("CERT_TRUST_IS_NOT_TIME_VALID") || upper.contains("EXPIRED") {
        "not_time_valid"
    } else if upper.contains("CERT_TRUST_IS_OFFLINE_REVOCATION") {
        "revocation_offline"
    } else if success && upper.contains("VERIFIED") {
        "chain_ok"
    } else if success {
        "verify_ok_no_detail"
    } else {
        // Unknown failure: keep the first meaningful line for forensics.
        let first = body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("====="))
            .unwrap_or("unknown")
            .chars()
            .take(160)
            .collect::<String>();
        return format!("unknown_failure: {first}");
    };
    verdict.to_string()
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

/// Non-Windows stub: there is no certutil here, and callers treat `None`
/// as "can't verify", which is only used for marker keying.
#[cfg(not(windows))]
fn ca_sha1_thumbprint(_ca_pem: &Path) -> Option<String> {
    None
}

/// Check the CA cert is really in the LOCAL MACHINE Trusted Root store
/// (no `-user` flag → machine context). The game's TLS chain build runs in
/// a machine context and cannot see the user store — v0.4.1 proved this on
/// real hardware: `certutil -urlfetch -verify` reported
/// CERT_TRUST_IS_PARTIAL_CHAIN / CERT_E_CHAINING with the CA present only
/// in the user store.
#[cfg(windows)]
fn verify_ca_trust_machine(ca_pem: &Path) -> bool {
    let hash = match ca_sha1_thumbprint(ca_pem) {
        Some(h) => h,
        None => return false,
    };
    match std::process::Command::new("certutil")
        .args(["-store", "Root", &hash])
        .output()
    {
        Ok(o) => {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .to_uppercase()
                    .replace([' ', ':'], "")
                    .contains(&hash)
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn verify_ca_trust_machine(_ca_pem: &Path) -> bool {
    true
}

/// Install the CA into the LOCAL MACHINE Trusted Root store. Needs admin,
/// so this elevates via PowerShell `Start-Process -Verb RunAs` (one UAC
/// prompt). The exit code threaded back is certutil's own; a declined UAC
/// surfaces as a PowerShell failure. Non-Windows builds skip this.
#[cfg(windows)]
fn install_ca_trust_machine(ca_pem: &Path) -> Result<(), String> {
    // Single-quote-escape for the PowerShell single-quoted path below.
    let path = ca_pem.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$p = Start-Process certutil -ArgumentList '-addstore','-f','Root','\"{path}\"' \
         -Verb RunAs -Wait -PassThru; exit $p.ExitCode"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|e| format!("couldn't run powershell: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Flux Rec couldn't install its certificate into the machine Trusted Root store.\n\n\
             powershell said: {}\n\n\
             Try running Flux Rec once as administrator, then normally.",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn install_ca_trust_machine(_ca_pem: &Path) -> Result<(), String> {
    Ok(())
}

/// Ensure the CA is also trusted in the LOCAL MACHINE Root store (see
/// `verify_ca_trust_machine`). Fail-soft by design: a declined UAC prompt
/// is logged, not fatal. A per-CA marker file (`ca-machine-installed`,
/// holding the CA thumbprint) records the attempt so we don't re-prompt on
/// every boot; a regenerated CA (new thumbprint) retries automatically.
fn ensure_machine_ca_trust(ca_pem: &Path, cert_dir: &Path) {
    if verify_ca_trust_machine(ca_pem) {
        log_req("TLS", "local CA already trusted in machine Root store");
        return;
    }
    let thumb = match ca_sha1_thumbprint(ca_pem) {
        Some(t) => t,
        None => {
            log_req("TLS", "machine CA trust: couldn't read CA thumbprint; skipping");
            return;
        }
    };
    let marker = cert_dir.join("ca-machine-installed");
    let attempted = std::fs::read_to_string(&marker)
        .map(|s| s.trim() == thumb.as_str())
        .unwrap_or(false);
    if attempted {
        log_req(
            "TLS",
            "machine CA trust: install already attempted for this CA; skipping re-prompt",
        );
        return;
    }
    log_req(
        "TLS",
        "machine CA trust: installing local CA into machine Root store (admin approval needed)",
    );
    let outcome = match install_ca_trust_machine(ca_pem) {
        Ok(()) => {
            if verify_ca_trust_machine(ca_pem) {
                "installed".to_string()
            } else {
                "install ran but CA not found in machine store afterwards".to_string()
            }
        }
        Err(e) => format!(
            "install failed: {}",
            e.split_whitespace().collect::<Vec<_>>().join(" ")
        ),
    };
    // Record the attempt (success or failure) so a UAC decline doesn't
    // re-prompt every boot.
    let _ = std::fs::write(&marker, thumb.as_str());
    log_req("TLS", &format!("machine CA trust: {outcome}"));
}

/// Does this hosts-file text already map `host` to `want_ip`?
fn hosts_has(hosts: &str, want_ip: &str, host: &str) -> bool {
    hosts.lines().any(|l| {
        let body = l.trim_start().split('#').next().unwrap_or("");
        let mut toks = body.split_whitespace();
        match toks.next() {
            Some(ip) if ip == want_ip => toks.any(|t| t.eq_ignore_ascii_case(host)),
            _ => false,
        }
    })
}

/// Does this hosts-file text mention `host` at all (any IP, not commented)?
fn hosts_mentions(hosts: &str, host: &str) -> bool {
    hosts.lines().any(|l| {
        let t = l.trim_start();
        if t.starts_with('#') {
            return false;
        }
        // Strip inline comments, then look at the tokens.
        let body = t.split('#').next().unwrap_or("");
        body.split_whitespace()
            .any(|tok| tok.eq_ignore_ascii_case(host))
    })
}

/// Ensure `127.0.0.1 ns.rec.net` is in the Windows hosts file.
///
/// Why: the Showdown client's nameserver query resolves `ns.rec.net` with
/// System.Net.Dns directly — bypassing the plugin's BestHTTP URL rewrite —
/// and on machines where that (long-dead) hostname doesn't resolve, the
/// game hangs at "Connecting to server..." forever. Pointing it at loopback
/// routes the query to the local backend. Idempotent: skips when the entry
/// already exists. Needs admin, so this elevates via PowerShell
/// `Start-Process -Verb RunAs` (one UAC prompt, first run only), the same
/// pattern as the machine cert-store install. Fail-soft: a declined UAC is
/// logged, not fatal. Non-Windows builds skip this.
#[cfg(windows)]
fn ensure_hosts_entry() {
    const HOSTS: &str = r"C:\Windows\System32\drivers\etc\hosts";
    const WANT_IP: &str = "127.0.0.1";
    const HOST: &str = "ns.rec.net";

    let current = match std::fs::read_to_string(HOSTS) {
        Ok(c) => c,
        Err(e) => {
            log_req(
                "TLS",
                &format!("hosts: couldn't read hosts file ({e}); skipping"),
            );
            return;
        }
    };
    if hosts_has(&current, WANT_IP, HOST) {
        log_req("TLS", "hosts entry 127.0.0.1 ns.rec.net already present");
        return;
    }
    if hosts_mentions(&current, HOST) {
        log_req(
            "TLS",
            "hosts file already maps ns.rec.net somewhere else; leaving it alone",
        );
        return;
    }

    // Write a small PS1 and run it elevated. The script itself is written
    // by us (no quoting layers to get wrong); only the -File path goes
    // through the elevated command line.
    let dir = std::env::temp_dir().join("fluxrec");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log_req(
            "TLS",
            &format!("hosts: couldn't create temp dir ({e}); skipping"),
        );
        return;
    }
    let ps1 = dir.join("add_ns_rec_net_hosts.ps1");
    let script = "$h = 'C:\\Windows\\System32\\drivers\\etc\\hosts'\r\n\
         $c = Get-Content -Path $h -Raw\r\n\
         if ($c -notmatch '(?m)^[^#\\r\\n]*\\bns\\.rec\\.net\\b') {\r\n\
         Add-Content -Path $h -Value \"`r`n127.0.0.1 ns.rec.net # Flux Rec local server\"\r\n\
         }\r\n\
         exit 0\r\n";
    if let Err(e) = std::fs::write(&ps1, script) {
        log_req(
            "TLS",
            &format!("hosts: couldn't write helper script ({e}); skipping"),
        );
        return;
    }
    log_req(
        "TLS",
        "hosts: adding 127.0.0.1 ns.rec.net (admin approval needed)",
    );
    let path = ps1.to_string_lossy().replace('\'', "''");
    let cmd = format!(
        "$p = Start-Process powershell -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{path}' \
         -Verb RunAs -Wait -PassThru; exit $p.ExitCode"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &cmd])
        .output();
    let _ = std::fs::remove_file(&ps1);
    match out {
        Ok(o) if o.status.success() => match std::fs::read_to_string(HOSTS) {
            Ok(c) if hosts_has(&c, WANT_IP, HOST) => {
                log_req("TLS", "hosts: 127.0.0.1 ns.rec.net added")
            }
            Ok(_) => log_req(
                "TLS",
                "hosts: elevated script ran but entry not found afterwards",
            ),
            Err(e) => log_req("TLS", &format!("hosts: couldn't verify ({e})")),
        },
        Ok(o) => log_req(
            "TLS",
            &format!(
                "hosts: elevated script failed ({}); the game may hang at 'Connecting to server'",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
        ),
        Err(e) => log_req(
            "TLS",
            &format!("hosts: couldn't launch powershell ({e})"),
        ),
    }
}

#[cfg(not(windows))]
fn ensure_hosts_entry() {}
