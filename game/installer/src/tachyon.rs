//! Tachyon background server (v1.0.83).
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
//! Running-server takeover (v1.0.82): the spawned server is detached
//! and outlives the launcher, so without a takeover a mirror update
//! would never take effect on a machine whose old server is still
//! running — the stale binary would serve forever (v1.0.80's fast
//! path returned before even syncing). The gate now records the
//! spawned server's PID in `tachyon-server.pid`; when the sync
//! brings a newer binary and the port is held by the process that
//! pidfile names — verified live, by image name — the gate stops
//! that one process and starts the updated binary instead. Servers
//! spawned by v1.0.80 or earlier left no pidfile, so a second proof
//! covers them: the process owning the port must be running an
//! image whose full path is exactly our server exe's path. A
//! listener that fails both proofs is never touched.
//!
//! Spawned-hash staleness (v1.0.83): the v1.0.82 takeover only fired
//! when the *same launch's* sync reported a fresh download, but
//! nothing recorded which binary the RUNNING server was started
//! from — so once the on-disk exe reached the mirror version while
//! an older process kept serving (a takeover whose proof failed,
//! a file replaced out-of-band), every later launch synced as
//! "unchanged", logged "already listening", and the stale process
//! served forever. The gate now also records the spawned binary's
//! sha256 in `tachyon-server.spawned-hash` at spawn time, and the
//! takeover decision is driven by `running_is_stale`: a listener
//! whose spawned record is missing (any pre-v1.0.83 server) or
//! disagrees with the on-disk or mirror hash is stale and goes
//! through the same ownership proofs on EVERY launch, whatever the
//! sync outcome was. A proof failure still leaves the listener
//! running — but now logs the full proof report (recorded PID,
//! image-name result, port owner, owner image path, expected exe
//! path, and all three hashes) so the next diagnosis is never
//! blind. Gate log lines also carry a UTC timestamp from v1.0.83.
//!
//! Everything here is fail-soft by design:
//! - a failed sidecar fetch keeps the installed exe untouched;
//! - a failed download never replaces a working exe (temp file, hash
//!   verified, then renamed into place);
//! - a busy UDP 7777 means a server is already running — nothing to
//!   do, unless the takeover above proves it is our own stale one;
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
/// Pidfile naming the server process the gate spawned, written next
/// to the binary. It is the takeover's ownership proof: a later
/// launch may replace a running server only when the PID recorded
/// here is still alive and still running our server image.
const SERVER_PID_NAME: &str = "tachyon-server.pid";
/// Spawned-hash record naming the binary the running server was
/// started from: the sha256 of the exe file at spawn time, written
/// next to the binary (v1.0.83). The pidfile proves WHO the
/// listener is; this record proves WHAT it is running — the
/// staleness half of the takeover decision (`running_is_stale`).
/// Servers spawned before v1.0.83 left no record, which the
/// staleness check treats as stale (their vintage is unknown).
const SERVER_SPAWNED_HASH_NAME: &str = "tachyon-server.spawned-hash";
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
/// How long the gate waits, after a takeover stop, for UDP 7777 to
/// free up before it installs + respawns the updated server
/// (further bounded by the remaining budget).
const TAKEOVER_FREE_WAIT: Duration = Duration::from_secs(5);
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

fn server_pid_file(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join(SERVER_PID_NAME)
}

/// Temp file a mirror download lands in before its hash verifies
/// and it is renamed over `server_exe`.
fn server_download_temp(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join(format!("{SERVER_EXE_NAME}.download"))
}

/// Parse pidfile contents: a plain decimal PID. Tolerant of the
/// whitespace a hand edit might add; zero, junk and overflow all
/// mean "no usable pid" — the takeover fails closed on any of them.
fn parse_server_pid(text: &str) -> Option<u32> {
    let pid: u32 = text.trim().parse().ok()?;
    (pid > 0).then_some(pid)
}

/// The PID recorded in the pidfile, if it holds a usable one.
fn read_server_pid(game_dir: &Path) -> Option<u32> {
    parse_server_pid(&std::fs::read_to_string(server_pid_file(game_dir)).ok()?)
}

/// Remember which process the gate spawned, so a later launch can
/// prove the listener on udp/7777 is that very process before
/// replacing it after a mirror update (the takeover in
/// `ensure_server_ready`). Best-effort, like every file write in
/// this module: a missing pidfile only ever disables the takeover,
/// never the launch.
fn write_server_pid(game_dir: &Path, pid: u32) {
    let _ = std::fs::create_dir_all(server_dir(game_dir));
    let _ = std::fs::write(server_pid_file(game_dir), pid.to_string());
}

fn server_spawned_hash_file(game_dir: &Path) -> PathBuf {
    server_dir(game_dir).join(SERVER_SPAWNED_HASH_NAME)
}

/// Parse spawned-hash contents: a 64-char hex sha256. Tolerant of
/// surrounding whitespace and of upper case (normalized to the
/// lowercase form `sha256_file` and the sidecar parser produce);
/// anything else means "no usable record" — which the staleness
/// check treats exactly like a missing file (stale).
fn parse_spawned_hash(text: &str) -> Option<String> {
    let token = text.trim().to_ascii_lowercase();
    (token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())).then_some(token)
}

/// The recorded hash of the binary the running server was spawned
/// from, if a usable record exists.
fn read_spawned_hash(game_dir: &Path) -> Option<String> {
    parse_spawned_hash(&std::fs::read_to_string(server_spawned_hash_file(game_dir)).ok()?)
}

/// Remember which binary the gate just spawned, so a later launch
/// can tell whether the listener on udp/7777 is still running that
/// binary or a stale one (`running_is_stale`). Best-effort, like
/// the pidfile: a missing record only ever makes the listener look
/// stale (and the ownership proofs still gate any replacement),
/// never blocks a launch.
fn write_spawned_hash(game_dir: &Path, hash: &str) {
    let _ = std::fs::create_dir_all(server_dir(game_dir));
    let _ = std::fs::write(server_spawned_hash_file(game_dir), hash);
}

