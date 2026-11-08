//! Tachyon background server (v1.0.81).
//!
//! The 2026 client's room/voice transport is a Tachyon session server
//! (Unity Netcode/LiteNetLib under the historical "Photon" label — see
//! the patch-2026 notes), and the matchmaking DTO hands the client a
//! `host:port` endpoint. The launcher therefore bundles our Tachyon
//! server build: every play session keeps
//! `<game dir>/tachyon-server/TachyonServer.exe` in sync with the HF
//! mirror and makes sure one instance is running, hidden and detached,
//! before the game starts.
//!
//! The same folder also carries the playit.gg tunnel agent
//! (`playit.exe`, pinned below), which is what makes the owner's
//! locally-hosted server reachable from the public internet. The agent
//! is downloaded for everyone but only ever *started* on the host's
//! own install: the launcher holds no account context, so the gate is
//! the presence of a `playit-secret.txt` in the folder — provisioned
//! out-of-band, or by release builds themselves on the owner's
//! machine (see `provision_host_secret`).
//!
//! Startup ordering (v1.0.80): the server lifecycle — sync, spawn,
//! wait-until-listening — runs through the synchronous readiness gate
//! `ensure_server_ready` on the play path, NOT on a detached thread.
//! A client handed `127.0.0.1:7777` before anything listens there
//! does not fall back to solo play; it retries the voice/room
//! connection and the load hangs (v1.0.78 shipped exactly that race:
//! the gate is the fix). Only the playit agent duties — none of which
//! may gate the game — stay on the detached `kick_off` thread.
//!
//! Everything here is fail-soft by design:
//! - a failed sidecar fetch keeps the installed exe untouched;
//! - a failed download never replaces a working exe (temp file, hash
//!   verified, then renamed into place);
//! - a busy UDP 7777 means a server is already running — nothing to do;
//! - no error in this module may block or fail the game launch. The
//!   readiness gate is bounded (see `READY_BUDGET`); on any failure
//!   it logs and the game launches unarmed.
//! - the gate's outcome lines are also appended to
//!   `<game dir>/tachyon-server/tachyon.log` (see `gate_log`): the
//!   launcher's stdout is invisible after the fact on Windows, and
//!   that file is the artifact support can ask for.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// HF mirror directory holding the server binary and its sha256 sidecar
/// (same dataset the launcher's other assets sync from).
const MIRROR_BASE: &str =
    "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/tachyon-server/";
const SERVER_EXE_NAME: &str = "TachyonServer.exe";
/// UDP port the bundled server listens on (loopback endpoint handed out
/// by matchmaking for local sessions).
pub(crate) const SERVER_PORT: u16 = 7777;

/// Hard total budget for the synchronous readiness gate: the mirror
/// sync, the spawn and the listen wait together may hold the game
/// launch for at most this long. A 67 MB first-run download on a slow
/// connection fits comfortably; anything slower launches unarmed
/// rather than freezing the launcher.
const READY_BUDGET: Duration = Duration::from_secs(180);
/// How long the gate waits, after the spawn step, for the server to
/// actually bind UDP 7777 (further bounded by the remaining budget).
const LISTEN_WAIT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the port to be taken.
const LISTEN_POLL: Duration = Duration::from_millis(250);

/// playit.gg tunnel agent (daemon build), kept next to the server
/// under the local name `playit.exe`. Pinned to upstream's v2.0.0
/// release (playit-cloud/playit-agent, tag v2.0.0-r1): the signed
/// x86_64 daemon exe, not the .msi installer — the signature keeps
/// Windows Security from quarantining it the way unsigned helpers
/// have been hit before. playit publishes no hash sidecar, so the
/// pin is version + URL. The v2 daemon takes its secret from a
/// `playit.toml` config file, NOT from the environment (see
/// `provision_agent_config`).
const AGENT_EXE_NAME: &str = "playit.exe";
const AGENT_DOWNLOAD_URL: &str = "https://github.com/playit-cloud/playit-agent/releases/download/v2.0.0-r1/playitd-windows-x86_64-signed.exe";
/// Owner-only secret file. Its mere presence in the tachyon-server
/// folder is the host gate (see `ensure_agent_running`); the contents
/// are the playit agent secret key and are never logged.
const AGENT_SECRET_NAME: &str = "playit-secret.txt";

/// Directory inside the game dir that the bundled server lives in.
fn server_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("tachyon-server")
}

fn server_exe(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join(SERVER_EXE_NAME)
}

fn agent_exe(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join(AGENT_EXE_NAME)
}

fn agent_secret_file(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join(AGENT_SECRET_NAME)
}

/// Detached play-path worker, called from `main()`'s `--play` /
/// `--launch-game` dispatch right after `ensure_server_ready`.
///
/// Since v1.0.80 this thread owns ONLY the playit agent duties —
/// agent binary sync, owner-host provisioning, agent start — none of
/// which may gate the game, so all of it stays off the launch path.
/// The server lifecycle (sync, spawn, listen wait) deliberately does
/// NOT live here anymore: it moved to the synchronous
/// `ensure_server_ready` gate, because a detached thread cannot
/// promise the server is listening before the game starts (the
/// v1.0.78 18%-hang race). Do not re-add server work here; the gate
/// is its single owner on the play path.
pub(crate) fn kick_off(game_dir: &Path) {
    let game_dir = game_dir.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("tachyon-agent".to_string())
        .spawn(move || {
            // Unity keeps appending to Player.log, so the owner's identity line
            // can be far from the end. Provision before mirror work can delay it.
            provision_host_secret(&game_dir);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                runtime.block_on(async {
                    ensure_agent(&game_dir).await;
                });
            }
            // The tunnel agent exposes the server the readiness gate
            // started just before this thread was spawned.
            ensure_agent_running(&game_dir);
        });
}

