//! Tachyon background server (v1.0.78).
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
//! the presence of an out-of-band provisioned `playit-secret.txt`
//! (see `ensure_agent_running`).
//!
//! Everything here is fail-soft by design:
//! - a failed sidecar fetch keeps the installed exe untouched;
//! - a failed download never replaces a working exe (temp file, hash
//!   verified, then renamed into place);
//! - a busy UDP 7777 means a server is already running — nothing to do;
//! - no error in this module may block or fail the game launch. All the
//!   work runs on a detached thread (see `kick_off`); the launch path
//!   only pays for one thread spawn.

use std::io::Write;
use std::path::{Path, PathBuf};

/// HF mirror directory holding the server binary and its sha256 sidecar
/// (same dataset the launcher's other assets sync from).
const MIRROR_BASE: &str =
    "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/tachyon-server/";
const SERVER_EXE_NAME: &str = "TachyonServer.exe";
/// UDP port the bundled server listens on (loopback endpoint handed out
/// by matchmaking for local sessions).
pub(crate) const SERVER_PORT: u16 = 7777;

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

/// Play-path entry point, called from `main()`'s `--play` /
/// `--launch-game` dispatch. Spawns the detached worker thread and
/// returns immediately — the mirror sync and the server spawn happen
/// off the launch path, so a slow network can never delay the game.
pub(crate) fn kick_off(game_dir: &Path) {
    let game_dir = game_dir.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("tachyon-server".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                runtime.block_on(async {
                    ensure_server(&game_dir).await;
                    ensure_agent(&game_dir).await;
                });
            }
            ensure_running(&game_dir);
            // The tunnel agent comes up only after the server it
            // exposes is running.
            ensure_agent_running(&game_dir);
        });
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
    let expected = match fetch_expected_hash(&client, &sidecar_url).await {
        Some(hash) => hash,
        None => {
            if exe.is_file() {
                println!("[tachyon] mirror unreachable — keeping installed server.");
            } else {
                eprintln!("[tachyon] mirror unreachable and no server installed yet.");
            }
            return;
        }
    };

    if exe.is_file() && sha256_file(&exe).as_deref() == Some(expected.as_str()) {
        return; // already in sync
    }

    // Download to a temp name, verify the hash, then move into place:
    // a partial or corrupt download never replaces a working exe.
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

/// Start the playit tunnel agent — HOST ONLY.
///
/// The launcher holds no account context (sign-in is web-only; the
/// play path never sees an account id or access token), so it cannot
/// prove the signed-in player is the host account. The gate is
/// therefore the out-of-band provisioned secret file: the agent runs
/// only when `playit-secret.txt` exists in the tachyon-server folder,
/// which is only ever true on the host's own install. Every other
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
        // builds. Either way the secret never goes on the command
        // line (visible in process listings) and never into a log.
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
    use super::{parse_sidecar_hash, server_exe, sha256_file, udp_port_free};
    use std::path::Path;

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
}