/// Is the server listening on udp/7777 running a stale binary?
/// Pure, so the whole matrix is unit-tested.
///
/// - `spawned_hash`: the record written at spawn time — WHICH
///   binary the running process was started from.
/// - `disk_hash`: the exe on disk now (post-sync).
/// - `mirror_hash`: the hash the mirror currently serves, when
///   the sync could fetch it.
///
/// Stale when the spawned record is MISSING (any pre-v1.0.83
/// server: its vintage is unknown, so it cannot be trusted to be
/// current), or the spawned hash disagrees with any hash that is
/// known. Not stale only when a record exists and matches every
/// known hash — including the vacuous case where neither other
/// hash could be determined (offline first run after a spawn:
/// the record is all we have, and it is ours).
fn running_is_stale(
    spawned_hash: Option<&str>,
    disk_hash: Option<&str>,
    mirror_hash: Option<&str>,
) -> bool {
    let spawned = match spawned_hash {
        Some(hash) => hash,
        None => return true,
    };
    if let Some(disk) = disk_hash {
        if spawned != disk {
            return true;
        }
    }
    if let Some(mirror) = mirror_hash {
        if spawned != mirror {
            return true;
        }
    }
    false
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

/// What one mirror sync pass did to the exe on disk. The readiness
/// gate uses this for the pending-install step (`UpdatePending`)
/// and to leave a stale listener alone when the sync itself
/// `Failed`; since v1.0.83 the takeover's warrant is the listener's
/// staleness (`running_is_stale`), not this outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// The installed exe already matched the mirror, or the mirror
    /// was unreachable and the installed exe was kept: the binary
    /// on disk is exactly what it was before the sync.
    Unchanged,
    /// A different exe was downloaded and installed.
    Updated,
    /// A different exe was downloaded and hash-verified but could
    /// NOT be installed: the running server locks its own image on
    /// Windows, so the rename had to wait. The verified download
    /// stays in the `.download` temp file for
    /// `install_pending_server` (the takeover's second half).
    UpdatePending,
    /// The sync could not complete (client build failure, an early
    /// IO error, or a failed download): disk untouched.
    Failed,
}

/// Keep the bundled `TachyonServer.exe` in sync with the HF mirror:
/// fetch the `.sha256` sidecar and download the exe only when the
/// local copy is missing or its hash differs. If the sidecar fetch
/// fails (offline, mirror hiccup) and a local exe exists, it is kept
/// as-is — the same fail-soft contract as the plugin sync.
///
/// Returns the outcome AND the mirror hash the sync fetched
/// (`None` when the sidecar could not be fetched): the readiness
/// gate threads that hash into `running_is_stale`, so a running
/// server can be judged against what the mirror serves NOW, not
/// only against what this launch happened to download (v1.0.83).
pub(crate) async fn ensure_server(game_dir: &Path) -> (SyncOutcome, Option<String>) {
    let dir = server_dir(game_dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("[tachyon] cannot create {}: {error}", dir.display());
        return (SyncOutcome::Failed, None);
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
            return (SyncOutcome::Failed, None);
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
        SyncDecision::UpToDate => return (SyncOutcome::Unchanged, fetched), // already in sync
        SyncDecision::KeepInstalledNoMirror => {
            if exe.is_file() {
                println!("[tachyon] mirror unreachable — keeping installed server.");
            } else {
                eprintln!("[tachyon] mirror unreachable and no server installed yet.");
            }
            // `fetched` is None in this branch by construction (the
            // decision only lands here when no hash was fetched).
            return (SyncOutcome::Unchanged, fetched);
        }
        SyncDecision::Download => match fetched {
            Some(hash) => hash,
            // sync_decision only picks Download when a hash was
            // fetched; this arm is unreachable in practice.
            None => return (SyncOutcome::Failed, None),
        },
    };

    // Download to a temp name, verify the hash, then move into place:
    // a partial or corrupt download never replaces a working exe.
    println!("[tachyon] Downloading Tachyon server…");
    let temp = server_download_temp(game_dir);
    let url = format!("{MIRROR_BASE}{SERVER_EXE_NAME}");
    if let Err(error) = download_verified(&client, &url, &temp, &expected).await {
        eprintln!("[tachyon] server download failed: {error}");
        let _ = std::fs::remove_file(&temp);
        return (SyncOutcome::Failed, Some(expected));
    }
    if exe.is_file() {
        // Windows cannot replace a running exe: if the old server is
        // still up, this removal fails and the rename below with it.
        let _ = std::fs::remove_file(&exe);
    }
    if let Err(error) = std::fs::rename(&temp, &exe) {
        // Almost always the running server locking its own image.
        // The download is verified and stays in the temp file; the
        // gate's takeover stops the stale server and then installs
        // it via `install_pending_server`. Any other cause simply
        // defers the update to a later launch, as before.
        eprintln!("[tachyon] downloaded server not installed yet (exe busy?): {error}");
        return (SyncOutcome::UpdatePending, Some(expected));
    }
    println!("[tachyon] Tachyon server synced from mirror.");
    (SyncOutcome::Updated, Some(expected))
}

/// Install a download that an earlier sync pass (this same gate
/// run) verified but could not move into place because the running
/// server locked its exe. Retries the remove + rename from the
/// `.download` temp file; true when the new binary is in place
/// afterwards. The temp content was hash-verified by
/// `download_verified` before it was parked, so no second check
/// happens here.
fn install_pending_server(game_dir: &Path) -> bool {
    let temp = server_download_temp(game_dir);
    if !temp.is_file() {
        return false;
    }
    let exe = server_exe(game_dir);
    if exe.is_file() {
        let _ = std::fs::remove_file(&exe);
    }
    if std::fs::rename(&temp, &exe).is_err() {
        return false;
    }
    println!("[tachyon] Tachyon server updated from the downloaded build.");
    true
}

/// Whether a sync outcome means a newer server binary than the
/// one a running server was started from is now on disk (or, for
/// `UpdatePending`, verified and waiting in the temp file).
fn sync_warrants_takeover(outcome: SyncOutcome) -> bool {
    matches!(outcome, SyncOutcome::Updated | SyncOutcome::UpdatePending)
}

/// What the readiness gate should do about a listener on udp/7777
/// after the sync step — pure, so the whole decision matrix is
/// unit-tested with fixtures.
#[derive(Debug, PartialEq, Eq)]
enum TakeoverAction {
    /// Leave the listener alone: it already runs the current
    /// binary, or it is not provably the launcher's own server
    /// (missing pidfile, stale recorded PID, the PID now belongs
    /// to another program, and the port owner is not running our
    /// exe from its own path). A busy port keeps meaning "server
    /// ready", exactly as it did before the takeover existed.
    KeepRunning,
    /// Stop this PID — proven ours by an ownership proof (the
    /// pidfile's recorded process, still alive and still running
    /// our server image; or the port's owner, running an image
    /// whose path is exactly our server exe's) — so the gate can
    /// install the update (if it is still pending) and spawn the
    /// new binary.
    Replace(u32),
}

/// The pre-v1.0.83 decision, gated on this launch's sync outcome.
/// Retained as a wrapper so its test matrix keeps pinning the old
/// behaviour; the gate itself now decides via
/// `takeover_action_for_stale` (see `running_is_stale` for why the
/// outcome gate was a dead-end).
#[allow(dead_code)]
fn takeover_action(
    outcome: SyncOutcome,
    port_taken: bool,
    pidfile_pid: Option<u32>,
    image_name: Option<&str>,
) -> TakeoverAction {
    takeover_action_for_stale(
        sync_warrants_takeover(outcome),
        port_taken,
        pidfile_pid,
        image_name,
    )
}