/// What the mirror sync should do, decided from the local and
/// mirror hashes alone — pure, so the decision matrix is unit-tested
/// without any network.
#[derive(Debug, PartialEq, Eq)]
enum SyncDecision {
    /// Local exe already matches the mirror hash: nothing to do.
    UpToDate,
    /// Local exe missing or stale: download and install.
    Download,
    /// The mirror hash could not be fetched (offline, mirror
    /// hiccup): keep whatever is installed, stale or not.
    KeepInstalledNoMirror,
}

fn sync_decision(local_hash: Option<&str>, expected_hash: Option<&str>) -> SyncDecision {
    match expected_hash {
        None => SyncDecision::KeepInstalledNoMirror,
        Some(expected) if local_hash == Some(expected) => SyncDecision::UpToDate,
        Some(_) => SyncDecision::Download,
    }
}

/// Keep the bundled `TachyonServer.exe` in sync with the HF mirror:
/// fetch the `.sha256` sidecar and download the exe only when the
/// local copy is missing or its hash differs. If the sidecar fetch
/// fails (offline, mirror hiccup) and a local exe exists, it is kept
/// as-is — the same fail-soft contract as the plugin sync.
pub(crate) async fn ensure_server(game_dir: &Path) {
    let dir = server_dir(game_dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("[tachyon] cannot create {}: {error}", dir.display());
        return;
    }
    let exe = dir.join(SERVER_EXE_NAME);

    let client = match reqwest::Client::builder()
        .user_agent(concat!("FluxRec-Setup/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[tachyon] http client failed: {error}");
            return;
        }
    };

    let sidecar_url = format!("{MIRROR_BASE}{SERVER_EXE_NAME}.sha256");
    let fetched = fetch_expected_hash(&client, &sidecar_url).await;
    let local = if exe.is_file() {
        sha256_file(&exe)
    } else {
        None
    };
    let expected = match sync_decision(local.as_deref(), fetched.as_deref()) {
        SyncDecision::UpToDate => return, // already in sync
        SyncDecision::KeepInstalledNoMirror => {
            if exe.is_file() {
                println!("[tachyon] mirror unreachable — keeping installed server.");
            } else {
                eprintln!("[tachyon] mirror unreachable and no server installed yet.");
            }
            return;
        }
        SyncDecision::Download => match fetched {
            Some(hash) => hash,
            // sync_decision only picks Download when a hash was
            // fetched; this arm is unreachable in practice.
            None => return,
        },
    };

    // Download to a temp name, verify the hash, then move into place:
    // a partial or corrupt download never replaces a working exe.
    println!("[tachyon] Downloading Tachyon server…");
    let temp = dir.join(format!("{SERVER_EXE_NAME}.download"));
    let url = format!("{MIRROR_BASE}{SERVER_EXE_NAME}");
    if let Err(error) = download_verified(&client, &url, &temp, &expected).await {
        eprintln!("[tachyon] server download failed: {error}");
        let _ = std::fs::remove_file(&temp);
        return;
    }
    if exe.is_file() {
        // Windows cannot replace a running exe; if the old server is
        // still up this removal fails and the new binary lands on the
        // next launch instead (ensure_running starts nothing there).
        let _ = std::fs::remove_file(&exe);
    }
    if let Err(error) = std::fs::rename(&temp, &exe) {
        eprintln!("[tachyon] could not install downloaded server: {error}");
        let _ = std::fs::remove_file(&temp);
        return;
    }
    println!("[tachyon] Tachyon server synced from mirror.");
}

/// Pure budget math for the readiness gate: how much of `budget` is
/// left after `elapsed` (saturates at zero once the budget is spent).
fn budget_remaining(budget: Duration, elapsed: Duration) -> Duration {
    budget.saturating_sub(elapsed)
}

/// How long the gate may wait for the port after the spawn step: the
/// listen-wait cap, further bounded by the launch budget left over.
fn listen_wait_allowance(budget: Duration, elapsed: Duration) -> Duration {
    LISTEN_WAIT.min(budget_remaining(budget, elapsed))
}

/// Poll `is_listening` every `poll` until it reports true or
/// `max_wait` has elapsed. Probe and sleep are injected so tests can
/// drive the loop without real ports or real time passing.
fn wait_until_listening_with(
    mut is_listening: impl FnMut() -> bool,
    max_wait: Duration,
    poll: Duration,
    mut sleep: impl FnMut(Duration),
) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        if is_listening() {
            return true;
        }
        if waited >= max_wait {
            return false;
        }
        let step = poll.min(max_wait - waited);
        sleep(step);
        waited += step;
    }
}

/// `wait_until_listening_with` against the real server port.
fn wait_until_listening(max_wait: Duration) -> bool {
    wait_until_listening_with(
        || !udp_port_free(SERVER_PORT),
        max_wait,
        LISTEN_POLL,
        std::thread::sleep,
    )
}

/// The readiness gate's own log file, next to the server binary.
///
/// The crate's `Diagnostics/` folder holds the *game's* `Player.log`
/// — a file the game itself writes via its `-logFile` argument (the
/// path helper is private to `launcher.rs`, and it locates the game's
/// log, not a launcher log). There is no launcher file-log sink to
/// reuse, and mixing launcher lines into the game's log would
/// corrupt the very artifact the owner gate reads — so the gate
/// keeps its own file here instead.
fn tachyon_log_path(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join("tachyon.log")
}

/// Emit one readiness-gate outcome line: to stdout, like every
/// launcher log line, AND appended to `tachyon.log` — on Windows the
/// stdout is gone once the launcher exits, while the file is the
/// artifact support can ask for when a launch misbehaves. Strictly
/// best-effort: a failed file write never affects the gate.
fn gate_log(game_dir: &Path, line: &str) {
    println!("{line}");
    if std::fs::create_dir_all(server_dir(game_dir)).is_ok() {
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(tachyon_log_path(game_dir))
            .and_then(|mut file| writeln!(file, "{line}"));
        let _ = appended;
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// Synchronous readiness gate for the play path (v1.0.80) — the fix
/// for the v1.0.78 startup-ordering flaw. `main()` calls this BEFORE
/// starting the game, and the invariant it restores is:
///
/// **the game process is never started until UDP 127.0.0.1:7777 is
/// confirmed listening, or the bounded budget (`READY_BUDGET`)
/// expires and the gate logs and lets the launch proceed unarmed.**
///
/// In order: (a) sync `TachyonServer.exe` with the mirror,
/// synchronously — a first run may spend a while on the 67 MB
/// download, with progress lines, instead of the game freezing at
/// 18% against a dead endpoint; (b) spawn the server hidden and
/// detached if the port is free (the existing `ensure_running`);
/// (c) poll until the port is actually taken. A server left over
/// from an earlier session (it is detached and outlives the
/// launcher) short-circuits the whole gate: it is already listening,
/// and its binary cannot be replaced while it runs anyway.
///
/// Returns true when a server is listening. Every failure path
/// returns false and NEVER propagates an error: worst case the
/// launch behaves exactly like the pre-Tachyon launcher.
pub(crate) fn ensure_server_ready(game_dir: &Path) -> bool {
    // Fast path: already listening (see above).
    if !udp_port_free(SERVER_PORT) {
        gate_log(
            game_dir,
            "[tachyon] readiness gate: server already listening on udp/7777",
        );
        return true;
    }
    let started = Instant::now();
    gate_log(game_dir, "[tachyon] readiness gate: syncing background server…");

    // (a) Sync the binary, synchronously, inside the launch budget.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[tachyon] readiness gate: could not start async runtime: {error}");
            return false;
        }
    };
    let sync_budget = budget_remaining(READY_BUDGET, started.elapsed());
    let synced = runtime.block_on(async {
        tokio::time::timeout(sync_budget, ensure_server(game_dir))
            .await
            .is_ok()
    });
    if !synced {
        // The dropped sync may leave a partial `.download` temp file;
        // the next launch's download truncates and rewrites it.
        gate_log(
            game_dir,
            "[tachyon] readiness gate: sync exceeded the launch budget — launching unarmed",
        );
        return false;
    }
    gate_log(
        game_dir,
        &format!(
            "[tachyon] readiness gate: sync done (server binary present: {})",
            yes_no(server_exe(game_dir).is_file())
        ),
    );

    // (b) Spawn hidden/detached if the port is (still) free.
    ensure_running(game_dir);
    gate_log(
        game_dir,
        &format!(
            "[tachyon] readiness gate: spawn step done (port taken: {})",
            yes_no(!udp_port_free(SERVER_PORT))
        ),
    );

    // (c) Wait until the port is actually taken — a spawned process
    // is not a listening server until it binds.
    let allowance = listen_wait_allowance(READY_BUDGET, started.elapsed());
    if wait_until_listening(allowance) {
        gate_log(
            game_dir,
            "[tachyon] readiness gate: udp/7777 listening — server ready",
        );
        true
    } else {
        gate_log(
            game_dir,
            "[tachyon] readiness gate: udp/7777 not listening in time — launching unarmed",
        );
        false
    }
}

/// Keep the pinned playit agent binary present next to the server.
/// Presence is not permission to run — `ensure_agent_running` applies
/// the host gate separately. Fail-soft like everything in this module:
/// a failed download simply leaves the agent for the next launch.
pub(crate) async fn ensure_agent(game_dir: &Path) {
    let exe = agent_exe(game_dir);
    if exe.is_file() {
        return;
    }
    let dir = server_dir(game_dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("[tachyon] cannot create {}: {error}", dir.display());
        return;
    }
    let client = match reqwest::Client::builder()
        .user_agent(concat!("FluxRec-Setup/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[tachyon] http client failed: {error}");
            return;
        }
    };
    // No upstream hash exists to verify against; download to a temp
    // name and only move into place when the result at least parses
    // as a Windows PE executable (corruption gate, not a signature
    // check — the pinned asset is upstream's code-signed build).
    let temp = dir.join(format!("{AGENT_EXE_NAME}.download"));
    if let Err(error) = download_to_file(&client, AGENT_DOWNLOAD_URL, &temp).await {
        eprintln!("[tachyon] playit agent download failed: {error}");
        let _ = std::fs::remove_file(&temp);
        return;
    }
    if !looks_like_pe(&temp) {
        eprintln!("[tachyon] playit agent download is not a PE executable; discarding.");
        let _ = std::fs::remove_file(&temp);
        return;
    }
    if let Err(error) = std::fs::rename(&temp, &exe) {
        eprintln!("[tachyon] could not install playit agent: {error}");
        let _ = std::fs::remove_file(&temp);
        return;
    }
    println!("[tachyon] playit agent installed (pinned v2.0.0).");
}