/// The v1.0.83 pidfile-proof decision: identical in shape to
/// `takeover_action`, but the warrant is the listener's STALENESS
/// (`running_is_stale`) rather than this launch's sync outcome.
/// A stale listener goes through the proof on every launch —
/// including the launches where the sync changes nothing, which
/// is exactly the case the outcome gate could never recover from.
fn takeover_action_for_stale(
    stale: bool,
    port_taken: bool,
    pidfile_pid: Option<u32>,
    image_name: Option<&str>,
) -> TakeoverAction {
    if !port_taken || !stale {
        return TakeoverAction::KeepRunning;
    }
    let pid = match pidfile_pid {
        Some(pid) => pid,
        None => return TakeoverAction::KeepRunning,
    };
    match image_name {
        Some(name) if image_name_is_server(name) => TakeoverAction::Replace(pid),
        _ => TakeoverAction::KeepRunning,
    }
}

/// The fallback decision, for when the pidfile proof cannot speak:
/// servers spawned by v1.0.80 or earlier left no pidfile at all,
/// and a pidfile can also go stale. The gate has then found the
/// process OWNING the port (the caller only runs this with the
/// port taken); it may be replaced only when its image path is
/// exactly our server exe's path. Same fail-closed shape as
/// `takeover_action`: no update, no owner, or a foreign path all
/// keep the listener running.
/// The pre-v1.0.83 fallback decision, gated on the sync outcome.
/// Retained as a wrapper like `takeover_action`; the gate now
/// decides via `port_owner_action_for_stale`.
#[allow(dead_code)]
fn port_owner_action(
    outcome: SyncOutcome,
    owner_pid: Option<u32>,
    owner_image_path: Option<&str>,
    server_exe_path: &str,
) -> TakeoverAction {
    port_owner_action_for_stale(
        sync_warrants_takeover(outcome),
        owner_pid,
        owner_image_path,
        server_exe_path,
    )
}

/// The v1.0.83 fallback decision: same fail-closed shape as
/// `port_owner_action`, warranted by staleness instead of by this
/// launch's sync outcome.
fn port_owner_action_for_stale(
    stale: bool,
    owner_pid: Option<u32>,
    owner_image_path: Option<&str>,
    server_exe_path: &str,
) -> TakeoverAction {
    if !stale {
        return TakeoverAction::KeepRunning;
    }
    let pid = match owner_pid {
        Some(pid) => pid,
        None => return TakeoverAction::KeepRunning,
    };
    match owner_image_path {
        Some(path) if image_path_is_our_server(path, server_exe_path) => {
            TakeoverAction::Replace(pid)
        }
        _ => TakeoverAction::KeepRunning,
    }
}

/// Pull the image name out of `tasklist /FO LIST` output
/// (`Image Name:   TachyonServer.exe`). `None` when no such line
/// exists — e.g. the "No tasks are running" info line tasklist
/// prints for a dead PID. (Referenced from the Windows
/// `process_image_name` and from the tests; dead on other
/// non-test builds.)
#[allow(dead_code)]
fn parse_tasklist_image_name(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let name = line.trim().strip_prefix("Image Name:")?.trim();
        (!name.is_empty()).then(|| name.to_string())
    })
}

/// Does a tasklist image name denote our bundled server? Exact,
/// case-insensitive (Windows image names are): anything looser
/// could match an unrelated process, and the takeover only ever
/// stops a process it has proven is ours.
fn image_name_is_server(image_name: &str) -> bool {
    image_name.trim().eq_ignore_ascii_case(SERVER_EXE_NAME)
}

/// Pull the owning PID for `port` out of `netstat -ano -p UDP`
/// output. Rows look like:
///   `  UDP    127.0.0.1:7777    *:*                       1234`
/// (proto, local address, foreign address, PID). The local
/// address's port must match exactly — `:77770` is not `:7777`.
/// `None` when no row owns the port. (Referenced from the Windows
/// `udp_port_owner_pid` and from the tests; dead on other
/// non-test builds.)
#[allow(dead_code)]
fn parse_netstat_udp_owner(output: &str, port: u16) -> Option<u32> {
    let port_suffix = format!(":{port}");
    output.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || !fields[0].eq_ignore_ascii_case("UDP") {
            return None;
        }
        if !fields[1].ends_with(&port_suffix) {
            return None;
        }
        fields.last().and_then(|pid| parse_server_pid(pid))
    })
}

/// Parse what
/// `Get-NetUDPEndpoint … | Select-Object -ExpandProperty OwningProcess`
/// prints: one decimal PID per line. The first usable one wins —
/// a listener bound on several addresses reports its process once
/// per endpoint. (Same references/liveness as
/// `parse_netstat_udp_owner`.)
#[allow(dead_code)]
fn parse_udp_owner_pids(output: &str) -> Option<u32> {
    output.lines().find_map(parse_server_pid)
}

/// Normalize an executable path for comparison: Windows paths are
/// case-insensitive, accept either separator spelling, and tool
/// output may wrap the value in quotes or stray whitespace.
fn normalize_exe_path(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .replace('/', "\\")
        .to_lowercase()
}

/// Does `image_path` (a running process's full image path, e.g.
/// from `Get-Process`) point at OUR server binary — the exe in
/// the game's `tachyon-server` folder? Full-path equality after
/// normalization: a `TachyonServer.exe` anywhere else (a second
/// install, a renamed copy in the same folder) does NOT prove
/// the port owner is the server this launcher manages.
fn image_path_is_our_server(image_path: &str, server_exe_path: &str) -> bool {
    let candidate = normalize_exe_path(image_path);
    !candidate.is_empty() && candidate == normalize_exe_path(server_exe_path)
}

/// Image name of the process with `pid`, via tasklist — the same
/// tool `spawn_agent` already shells out to for its duplicate
/// check. `None` when the process is gone or tasklist failed.
#[cfg(windows)]
fn process_image_name(pid: u32) -> Option<String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "LIST"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_tasklist_image_name(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(windows))]
fn process_image_name(_pid: u32) -> Option<String> {
    // tasklist is a Windows tool, and the bundled server is a
    // Windows binary: other platforms have nothing to inspect, so
    // the takeover simply never fires there.
    None
}