/// Stream `url` to `dest` without a digest check (for the pinned
/// playit asset, which has no published hash).
async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read {url}: {e}"))?
    {
        file.write_all(&chunk)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
    }
    file.flush()
        .map_err(|e| format!("flush {}: {e}", dest.display()))?;
    Ok(())
}

/// True when the file starts with the PE "MZ" magic.
fn looks_like_pe(path: &Path) -> bool {
    use std::io::Read;

    let mut magic = [0_u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map(|_| &magic == b"MZ")
        .unwrap_or(false)
}

/// Fetch and parse the mirror's sha256 sidecar. `None` on any failure —
/// callers treat that as "keep what is installed".
async fn fetch_expected_hash(client: &reqwest::Client, url: &str) -> Option<String> {
    let text = client
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    parse_sidecar_hash(&text)
}

/// The sidecar is a sha256sum-style line: `<hex>  TachyonServer.exe`.
/// The bare-hash form is accepted too.
fn parse_sidecar_hash(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    let token = token.to_ascii_lowercase();
    (token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())).then_some(token)
}

/// Stream `url` to `dest`, hashing as it lands; the file is only kept
/// (by the caller) when the final digest matches `expected_sha256`.
async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha256: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut digest = Sha256::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read {url}: {e}"))?
    {
        digest.update(&chunk);
        file.write_all(&chunk)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
    }
    file.flush()
        .map_err(|e| format!("flush {}: {e}", dest.display()))?;
    let actual = hex::encode(digest.finalize());
    if actual != expected_sha256 {
        return Err(format!(
            "sha256 mismatch for {url}: got {actual}, want {expected_sha256}"
        ));
    }
    Ok(())
}

/// Lowercase hex sha256 of a file's contents, `None` if unreadable.
fn sha256_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};

    let bytes = std::fs::read(path).ok()?;
    let mut digest = Sha256::new();
    digest.update(&bytes);
    Some(hex::encode(digest.finalize()))
}

/// Start the bundled server, hidden and detached, unless one is
/// already listening: a loopback UDP port that cannot be bound is the
/// "already running" signal.
pub(crate) fn ensure_running(game_dir: &Path) {
    let exe = server_exe(game_dir);
    if !exe.is_file() {
        return;
    }
    if !udp_port_free(SERVER_PORT) {
        return; // already running (or another listener owns the port)
    }
    spawn_server(&exe);
}

/// True when nothing is bound to UDP 127.0.0.1:`port`.
fn udp_port_free(port: u16) -> bool {
    std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(windows)]