/// PID of the process owning UDP `port`, if any. PowerShell's
/// `Get-NetUDPEndpoint` first; `netstat -ano -p UDP` is the
/// fallback for systems where the cmdlet is missing or slow to
/// load. Only a candidate finder — the caller still has to prove
/// the PID is our server (image path) before acting on it.
#[cfg(windows)]
fn udp_port_owner_pid(port: u16) -> Option<u32> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        "Get-NetUDPEndpoint -LocalPort {port} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess"
    );
    let via_cmdlet = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .and_then(|out| parse_udp_owner_pids(&String::from_utf8_lossy(&out.stdout)));
    if via_cmdlet.is_some() {
        return via_cmdlet;
    }
    let output = std::process::Command::new("netstat")
        .args(["-ano", "-p", "UDP"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_netstat_udp_owner(&String::from_utf8_lossy(&output.stdout), port)
}

#[cfg(not(windows))]
fn udp_port_owner_pid(_port: u16) -> Option<u32> {
    // The endpoint cmdlets and netstat's `-ano` shape are Windows
    // tools: other platforms have nothing to inspect, so the
    // fallback proof simply never fires there.
    None
}

/// Full image path of the process with `pid`, via PowerShell's
/// `Get-Process` (the supported successor to `wmic process`).
/// `None` when the process is gone, its path is not readable, or
/// PowerShell failed.
#[cfg(windows)]
fn process_image_path(pid: u32) -> Option<String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!("(Get-Process -Id {pid} -ErrorAction SilentlyContinue).Path");
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(not(windows))]
fn process_image_path(_pid: u32) -> Option<String> {
    None
}