fn spawn_server(exe: &Path) {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW | DETACHED_PROCESS: no console window, ever, and
    // the server is not tied to the launcher's lifetime — it keeps
    // serving the room after the launcher window closes.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    match std::process::Command::new(exe)
        .current_dir(dir)
        .arg("--log-path")
        .arg(dir.join("playit.log"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
    {
        Ok(_) => println!("[tachyon] background server started on udp/{SERVER_PORT}."),
        Err(error) => eprintln!("[tachyon] could not start server: {error}"),
    }
}

#[cfg(not(windows))]
fn spawn_server(_exe: &Path) {
    // The bundled server binary is Windows-only; other platforms have
    // nothing to spawn. No-op so the launch flow (and its tests) stays
    // identical in Linux CI checks.
}

/// Read the provisioned playit secret. `None` when the file is
/// missing, empty, or not the expected shape (the playit secret is
/// a 64-char hex string). The value is never logged anywhere.
fn read_agent_secret(game_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(agent_secret_file(game_dir)).ok()?;
    let secret = text.trim().to_string();
    plausible_agent_secret(&secret).then_some(secret)
}

/// The playit agent secret is exactly 64 hex characters.
fn plausible_agent_secret(secret: &str) -> bool {
    secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The v2 daemon's config file contents for a given secret.
fn agent_toml(secret: &str) -> String {
    format!("secret_key = \"{secret}\"\n")
}

/// Write `playit.toml` into `dir` (created if needed).
fn write_playit_toml(dir: &Path, secret: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(dir.join("playit.toml"), agent_toml(secret))
        .map_err(|e| format!("write playit.toml: {e}"))
}

/// Provision the daemon's config before spawning it. The v2 daemon
/// ignores the SECRET_KEY environment variable and waits for a
/// `playit.toml` instead (verified against the real agent): its
/// Windows config location is `%APPDATA%\playit_gg\playit.toml`.
/// A second copy lands next to the agent exe. Both writes are
/// best-effort — a failure is logged, never fatal.
fn provision_agent_config(game_dir: &Path, secret: &str) {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let config_dir = PathBuf::from(appdata).join("playit_gg");
        if let Err(error) = write_playit_toml(&config_dir, secret) {
            eprintln!("[tachyon] could not write playit config: {error}");
        }
    }
    if let Err(error) = write_playit_toml(&server_dir(game_dir), secret) {
        eprintln!("[tachyon] could not write playit config next to agent: {error}");
    }
}

/// Playit secret baked in at compile time by CI release builds from
/// the `PLAYIT_SECRET_KEY` Actions secret (the workflow passes it as
/// `FLUXREC_PLAYIT_SECRET` — the same `option_env!` pattern as the ns
/// host and Photon ids in `main.rs`). The value exists ONLY in the
/// compiled binary: never in source, never logged, never written
/// anywhere except the owner's `playit-secret.txt` below. `None` in
/// any build whose environment lacked the variable (forks, local
/// builds, PR builds), which keeps those builds fully dormant.
const EMBEDDED_AGENT_SECRET: Option<&str> = option_env!("FLUXREC_PLAYIT_SECRET");

/// The owner's public in-game handle. A machine's own Player.log
/// carries identity lines naming the *local* player, so the owner's
/// log contains this marker and no other player's log does.
const OWNER_LOG_MARKER: &str = "Ripo6000";

/// Bytes of Player.log tail the owner gate inspects — enough to cover
/// a recent session's identity lines without reading a log that can
/// grow to tens of MB end to end.
const LOG_TAIL_BYTES: u64 = 256 * 1024;

/// Candidate `Player.log` paths, mirroring the folder logic of
/// `launcher::diagnostic_log_path` (the game's `Diagnostics` folder
/// first, then the per-user FluxRec folder under LOCALAPPDATA).
/// Read-only: unlike the diagnostics helper this never creates
/// folders or files.
fn player_log_candidates(game_dir: &Path) -> Vec<PathBuf> {
    let mut folders = vec![game_dir.join("Diagnostics")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        folders.push(PathBuf::from(local).join("FluxRec").join("Diagnostics"));
    }
    folders.into_iter().map(|f| f.join("Player.log")).collect()
}

/// The most recently modified existing candidate log, if any.
fn most_recent_player_log(game_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for candidate in player_log_candidates(game_dir) {
        let modified = std::fs::metadata(&candidate)
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok());
        if let Some(modified) = modified {
            if best.as_ref().map_or(true, |(_, t)| modified > *t) {
                best = Some((candidate, modified));
            }
        }
    }
    best.map(|(path, _)| path)
}

/// The last `max_bytes` of a file as lossy UTF-8; `None` on any IO
/// error. Seeks instead of reading the whole file.
fn read_log_tail(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max_bytes))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Scan the full game log for the owner's marker with bounded memory.
/// Unity keeps appending to Player.log, so the identity line can be
/// hundreds of kilobytes before the end and disappear from the short
/// tail used for the common case.
fn log_file_shows_owner(path: &Path) -> Option<bool> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let marker = OWNER_LOG_MARKER.as_bytes();
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut overlap = Vec::with_capacity(marker.len().saturating_sub(1));
    loop {
        let count = file.read(&mut chunk).ok()?;
        if count == 0 {
            return Some(false);
        }
        let mut window = Vec::with_capacity(overlap.len() + count);
        window.extend_from_slice(&overlap);
        window.extend_from_slice(&chunk[..count]);
        if window.windows(marker.len()).any(|bytes| bytes == marker) {
            return Some(true);
        }
        let keep_from = window.len().saturating_sub(marker.len().saturating_sub(1));
        overlap.clear();
        overlap.extend_from_slice(&window[keep_from..]);
    }
}

/// Pure owner gate: does this log text prove the local player is the
/// owner? Takes the text (not a path) so tests drive it with fixtures.
fn log_shows_owner(log_text: &str) -> bool {
    log_text.contains(OWNER_LOG_MARKER)
}

/// What provisioning should do — a pure decision, unit-tested with a
/// dummy secret. The real secret never flows through the tests.
#[derive(Debug, PartialEq, Eq)]
enum ProvisionAction {
    /// Nothing usable was compiled in: stay dormant (fork/local build
    /// or an unset CI secret).
    NoEmbeddedSecret,
    /// A secret exists but this machine's log does not prove the
    /// owner (a missing or unreadable log included): write nothing.
    NotOwner,
    /// Owner proven: write this secret to `playit-secret.txt`.
    Write(String),
}

fn provision_action(embedded: Option<&str>, log_tail: Option<&str>) -> ProvisionAction {
    let secret = match embedded {
        Some(raw) if plausible_agent_secret(raw.trim()) => raw.trim(),
        _ => return ProvisionAction::NoEmbeddedSecret,
    };
    match log_tail {
        Some(text) if log_shows_owner(text) => ProvisionAction::Write(secret.to_string()),
        _ => ProvisionAction::NotOwner,
    }
}

/// Owner-host provisioning, run inside `kick_off` before the agent
/// start: on the owner's machine only, materialise `playit-secret.txt`
/// from the embedded secret so `ensure_agent_running` can bring the
/// tunnel up. Fail-closed (no owner proof → nothing is written) and
/// fail-soft (any IO error → nothing is written, launch proceeds).
/// The secret itself is never logged — only the outcome line is.
pub(crate) fn provision_host_secret(game_dir: &Path) {
    let log_tail = most_recent_player_log(game_dir)
        .and_then(|path| {
            let tail = read_log_tail(&path, LOG_TAIL_BYTES)?;
            if log_shows_owner(&tail) || log_file_shows_owner(&path)? {
                Some(if log_shows_owner(&tail) {
                    tail
                } else {
                    OWNER_LOG_MARKER.to_string()
                })
            } else {
                Some(tail)
            }
        });
    match provision_action(EMBEDDED_AGENT_SECRET, log_tail.as_deref()) {
        ProvisionAction::Write(secret) => {
            let file = agent_secret_file(game_dir);
            let already = std::fs::read_to_string(&file)
                .map(|text| text.trim() == secret.as_str())
                .unwrap_or(false);
            if already {
                gate_log(game_dir, "[tachyon] host provisioning: applied");
                return;
            }
            // Exact secret bytes, no trailing newline.
            let written = std::fs::create_dir_all(server_dir(game_dir))
                .and_then(|()| std::fs::write(&file, &secret));
            match written {
                Ok(()) => gate_log(game_dir, "[tachyon] host provisioning: applied"),
                Err(error) => gate_log(
                    game_dir,
                    &format!("[tachyon] host provisioning failed: {error}"),
                ),
            }
        }
        ProvisionAction::NotOwner => {
            gate_log(
                game_dir,
                "[tachyon] host provisioning: owner marker not found in the local game log",
            );
        }
        ProvisionAction::NoEmbeddedSecret => {
            gate_log(
                game_dir,
                "[tachyon] host provisioning: this release has no valid Playit secret",
            );
        }
    }
}

/// Start the playit tunnel agent — HOST ONLY.
///
/// The launcher holds no account context (sign-in is web-only; the
/// play path never sees an account id or access token), so it cannot
/// prove the signed-in player is the host account. The gate is
/// therefore the provisioned secret file: the agent runs only when
/// `playit-secret.txt` exists in the tachyon-server folder, which is
/// only ever true on the host's own install (written there either
/// out-of-band or by `provision_host_secret` on the owner's machine).
/// Every other
/// player gets the binary (inert, a few MB) but never a running agent.
/// No secret file, no exe → skip silently.
pub(crate) fn ensure_agent_running(game_dir: &Path) {
    let exe = agent_exe(game_dir);
    if !exe.is_file() {
        return;
    }
    let secret = match read_agent_secret(game_dir) {
        Some(secret) => secret,
        None => return,
    };
    provision_agent_config(game_dir, &secret);
    spawn_agent(&exe, &secret);
}

#[cfg(windows)]
fn spawn_agent(exe: &Path, secret: &str) {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW | DETACHED_PROCESS (same pattern as the server).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    // One agent per machine is enough: if any playit.exe is already
    // running (ours from an earlier launch, or the host's own), the
    // tunnel is up — don't stack duplicates on every game launch.
    let already_running = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq playit.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("playit.exe"))
        .unwrap_or(false);
    if already_running {
        return;
    }

    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    match std::process::Command::new(exe)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // The v2 daemon authenticates from playit.toml (written by
        // provision_agent_config before this spawn); the environment
        // variable is kept as a harmless belt-and-braces for other
        // builds. The launcher keeps the secret out of arguments and
        // its own log; Playit writes tunnel status to playit.log.
        .env("SECRET_KEY", secret)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
    {
        Ok(_) => println!("[tachyon] playit tunnel agent started."),
        Err(error) => eprintln!("[tachyon] could not start playit agent: {error}"),
    }
}

#[cfg(not(windows))]
fn spawn_agent(_exe: &Path, _secret: &str) {
    // Windows-only binary; nothing to spawn on other platforms.
}

#[cfg(test)]
mod tests {
    use super::{
        budget_remaining, gate_log, listen_wait_allowance, parse_sidecar_hash, server_exe,
        sha256_file, sync_decision, tachyon_log_path, udp_port_free, wait_until_listening_with,
        SyncDecision,
    };
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn parses_sha256sum_sidecar() {
        let hash = "9947d9ffd222d4eb10d180f947277dd3a3af3e46be31c8a29cf96823251cbde0";
        assert_eq!(
            parse_sidecar_hash(&format!("{hash}  TachyonServer.exe\n")),
            Some(hash.to_string())
        );
        assert_eq!(parse_sidecar_hash(hash), Some(hash.to_string()));
        assert_eq!(parse_sidecar_hash("  \n"), None);
        assert_eq!(parse_sidecar_hash("not-a-hash"), None);
        assert_eq!(parse_sidecar_hash("zzzz  TachyonServer.exe"), None);
    }

    #[test]
    fn occupied_udp_port_is_not_free() {
        let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        assert!(!udp_port_free(port));
        drop(socket);
        assert!(udp_port_free(port));
    }

    #[test]
    fn server_paths_live_under_game_dir() {
        let game_dir = Path::new("C:/Games/FluxRec");
        assert_eq!(
            server_exe(game_dir),
            Path::new("C:/Games/FluxRec/tachyon-server/TachyonServer.exe")
        );
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-hash-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc.bin");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn playit_asset_is_the_pinned_windows_x64_daemon_exe() {
        // The pin is deliberate: v2.0.0 daemon build, standalone signed
        // exe (renamed to playit.exe locally), never the .msi.
        assert!(super::AGENT_DOWNLOAD_URL.contains("/v2.0.0-r1/"));
        assert!(super::AGENT_DOWNLOAD_URL.ends_with("playitd-windows-x86_64-signed.exe"));
        assert!(!super::AGENT_DOWNLOAD_URL.ends_with(".msi"));
    }