/// Force-stop the process with `pid` (`taskkill /F`). Only ever
/// called with a PID an ownership proof (`takeover_action` or
/// `port_owner_action`) has proven is our own stale server. True
/// when taskkill reports success.
#[cfg(windows)]
fn terminate_process(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn terminate_process(_pid: u32) -> bool {
    false
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

/// How long the gate may wait for the port to free up after a
/// takeover stop: the takeover cap, further bounded by the launch
/// budget left over (same shape as `listen_wait_allowance`).
fn port_free_wait_allowance(budget: Duration, elapsed: Duration) -> Duration {
    TAKEOVER_FREE_WAIT.min(budget_remaining(budget, elapsed))
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

/// The gate's generic poll loop with the probe inverted: poll
/// until UDP 7777 can be bound again (bounded by `max_wait`). The
/// tail of a successful takeover stop — `taskkill` returning does
/// not guarantee the socket is released yet.
fn wait_until_port_free(max_wait: Duration) -> bool {
    wait_until_listening_with(
        || udp_port_free(SERVER_PORT),
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

/// Days since the Unix epoch → (year, month, day) in the
/// proleptic Gregorian calendar (Howard Hinnant's civil-from-days
/// algorithm). Pure integer math, so the timestamp formatter needs
/// no date crate — this crate deliberately carries no chrono/time
/// dependency for one log prefix.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era, [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // year of era, [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year, [0, 365]
    let mp = (5 * doy + 2) / 153; // month index, March = 0, [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format Unix seconds as an ISO-8601 UTC timestamp
/// (`2026-11-08T17:33:44Z`). Pure, so the conversion is
/// unit-tested against known vectors (epoch, leap days).
fn format_utc_from_unix(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// The current UTC time, formatted by `format_utc_from_unix`. A
/// clock reading from before the epoch (never in practice) clamps
/// to the epoch instead of failing — a log prefix must not be able
/// to break the gate.
fn utc_now_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_utc_from_unix(secs)
}

/// Emit one readiness-gate outcome line: to stdout, like every
/// launcher log line, AND appended to `tachyon.log` — on Windows the
/// stdout is gone once the launcher exits, while the file is the
/// artifact support can ask for when a launch misbehaves. Strictly
/// best-effort: a failed file write never affects the gate.
///
/// Every line carries a UTC timestamp prefix (v1.0.83):
/// `tachyon.log` accumulates across launches and server swaps, and
/// an undated line cannot be tied to the launch that produced it —
/// the exact ambiguity that made the v4/v5 server mix-up hard to
/// prove from the log alone.
fn gate_log(game_dir: &Path, line: &str) {
    let stamped = format!("{} {line}", utc_now_timestamp());
    println!("{stamped}");
    if std::fs::create_dir_all(server_dir(game_dir)).is_ok() {
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(tachyon_log_path(game_dir))
            .and_then(|mut file| writeln!(file, "{stamped}"));
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
/// v1.0.82 added the takeover step; v1.0.83 re-drives it by
/// staleness instead of by the sync outcome, and with it a second
/// invariant:
/// **a mirror update reaches the running server — on the launch
/// that brings it, or on any later launch that finds the running
/// server stale — but the gate only ever stops a process an
/// ownership proof vouches for (the pidfile record, or owning the
/// port with our exe's exact image path); any other listener is
/// never touched and keeps meaning "server ready".**
///
/// In order: (a) sync `TachyonServer.exe` with the mirror,
/// synchronously — a first run may spend a while on the 67 MB
/// download, with progress lines, instead of the game freezing at
/// 18% against a dead endpoint; (a2) if a server from an earlier
/// session still holds the port (it is detached and outlives the
/// launcher), judge it with `running_is_stale` — spawned-hash
/// record vs the post-sync disk hash and the mirror hash the sync
/// fetched. Not stale: the gate returns ready exactly as the old
/// fast path did. Stale: take over on ANY completed sync outcome
/// (`Unchanged` included — that is the v1.0.83 fix), stopping the
/// proven stale process (pidfile proof, or the port-owner path
/// proof for pre-record servers), installing the update if one is
/// still pending, and falling through to the spawn step with the
/// port free. A stale listener that fails both proofs is left
/// alone and the gate returns ready — but the full proof report
/// is logged first. A stale listener whose sync FAILED is also
/// left alone (nothing new was learned about the mirror; the
/// next launch retries with a completed sync);
/// (b) spawn
/// the server hidden and detached if the port is free (the
/// existing `ensure_running`), recording its PID in the pidfile;
/// (c) poll until the port is actually taken.
///
/// Returns true when a server is listening. Every failure path
/// returns false and NEVER propagates an error: worst case the
/// launch behaves exactly like the pre-Tachyon launcher.
pub(crate) fn ensure_server_ready(game_dir: &Path) -> bool {
    let started = Instant::now();
    if !udp_port_free(SERVER_PORT) {
        // No more unconditional fast path (that was the v1.0.80
        // stale-server trap): the sync still has to run so a mirror
        // update is at least discovered — and can then take over.
        gate_log(
            game_dir,
            "[tachyon] readiness gate: a server is listening on udp/7777 — checking the mirror before trusting it",
        );
    } else {
        gate_log(game_dir, "[tachyon] readiness gate: syncing background server…");
    }

    // (a) Sync the binary, synchronously, inside the launch budget.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[tachyon] readiness gate: could not start async runtime: {error}");
            if !udp_port_free(SERVER_PORT) {
                gate_log(
                    game_dir,
                    "[tachyon] readiness gate: server already listening on udp/7777",
                );
                return true;
            }
            return false;
        }
    };
    let sync_budget = budget_remaining(READY_BUDGET, started.elapsed());
    let (outcome, mirror_hash) = match runtime
        .block_on(async { tokio::time::timeout(sync_budget, ensure_server(game_dir)).await })
    {
        Ok(pair) => pair,
        Err(_) => {
            // The dropped sync may leave a partial `.download` temp
            // file; the next launch's download truncates and
            // rewrites it.
            if !udp_port_free(SERVER_PORT) {
                gate_log(
                    game_dir,
                    "[tachyon] readiness gate: sync exceeded the launch budget, but a server is already listening — launching armed",
                );
                return true;
            }
            gate_log(
                game_dir,
                "[tachyon] readiness gate: sync exceeded the launch budget — launching unarmed",
            );
            return false;
        }
    };
    gate_log(
        game_dir,
        match outcome {
            SyncOutcome::Updated => {
                "[tachyon] readiness gate: sync done — server binary updated from the mirror"
            }
            SyncOutcome::UpdatePending => {
                "[tachyon] readiness gate: sync done — update downloaded, install waiting on the running server"
            }
            SyncOutcome::Unchanged => {
                "[tachyon] readiness gate: sync done — server binary unchanged"
            }
            SyncOutcome::Failed => "[tachyon] readiness gate: sync done — sync failed",
        },
    );

    // (a2) Staleness + takeover (v1.0.83): a leftover server holds
    // the port. Judge it by `running_is_stale` — the spawned-hash
    // record against the post-sync disk hash and the mirror hash
    // the sync fetched — NOT by whether this launch's sync
    // downloaded anything (the v1.0.82 outcome gate dead-ended
    // permanently once the disk reached the mirror version under
    // a still-running older process). A stale listener is replaced
    // ONLY when an ownership proof vouches for it: primarily the
    // pidfile (the process an earlier gate spawned — recorded PID,
    // still alive, still our server image); as a fallback for
    // servers spawned before the pidfile existed (v1.0.80 and
    // earlier), the port's owner running an image whose path is
    // exactly our exe's path. Anything else keeps the pre-takeover
    // behavior: a busy port means a server is ready.
    if !udp_port_free(SERVER_PORT) {
        let spawned_hash = read_spawned_hash(game_dir);
        let disk_hash = sha256_file(&server_exe(game_dir));
        let stale = running_is_stale(
            spawned_hash.as_deref(),
            disk_hash.as_deref(),
            mirror_hash.as_deref(),
        );
        if !stale {
            gate_log(
                game_dir,
                "[tachyon] readiness gate: server already listening on udp/7777",
            );
            return true;
        }
        gate_log(
            game_dir,
            &format!(
                "[tachyon] staleness: the running server is STALE (spawned={} disk={} mirror={}) — checking ownership proofs",
                spawned_hash.as_deref().unwrap_or("none"),
                disk_hash.as_deref().unwrap_or("none"),
                mirror_hash.as_deref().unwrap_or("none"),
            ),
        );
        if outcome == SyncOutcome::Failed {
            // The sync learned nothing new (client build failure,
            // early IO error, failed download): leave the stale
            // listener serving rather than restarting it onto a
            // binary state this launch could not verify. The next
            // launch retries with a completed sync.
            gate_log(
                game_dir,
                "[tachyon] takeover: the sync failed, so the stale listener is left running this launch",
            );
            gate_log(
                game_dir,
                "[tachyon] readiness gate: server already listening on udp/7777",
            );
            return true;
        }
        let pidfile_pid = read_server_pid(game_dir);
        let image_name = pidfile_pid.and_then(process_image_name);
        let primary =
            takeover_action_for_stale(true, true, pidfile_pid, image_name.as_deref());
        // The port-owner fallback doubles as the proof report's
        // evidence, so it is gathered whenever the pidfile proof
        // declines — its own verdict may decline too.
        let mut owner_pid: Option<u32> = None;
        let mut owner_path: Option<String> = None;
        let action = if primary != TakeoverAction::KeepRunning {
            primary
        } else {
            owner_pid = udp_port_owner_pid(SERVER_PORT);
            owner_path = owner_pid.and_then(process_image_path);
            let fallback = port_owner_action_for_stale(
                true,
                owner_pid,
                owner_path.as_deref(),
                &server_exe(game_dir).to_string_lossy(),
            );
            if fallback != TakeoverAction::KeepRunning {
                gate_log(
                    game_dir,
                    "[tachyon] takeover: no usable pidfile proof — the port owner runs our exe from its own path",
                );
            }
            fallback
        };
        match action {
            TakeoverAction::Replace(pid) => {
                gate_log(
                    game_dir,
                    &format!(
                        "[tachyon] takeover: udp/7777 is held by our stale server (pid {pid}) — restarting it on the current binary"
                    ),
                );
                if terminate_process(pid) {
                    let allowance = port_free_wait_allowance(READY_BUDGET, started.elapsed());
                    if wait_until_port_free(allowance) {
                        gate_log(
                            game_dir,
                            &format!(
                                "[tachyon] takeover: stale server pid {pid} stopped — udp/7777 is free"
                            ),
                        );
                        if outcome == SyncOutcome::UpdatePending && install_pending_server(game_dir)
                        {
                            gate_log(
                                game_dir,
                                "[tachyon] takeover: updated server binary installed",
                            );
                        }
                    } else {
                        gate_log(
                            game_dir,
                            "[tachyon] takeover: udp/7777 stayed busy after the stop — leaving the listener in place",
                        );
                    }
                } else {
                    gate_log(
                        game_dir,
                        &format!(
                            "[tachyon] takeover: could not stop pid {pid} — leaving the running server in place"
                        ),
                    );
                }
                // Fall through: the spawn step below starts the
                // current binary if the port freed up (recording
                // the pidfile AND the spawned-hash), and no-ops
                // (port still taken) if it did not.
            }
            TakeoverAction::KeepRunning => {
                // Fail-closed as ever — but never blind again: the
                // full proof report goes to tachyon.log, so the
                // next diagnosis starts from evidence, not guesses.
                gate_log(
                    game_dir,
                    &format!(
                        "[tachyon] takeover proof report: the listener on udp/7777 is stale but NOT provably ours — leaving it running | pidfile_pid={} image_name={} owner_pid={} owner_image_path={} expected_exe={} spawned_hash={} disk_hash={} mirror_hash={}",
                        pidfile_pid.map(|pid| pid.to_string()).as_deref().unwrap_or("none"),
                        image_name.as_deref().unwrap_or("none"),
                        owner_pid.map(|pid| pid.to_string()).as_deref().unwrap_or("none"),
                        owner_path.as_deref().unwrap_or("none"),
                        server_exe(game_dir).display(),
                        spawned_hash.as_deref().unwrap_or("none"),
                        disk_hash.as_deref().unwrap_or("none"),
                        mirror_hash.as_deref().unwrap_or("none"),
                    ),
                );
                gate_log(
                    game_dir,
                    "[tachyon] readiness gate: server already listening on udp/7777",
                );
                return true;
            }
        }
    }

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
/// "already running" signal. A successful spawn is recorded in the
/// pidfile (the takeover's ownership proof) AND in the spawned-hash
/// record (the takeover's staleness proof, v1.0.83: the sha256 of
/// the exe file at spawn time), and its PID returned; `None` when
/// nothing was spawned. If the exe cannot be hashed the spawn still
/// counts — the missing record just makes the listener look stale
/// to the next launch, where the ownership proofs still gate any
/// replacement.
pub(crate) fn ensure_running(game_dir: &Path) -> Option<u32> {
    let exe = server_exe(game_dir);
    if !exe.is_file() {
        return None;
    }
    if !udp_port_free(SERVER_PORT) {
        return None; // already running (or another listener owns the port)
    }
    let pid = spawn_server(&exe)?;
    write_server_pid(game_dir, pid);
    if let Some(hash) = sha256_file(&exe) {
        write_spawned_hash(game_dir, &hash);
    }
    Some(pid)
}

/// True when nothing is bound to UDP 127.0.0.1:`port`.
fn udp_port_free(port: u16) -> bool {
    std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
}

/// Spawn the server binary; returns the child PID for the pidfile.
/// The `Child` handle itself is deliberately dropped: the server is
/// detached and must outlive the launcher, not be reaped by it.
#[cfg(windows)]
fn spawn_server(exe: &Path) -> Option<u32> {
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
        Ok(child) => {
            println!("[tachyon] background server started on udp/{SERVER_PORT}.");
            Some(child.id())
        }
        Err(error) => {
            eprintln!("[tachyon] could not start server: {error}");
            None
        }
    }
}

#[cfg(not(windows))]
fn spawn_server(_exe: &Path) -> Option<u32> {
    // The bundled server binary is Windows-only; other platforms have
    // nothing to spawn. No-op so the launch flow (and its tests) stays
    // identical in Linux CI checks.
    None
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
        budget_remaining, format_utc_from_unix, gate_log, image_name_is_server,
        image_path_is_our_server, install_pending_server, listen_wait_allowance,
        parse_netstat_udp_owner, parse_server_pid, parse_sidecar_hash, parse_spawned_hash,
        parse_tasklist_image_name, parse_udp_owner_pids, port_free_wait_allowance,
        port_owner_action, port_owner_action_for_stale, read_server_pid, read_spawned_hash,
        running_is_stale, server_download_temp, server_exe, server_pid_file,
        server_spawned_hash_file, sha256_file, sync_decision, takeover_action,
        takeover_action_for_stale, tachyon_log_path, udp_port_free, wait_until_listening_with,
        write_server_pid, write_spawned_hash, SyncDecision, SyncOutcome, TakeoverAction,
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
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2);
        // v1.0.83: every line carries a UTC timestamp prefix
        // (`2026-11-08T17:33:44Z `, 21 chars) ahead of the message.
        for (line, message) in lines.iter().zip([
            "[tachyon] readiness gate: first",
            "[tachyon] readiness gate: second",
        ]) {
            assert_eq!(line.len(), 21 + message.len());
            assert!(line.ends_with(message));
            let stamp = &line[..20];
            assert_eq!(&stamp[4..5], "-");
            assert_eq!(&stamp[7..8], "-");
            assert_eq!(&stamp[10..11], "T");
            assert_eq!(&stamp[13..14], ":");
            assert_eq!(&stamp[16..17], ":");
            assert_eq!(&stamp[19..20], "Z");
            assert_eq!(&line[20..21], " ");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn utc_timestamp_format_matches_known_vectors() {
        // Independently computed vectors (system `date -u`).
        assert_eq!(format_utc_from_unix(0), "1970-01-01T00:00:00Z");
        // Leap days: 2000 (divisible by 400) and 2024.
        assert_eq!(format_utc_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc_from_unix(1_709_208_000), "2024-02-29T12:00:00Z");
        assert_eq!(format_utc_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
        // The v5 Tachyon server ship moment.
        assert_eq!(format_utc_from_unix(1_794_159_224), "2026-11-08T17:33:44Z");
        // Day/month/year boundaries roll over cleanly.
        assert_eq!(format_utc_from_unix(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(format_utc_from_unix(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn running_is_stale_full_matrix() {
        let current = "9947d9ffd222d4eb10d180f947277dd3a3af3e46be31c8a29cf96823251cbde0";
        let older = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        // Missing record -> stale, whatever else is known: any
        // pre-v1.0.83 server (or a lost record) has unknown vintage.
        assert!(running_is_stale(None, Some(current), Some(current)));
        assert!(running_is_stale(None, Some(current), None));
        assert!(running_is_stale(None, None, Some(current)));
        assert!(running_is_stale(None, None, None));
        // Record matches every known hash -> NOT stale. Includes
        // the vacuous case (neither other hash determinable).
        assert!(!running_is_stale(Some(current), Some(current), Some(current)));
        assert!(!running_is_stale(Some(current), Some(current), None));
        assert!(!running_is_stale(Some(current), None, Some(current)));
        assert!(!running_is_stale(Some(current), None, None));
        // Disk mismatch -> stale (the out-of-band / manual sync
        // case: disk reached the mirror under a running old build).
        assert!(running_is_stale(Some(older), Some(current), Some(current)));
        assert!(running_is_stale(Some(older), Some(current), None));
        // Mirror mismatch -> stale (mirror moved past both the
        // running build and the not-yet-updated disk).
        assert!(running_is_stale(Some(older), Some(older), Some(current)));
        assert!(running_is_stale(Some(older), None, Some(current)));
        // One matching hash never rescues a mismatching other one.
        assert!(running_is_stale(Some(current), Some(current), Some(older)));
        assert!(running_is_stale(Some(current), Some(older), Some(current)));
    }

    #[test]
    fn spawned_hash_record_round_trips_and_parses() {
        let hash = "2ba4452223a69eb3dc21906ad590f208f2305efea02afbf8601224c9fbe4fd34";
        // Parse: exact, whitespace-tolerant, case-normalizing;
        // anything else is "no record" (which reads as stale).
        assert_eq!(parse_spawned_hash(hash), Some(hash.to_string()));
        assert_eq!(parse_spawned_hash(&format!("  {hash}\n")), Some(hash.to_string()));
        assert_eq!(
            parse_spawned_hash(&hash.to_uppercase()),
            Some(hash.to_string())
        );
        assert_eq!(parse_spawned_hash(""), None);
        assert_eq!(parse_spawned_hash("not-a-hash"), None);
        assert_eq!(parse_spawned_hash(&hash[..63]), None);
        // File round-trip through the tachyon-server folder.
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-spawned-{}",
            std::process::id()
        ));
        let game_dir = dir.join("game");
        assert_eq!(
            server_spawned_hash_file(&game_dir),
            game_dir
                .join("tachyon-server")
                .join("tachyon-server.spawned-hash")
        );
        assert_eq!(read_spawned_hash(&game_dir), None);
        write_spawned_hash(&game_dir, hash);
        assert_eq!(read_spawned_hash(&game_dir).as_deref(), Some(hash));
        // Garbage content reads as no record, like a missing file.
        std::fs::write(server_spawned_hash_file(&game_dir), "garbage").unwrap();
        assert_eq!(read_spawned_hash(&game_dir), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_takeover_decision_matrix() {
        let ours_path = "C:\\Games\\FluxRec\\tachyon-server\\TachyonServer.exe";
        let pid = Some(1234);
        let ours_name = Some("TachyonServer.exe");
        // Stale + full pidfile proof -> replace, on ANY sync
        // outcome: the gate passes `stale` here, never the outcome,
        // which is the whole v1.0.83 fix (an Unchanged sync used to
        // dead-end this exact case).
        assert_eq!(
            takeover_action_for_stale(true, true, pid, ours_name),
            TakeoverAction::Replace(1234)
        );
        // Not stale -> never replace, however clean the proof.
        assert_eq!(
            takeover_action_for_stale(false, true, pid, ours_name),
            TakeoverAction::KeepRunning
        );
        // Port free -> nothing to take over.
        assert_eq!(
            takeover_action_for_stale(true, false, pid, ours_name),
            TakeoverAction::KeepRunning
        );
        // Stale but the proof fails -> keep running (fail-closed).
        assert_eq!(
            takeover_action_for_stale(true, true, None, None),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            takeover_action_for_stale(true, true, pid, Some("other.exe")),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            takeover_action_for_stale(true, true, pid, None),
            TakeoverAction::KeepRunning
        );
        // The fallback follows the same stale warrant.
        assert_eq!(
            port_owner_action_for_stale(true, Some(4321), Some(ours_path), ours_path),
            TakeoverAction::Replace(4321)
        );
        assert_eq!(
            port_owner_action_for_stale(false, Some(4321), Some(ours_path), ours_path),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            port_owner_action_for_stale(true, None, Some(ours_path), ours_path),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            port_owner_action_for_stale(
                true,
                Some(4321),
                Some("C:\\Windows\\System32\\svchost.exe"),
                ours_path
            ),
            TakeoverAction::KeepRunning
        );
    }

    #[test]
    fn pidfile_parse_accepts_only_a_plain_decimal_pid() {
        assert_eq!(parse_server_pid("1234"), Some(1234));
        // Whitespace/newlines from hand edits or other writers are fine.
        assert_eq!(parse_server_pid("  98765\n"), Some(98765));
        // Everything else fails closed: the takeover must never act
        // on a PID it cannot read exactly.
        assert_eq!(parse_server_pid(""), None);
        assert_eq!(parse_server_pid("   \n"), None);
        assert_eq!(parse_server_pid("abc"), None);
        assert_eq!(parse_server_pid("12.5"), None);
        assert_eq!(parse_server_pid("12 34"), None);
        assert_eq!(parse_server_pid("-42"), None);
        assert_eq!(parse_server_pid("0"), None);
        assert_eq!(parse_server_pid("99999999999999999999999999"), None);
    }

    #[test]
    fn pidfile_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-pidfile-{}",
            std::process::id()
        ));
        let game_dir = dir.join("game");
        // The pidfile lives in the tachyon-server folder.
        assert_eq!(
            server_pid_file(&game_dir),
            game_dir.join("tachyon-server").join("tachyon-server.pid")
        );
        // Missing file -> no recorded server.
        assert_eq!(read_server_pid(&game_dir), None);
        write_server_pid(&game_dir, 4242);
        assert_eq!(
            std::fs::read_to_string(server_pid_file(&game_dir)).unwrap(),
            "4242"
        );
        assert_eq!(read_server_pid(&game_dir), Some(4242));
        // A later spawn overwrites the record.
        write_server_pid(&game_dir, 7777);
        assert_eq!(read_server_pid(&game_dir), Some(7777));
        // Garbage content fails closed, like a missing file.
        std::fs::write(server_pid_file(&game_dir), "not-a-pid").unwrap();
        assert_eq!(read_server_pid(&game_dir), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tasklist_list_output_yields_the_image_name() {
        // Fixture shaped like real `tasklist /FO LIST` output.
        let listing = "\r\nImage Name:   TachyonServer.exe\r\nPID:          1234\r\nSession Name: Console\r\nSession#:     1\r\nMem Usage:    42,000 K\r\n";
        assert_eq!(
            parse_tasklist_image_name(listing).as_deref(),
            Some("TachyonServer.exe")
        );
        // A dead PID prints an info line, not a listing.
        assert_eq!(
            parse_tasklist_image_name(
                "INFO: No tasks are running which match the specified criteria."
            ),
            None
        );
        assert_eq!(parse_tasklist_image_name(""), None);
    }

    #[test]
    fn server_image_name_match_is_exact_but_case_insensitive() {
        assert!(image_name_is_server("TachyonServer.exe"));
        assert!(image_name_is_server("tachyonserver.EXE"));
        assert!(image_name_is_server("  TachyonServer.exe  "));
        // Anything that is not exactly our binary must NOT match —
        // the takeover kills whatever this accepts.
        assert!(!image_name_is_server("NotTachyonServer.exe"));
        assert!(!image_name_is_server("TachyonServer2.exe"));
        assert!(!image_name_is_server("TachyonServer.exe.bak"));
        assert!(!image_name_is_server("playit.exe"));
        assert!(!image_name_is_server(""));
    }

    #[test]
    fn takeover_decision_matrix() {
        let pid = Some(1234);
        let ours = Some("TachyonServer.exe");
        let other = Some("something-else.exe");
        // The full proof: update ready + port held + pidfile PID
        // alive and still running our image -> replace exactly it.
        // (Updated: install already landed; UpdatePending: the
        // verified download waits in the temp file.)
        for outcome in [SyncOutcome::Updated, SyncOutcome::UpdatePending] {
            assert_eq!(
                takeover_action(outcome, true, pid, ours),
                TakeoverAction::Replace(1234)
            );
        }
        // No newer binary -> never take over, however clean the proof.
        for outcome in [SyncOutcome::Unchanged, SyncOutcome::Failed] {
            assert_eq!(
                takeover_action(outcome, true, pid, ours),
                TakeoverAction::KeepRunning
            );
        }
        // Port free -> nothing to take over (the spawn step handles it).
        assert_eq!(
            takeover_action(SyncOutcome::Updated, false, pid, ours),
            TakeoverAction::KeepRunning
        );
        // Update ready but the listener is not provably ours:
        // no pidfile, a foreign image, or a stale PID whose process
        // is gone (or whose slot another program now owns).
        assert_eq!(
            takeover_action(SyncOutcome::Updated, true, None, None),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            takeover_action(SyncOutcome::Updated, true, pid, other),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            takeover_action(SyncOutcome::Updated, true, pid, None),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            takeover_action(SyncOutcome::UpdatePending, true, None, ours),
            TakeoverAction::KeepRunning
        );
    }

    #[test]
    fn port_free_wait_allowance_matches_the_listen_shape() {
        let budget = Duration::from_secs(180);
        // Capped by the takeover limit while budget lasts…
        assert_eq!(
            port_free_wait_allowance(budget, Duration::from_secs(30)),
            super::TAKEOVER_FREE_WAIT
        );
        // …and by the remaining budget once that runs lower.
        assert_eq!(
            port_free_wait_allowance(budget, Duration::from_secs(178)),
            Duration::from_secs(2)
        );
        assert_eq!(
            port_free_wait_allowance(budget, Duration::from_secs(999)),
            Duration::ZERO
        );
    }

    #[test]
    fn pending_install_moves_the_verified_temp_into_place() {
        let dir = std::env::temp_dir().join(format!(
            "fluxrec-tachyon-pending-{}",
            std::process::id()
        ));
        let game_dir = dir.join("game");
        std::fs::create_dir_all(super::server_dir(&game_dir)).unwrap();
        // No temp parked -> nothing to install.
        assert!(!install_pending_server(&game_dir));
        // Stale exe in place + verified temp parked by the sync.
        std::fs::write(server_exe(&game_dir), b"old-binary").unwrap();
        std::fs::write(server_download_temp(&game_dir), b"new-binary").unwrap();
        assert!(install_pending_server(&game_dir));
        assert_eq!(
            std::fs::read(server_exe(&game_dir)).unwrap(),
            b"new-binary"
        );
        // The temp is consumed by the rename; a second call no-ops.
        assert!(!server_download_temp(&game_dir).exists());
        assert!(!install_pending_server(&game_dir));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn netstat_udp_output_yields_the_port_owner() {
        // Fixture shaped like real `netstat -ano -p UDP` output.
        let listing = "Active Connections\r\n\r\n  Proto  Local Address          Foreign Address        PID\r\n  UDP    0.0.0.0:500            *:*                    1180\r\n  UDP    127.0.0.1:7777         *:*                    4321\r\n  UDP    127.0.0.1:77770        *:*                    9999\r\n  UDP    [::]:5353              *:*                    1180\r\n";
        assert_eq!(parse_netstat_udp_owner(listing, 7777), Some(4321));
        // Other ports resolve to their own owners…
        assert_eq!(parse_netstat_udp_owner(listing, 500), Some(1180));
        // …and an unowned port, a truncated row, and empty output
        // all fail closed.
        assert_eq!(parse_netstat_udp_owner(listing, 7778), None);
        assert_eq!(parse_netstat_udp_owner("  UDP    127.0.0.1:7777\r\n", 7777), None);
        assert_eq!(parse_netstat_udp_owner("", 7777), None);
    }

    #[test]
    fn powershell_owner_output_parses_the_first_pid() {
        // What `Get-NetUDPEndpoint … -ExpandProperty OwningProcess`
        // prints: one decimal PID per line.
        assert_eq!(parse_udp_owner_pids("4321\r\n"), Some(4321));
        assert_eq!(parse_udp_owner_pids("  4321 \n"), Some(4321));
        // A listener on several addresses repeats its PID; the
        // first usable line wins, junk lines are skipped.
        assert_eq!(parse_udp_owner_pids("4321\n4321\n"), Some(4321));
        assert_eq!(parse_udp_owner_pids("\n4321\n"), Some(4321));
        assert_eq!(parse_udp_owner_pids(""), None);
        assert_eq!(parse_udp_owner_pids("no process\n"), None);
        assert_eq!(parse_udp_owner_pids("0\n"), None);
    }

    #[test]
    fn image_path_match_requires_our_exact_exe_path() {
        let ours = "C:\\Games\\FluxRec\\tachyon-server\\TachyonServer.exe";
        assert!(image_path_is_our_server(ours, ours));
        // Case and separator spelling are not significant on
        // Windows, and neither is tool-added quoting/whitespace.
        assert!(image_path_is_our_server(
            "c:\\games\\fluxrec\\tachyon-server\\tachyonserver.exe",
            ours
        ));
        assert!(image_path_is_our_server(
            "C:/Games/FluxRec/tachyon-server/TachyonServer.exe",
            ours
        ));
        assert!(image_path_is_our_server(
            "  \"C:\\Games\\FluxRec\\tachyon-server\\TachyonServer.exe\"  ",
            ours
        ));
        // Anything else must NOT match — the takeover kills
        // whatever this accepts: another install, a near-name in
        // the same folder, the agent, an empty reading.
        assert!(!image_path_is_our_server(
            "D:\\Other\\tachyon-server\\TachyonServer.exe",
            ours
        ));
        assert!(!image_path_is_our_server(
            "C:\\Games\\FluxRec\\tachyon-server\\TachyonServer2.exe",
            ours
        ));
        assert!(!image_path_is_our_server(
            "C:\\Games\\FluxRec\\tachyon-server\\playit.exe",
            ours
        ));
        assert!(!image_path_is_our_server("", ours));
    }

    #[test]
    fn port_owner_decision_matrix() {
        let ours_path = "C:\\Games\\FluxRec\\tachyon-server\\TachyonServer.exe";
        let other_path = Some("C:\\Windows\\System32\\svchost.exe");
        let owner = Some(4321);
        // The fallback proof: update ready + port owner found +
        // its image path is exactly our exe's -> replace exactly it.
        for outcome in [SyncOutcome::Updated, SyncOutcome::UpdatePending] {
            assert_eq!(
                port_owner_action(outcome, owner, Some(ours_path), ours_path),
                TakeoverAction::Replace(4321)
            );
        }
        // No newer binary -> never take over, however clean the proof.
        for outcome in [SyncOutcome::Unchanged, SyncOutcome::Failed] {
            assert_eq!(
                port_owner_action(outcome, owner, Some(ours_path), ours_path),
                TakeoverAction::KeepRunning
            );
        }
        // The owner is another program, cannot be found, or its
        // path cannot be read: fail closed in every combination.
        assert_eq!(
            port_owner_action(SyncOutcome::Updated, owner, other_path, ours_path),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            port_owner_action(SyncOutcome::Updated, None, Some(ours_path), ours_path),
            TakeoverAction::KeepRunning
        );
        assert_eq!(
            port_owner_action(SyncOutcome::Updated, owner, None, ours_path),
            TakeoverAction::KeepRunning
        );
    }
}