    #[test]
    fn pe_check_requires_mz_magic() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-pe-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pe = dir.join("agent.exe");
        std::fs::write(&pe, b"MZ\x90\x00rest").unwrap();
        assert!(super::looks_like_pe(&pe));
        let not_pe = dir.join("page.html");
        std::fs::write(&not_pe, b"<html>404</html>").unwrap();
        assert!(!super::looks_like_pe(&not_pe));
        assert!(!super::looks_like_pe(&dir.join("missing.exe")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agent_secret_gate_is_file_presence() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-secret-{}",
            std::process::id()
        ));
        let game_dir = dir.join("game");
        std::fs::create_dir_all(super::server_dir(&game_dir)).unwrap();
        let hex_secret = "0123456789abcdef".repeat(4);
        // No file -> not the host.
        assert_eq!(super::read_agent_secret(&game_dir), None);
        // Empty or malformed file -> still not the host.
        std::fs::write(super::agent_secret_file(&game_dir), "  \n").unwrap();
        assert_eq!(super::read_agent_secret(&game_dir), None);
        std::fs::write(super::agent_secret_file(&game_dir), "test-secret\n").unwrap();
        assert_eq!(super::read_agent_secret(&game_dir), None);
        // Provisioned 64-hex secret -> host; surrounding whitespace is trimmed.
        std::fs::write(
            super::agent_secret_file(&game_dir),
            format!("  {hex_secret}\n"),
        )
        .unwrap();
        assert_eq!(
            super::read_agent_secret(&game_dir).as_deref(),
            Some(hex_secret.as_str())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn playit_toml_wraps_the_hex_secret() {
        let hex_secret = "0123456789abcdef".repeat(4);
        assert_eq!(
            super::agent_toml(&hex_secret),
            format!("secret_key = \"{hex_secret}\"\n")
        );
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-toml-{}",
            std::process::id()
        ));
        super::write_playit_toml(&dir, &hex_secret).unwrap();
        let written = std::fs::read_to_string(dir.join("playit.toml")).unwrap();
        assert_eq!(written, format!("secret_key = \"{hex_secret}\"\n"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owner_gate_needs_the_owner_marker() {
        // Fixtures shaped like the local-player identity lines a real
        // Player.log carries.
        let owner_log =
            "Loaded scene 'DormRoom'\n[Net] Local player Ripo6000 (account 121) joined\n";
        assert!(super::log_shows_owner(owner_log));
        let other_log =
            "Loaded scene 'DormRoom'\n[Net] Local player Guest42 (account 999) joined\n";
        assert!(!super::log_shows_owner(other_log));
        assert!(!super::log_shows_owner(""));
    }

    #[test]
    fn provisioning_decision_is_fail_closed() {
        let dummy_secret = "0123456789abcdef".repeat(4);
        let owner_log = Some("session\nLocal player Ripo6000\n");
        let other_log = Some("session\nLocal player Guest42\n");
        // No usable embedded secret -> dormant, whatever the log says.
        assert_eq!(
            super::provision_action(None, owner_log),
            super::ProvisionAction::NoEmbeddedSecret
        );
        assert_eq!(
            super::provision_action(Some(""), owner_log),
            super::ProvisionAction::NoEmbeddedSecret
        );
        assert_eq!(
            super::provision_action(Some("not-a-secret"), owner_log),
            super::ProvisionAction::NoEmbeddedSecret
        );
        // Owner log + plausible secret -> write exactly the secret.
        assert_eq!(
            super::provision_action(Some(dummy_secret.as_str()), owner_log),
            super::ProvisionAction::Write(dummy_secret.clone())
        );
        // Any other log state -> fail closed, write nothing.
        assert_eq!(
            super::provision_action(Some(dummy_secret.as_str()), other_log),
            super::ProvisionAction::NotOwner
        );
        assert_eq!(
            super::provision_action(Some(dummy_secret.as_str()), Some("")),
            super::ProvisionAction::NotOwner
        );
        assert_eq!(
            super::provision_action(Some(dummy_secret.as_str()), None),
            super::ProvisionAction::NotOwner
        );
    }

    #[test]
    fn log_tail_reads_the_end_of_the_file() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-tail-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Player.log");
        let mut content = vec![b'x'; 4096];
        content.extend_from_slice(b"Ripo6000");
        std::fs::write(&file, &content).unwrap();
        let tail = super::read_log_tail(&file, 64).unwrap();
        assert_eq!(tail.len(), 64);
        assert!(tail.ends_with("Ripo6000"));
        // A short file is returned whole; a missing one yields None.
        let short = dir.join("short.log");
        std::fs::write(&short, b"tiny").unwrap();
        assert_eq!(super::read_log_tail(&short, 64).as_deref(), Some("tiny"));
        assert!(super::read_log_tail(&dir.join("missing.log"), 64).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owner_log_is_found_under_the_game_diagnostics_folder() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-logfind-{}",
            std::process::id()
        ));
        let game_dir = dir.join("game");
        let diagnostics = game_dir.join("Diagnostics");
        std::fs::create_dir_all(&diagnostics).unwrap();
        // No log anywhere -> no candidate.
        assert_eq!(super::most_recent_player_log(&game_dir), None);
        let log = diagnostics.join("Player.log");
        std::fs::write(&log, "Local player Ripo6000\n").unwrap();
        // The game-dir Diagnostics folder is the crate's primary
        // Player.log location (launcher::diagnostic_log_path).
        assert_eq!(super::player_log_candidates(&game_dir)[0], log);
        assert_eq!(super::most_recent_player_log(&game_dir), Some(log));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sync_decision_matrix() {
        let hash = "9947d9ffd222d4eb10d180f947277dd3a3af3e46be31c8a29cf96823251cbde0";
        let other = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        // Mirror hash fetched, local matches -> nothing to do.
        assert_eq!(sync_decision(Some(hash), Some(hash)), SyncDecision::UpToDate);
        // Mirror hash fetched, local missing or stale -> download.
        assert_eq!(sync_decision(None, Some(hash)), SyncDecision::Download);
        assert_eq!(
            sync_decision(Some(other), Some(hash)),
            SyncDecision::Download
        );
        // No mirror hash (offline) -> keep whatever is installed.
        assert_eq!(
            sync_decision(Some(hash), None),
            SyncDecision::KeepInstalledNoMirror
        );
        assert_eq!(
            sync_decision(None, None),
            SyncDecision::KeepInstalledNoMirror
        );
    }

    #[test]
    fn budget_math_saturates_and_caps() {
        let budget = Duration::from_secs(180);
        assert_eq!(
            budget_remaining(budget, Duration::from_secs(30)),
            Duration::from_secs(150)
        );
        assert_eq!(
            budget_remaining(budget, Duration::from_secs(180)),
            Duration::ZERO
        );
        // Elapsed beyond the budget saturates at zero, never wraps.
        assert_eq!(
            budget_remaining(budget, Duration::from_secs(999)),
            Duration::ZERO
        );
        // The listen wait is capped by its own limit while budget lasts…
        assert_eq!(
            listen_wait_allowance(budget, Duration::from_secs(30)),
            super::LISTEN_WAIT
        );
        // …and by whatever budget remains once that runs lower.
        assert_eq!(
            listen_wait_allowance(budget, Duration::from_secs(175)),
            Duration::from_secs(5)
        );
        assert_eq!(
            listen_wait_allowance(budget, Duration::from_secs(999)),
            Duration::ZERO
        );
    }

    #[test]
    fn wait_loop_polls_until_listening() {
        let mut probes = 0_u32;
        let mut sleeps: Vec<Duration> = Vec::new();
        let ready = wait_until_listening_with(
            || {
                probes += 1;
                probes >= 3 // listening on the third probe
            },
            Duration::from_secs(10),
            Duration::from_millis(250),
            |step| sleeps.push(step),
        );
        assert!(ready);
        assert_eq!(probes, 3);
        // Slept exactly once between probes, never past the wait.
        assert_eq!(
            sleeps,
            vec![Duration::from_millis(250), Duration::from_millis(250)]
        );
    }

    #[test]
    fn wait_loop_gives_up_at_max_wait() {
        let mut probes = 0_u32;
        let mut sleeps: Vec<Duration> = Vec::new();
        let ready = wait_until_listening_with(
            || {
                probes += 1;
                false // never listens
            },
            Duration::from_millis(1000),
            Duration::from_millis(300),
            |step| sleeps.push(step),
        );
        assert!(!ready);
        assert_eq!(probes, 5);
        // The last step is clamped so the total wait is exactly the
        // allowance — the gate never overshoots its budget here.
        assert_eq!(
            sleeps,
            vec![
                Duration::from_millis(300),
                Duration::from_millis(300),
                Duration::from_millis(300),
                Duration::from_millis(100),
            ]
        );
        assert_eq!(
            sleeps.iter().sum::<Duration>(),
            Duration::from_millis(1000)
        );
    }

    #[test]
    fn wait_loop_immediate_listening_never_sleeps() {
        let mut sleeps: Vec<Duration> = Vec::new();
        let ready = wait_until_listening_with(
            || true,
            Duration::from_secs(10),
            Duration::from_millis(250),
            |step| sleeps.push(step),
        );
        assert!(ready);
        assert!(sleeps.is_empty());
        // Zero allowance + not listening: one probe, no sleep, give up.
        let ready = wait_until_listening_with(
            || false,
            Duration::ZERO,
            Duration::from_millis(250),
            |step| sleeps.push(step),
        );
        assert!(!ready);
        assert!(sleeps.is_empty());
    }

    #[test]
    fn gate_log_appends_lines_to_tachyon_log() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-gatelog-{}",
            std::process::id()
        ));
        let game_dir = dir.join("game");
        // The log lives next to where the server binary would live.
        assert_eq!(
            tachyon_log_path(&game_dir),
            game_dir.join("tachyon-server").join("tachyon.log")
        );
        gate_log(&game_dir, "[tachyon] readiness gate: first");
        gate_log(&game_dir, "[tachyon] readiness gate: second");
        let written = std::fs::read_to_string(tachyon_log_path(&game_dir)).unwrap();
        assert_eq!(
            written,
            "[tachyon] readiness gate: first\n[tachyon] readiness gate: second\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
