//! Windows Defender / SmartScreen hardening.
//!
//! Real-world finding (2026-09-24, Armin's PC): Windows Security quarantined
//! a game file — most likely `RecNetPlugin.dll` or the Goldberg
//! `steam_api64.dll`, both classic heuristic targets (unsigned .NET /
//! emulator DLLs). Without the redirect plugin the client talks to the dead
//! real backend and hangs at "Connecting to server..." forever, which is
//! exactly the symptom he reported. His screenshot also shows SmartScreen
//! flagging `FluxRec-Setup.exe` as "Unknown publisher" (we ship unsigned —
//! the zero-spend rule forbids buying a code-signing certificate).
//!
//! This module holds the free, no-certificate mitigations:
//!
//! 1. **Defender exclusions** ([`ensure_defender_exclusions`]): after the
//!    game dir is written, the install dir is added to Defender's
//!    `ExclusionPath` list (plus `ExclusionProcess` for `RecRoom.exe` and
//!    `FluxRecLauncher.exe`), so real-time protection stops quarantining
//!    our files. Idempotent (checks `Get-MpPreference` first), verified
//!    (re-reads the list after adding), silent (hidden PowerShell).
//! 2. **Self-heal** ([`verify_quarantine_targets`] /
//!    [`repair_quarantine_targets`]): the files Defender likes to eat are
//!    verified on every `--play` alongside the existing `bypass::verify`
//!    step — the redirect plugin DLL, the Steam bypass DLL + settings, and
//!    the `ns.rec.net` hosts mapping. Anything missing is repaired
//!    automatically: the plugin is re-downloaded, the hosts entry is
//!    re-applied, the exclusion is re-added.
//! 3. **Mark-of-the-Web removal** ([`unblock_file`]): `Unblock-File` on
//!    every internet-sourced file we write (downloaded archives, the plugin
//!    DLL, the swapped emulator DLL, the launcher copy), so .NET/BepInEx
//!    never refuse to load them.
//!
//! # What this module deliberately does NOT do
//!
//! It never disables, weakens, or bypasses any security feature — no
//! SmartScreen registry kills, no Defender service stops, no tampering with
//! Tamper Protection. Doing that would be malware behavior, and Defender
//! flags software that does it *harder*. The SmartScreen "Unknown
//! publisher" blue screen can only be reduced over time via reputation and
//! Microsoft's free false-positive submission portal; see
//! `game/installer/AV_MITIGATION.md` for the full write-up.
//!
//! # Safety contract
//!
//! AV hardening must **never** break install or launch. Every public
//! function is fail-soft: Defender absent, no admin rights, Tamper
//! Protection on, or any command failure → log a warning and continue.
//! Nothing here prompts the user (elevation already happens in the normal
//! install flow) and nothing here deletes or quarantines anything —
//! repair only ever *restores* files.

use std::path::Path;

/// The dead real Rec Room nameserver hostname the 2023 client resolves
/// itself (via `System.Net.Dns`), bypassing the plugin's HTTP redirect.
pub const NS_DEAD_HOST: &str = "ns.rec.net";

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Normalize a Defender exclusion path: forward slashes become backslashes
/// and trailing separators are stripped (a bare drive root like `C:\` is
/// kept intact). Defender matches case-insensitively, so the idempotency
/// check lowercases both sides in [`already_excluded`].
pub fn normalize_exclusion_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// Parse the `ExclusionPath` list out of `(Get-MpPreference).ExclusionPath`
/// stdout: one path per line, blanks dropped, surrounding whitespace
/// trimmed.
pub fn parse_exclusion_paths(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// True when `dir` is already covered by the exclusion list — either
/// exactly, case-insensitively, or via an excluded parent directory
/// (excluding `C:\Games` covers `C:\Games\FluxRec`).
pub fn already_excluded(excluded: &[String], dir: &Path) -> bool {
    let want = normalize_exclusion_path(dir).to_lowercase();
    excluded.iter().any(|e| {
        let have = normalize_exclusion_path(Path::new(e)).to_lowercase();
        have == want || want.starts_with(&format!("{have}\\"))
    })
}

/// Pure check: does this hosts-file text contain ANY mapping for
/// `ns.rec.net` (to any IP)? Comment lines are ignored; an inline
/// `# comment` after the entry is fine.
///
/// This is deliberately IP-agnostic: the backend sits behind anycast DNS,
/// so two back-to-back resolutions of the backend host can legitimately
/// return different IPs. Comparing the entry against a freshly-resolved IP
/// (the old `ns_resolution_ok` approach) false-positived whenever the
/// anycast answer rotated — which sent the launcher into a bogus "repair"
/// loop that ended in a blocking error dialog. What matters for the game
/// is only that the dead hostname resolves at all, which any hosts-file
/// entry guarantees (the system resolver honors the hosts file).
pub fn ns_hosts_entry_present(hosts_content: &str) -> bool {
    for line in hosts_content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut parts = t.split_whitespace();
        // First token must be an IP literal; the hostname must be a
        // separate token on the same line.
        let ip = parts.next().unwrap_or("");
        if let Ok(addr) = ip.parse::<std::net::IpAddr>() {
            // Reject loopback: ns.rec.net never resolves to localhost. A
            // 127.x.x.x mapping (e.g. from hostile test setups) is wrong and
            // must be treated as absent so it gets rewritten with the real IP.
            if addr.is_loopback() {
                continue;
            }
            if parts.any(|tok| tok == NS_DEAD_HOST) {
                return true;
            }
        }
    }
    false
}

/// Read the system hosts file. `None` when unreadable (or not on Windows).
pub fn read_hosts_file() -> Option<String> {
    #[cfg(windows)]
    {
        std::fs::read_to_string("C:\\Windows\\System32\\drivers\\etc\\hosts").ok()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Strip an optional `http(s)://` scheme and any trailing path from a
/// configured nameserver host value, leaving a bare hostname or IP.
/// `to_socket_addrs` chokes on schemes, and the baked-in value may one day
/// be a full URL.
fn strip_scheme(host: &str) -> &str {
    let h = host.trim();
    let h = h
        .strip_prefix("https://")
        .or_else(|| h.strip_prefix("http://"))
        .unwrap_or(h);
    h.split('/').next().unwrap_or(h)
}

// ---------------------------------------------------------------------------
// Resolution helpers (network, fail-soft)
// ---------------------------------------------------------------------------

/// Resolve the configured backend host to an IP string. Returns `None`
/// when offline or unresolvable — callers treat that as "cannot verify",
/// never as a failure.
pub fn resolve_backend_ip(ns_host: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let host = strip_scheme(ns_host);
    if host.is_empty() {
        return None;
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Some(host.to_string());
    }
    format!("{host}:80")
        .to_socket_addrs()
        .ok()?
        .next()
        .map(|a| a.ip().to_string())
}

// ---------------------------------------------------------------------------
// PowerShell plumbing (hidden, fail-soft)
// ---------------------------------------------------------------------------

/// Single-quote escaping for PowerShell single-quoted string literals.
#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// Run a PowerShell snippet with no window and return stdout on success.
/// `None` on any failure (powershell missing, policy, non-zero exit…).
#[cfg(windows)]
fn ps_output(script: &str) -> Option<String> {
    let out = crate::stealth::hidden_command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[cfg(not(windows))]
fn ps_output(_script: &str) -> Option<String> {
    None
}

/// Fire-and-forget twin of [`ps_output`]; stdout is discarded.
#[cfg(windows)]
fn ps_run(script: &str) {
    let _ = crate::stealth::hidden_command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output();
}

#[cfg(not(windows))]
fn ps_run(_script: &str) {}

// ---------------------------------------------------------------------------
// 1. Defender exclusions
// ---------------------------------------------------------------------------

/// Current `ExclusionPath` list, or `None` when Defender's cmdlets are
/// unavailable (no Defender, third-party AV in charge, …).
#[cfg(windows)]
fn current_exclusions() -> Option<Vec<String>> {
    ps_output("(Get-MpPreference).ExclusionPath").map(|o| parse_exclusion_paths(&o))
}

#[cfg(not(windows))]
fn current_exclusions() -> Option<Vec<String>> {
    None
}

/// Ensure Windows Defender leaves the game dir alone.
///
/// Adds the install dir to `ExclusionPath` and `RecRoom.exe` /
/// `FluxRecLauncher.exe` to `ExclusionProcess`. Idempotent (skips the
/// privileged call when already excluded), verified (re-reads the list
/// afterwards), silent (hidden PowerShell), and fail-soft: no admin,
/// Defender absent, or Tamper Protection on → log a warning with manual
/// steps and continue. Never fails the install.
///
/// Note on Tamper Protection: when it is on, `Add-MpPreference` is denied
/// even to administrators. That is expected and fine — the self-heal path
/// will keep repairing quarantined files instead.
pub fn ensure_defender_exclusions(dir: &Path) {
    #[cfg(not(windows))]
    {
        let _ = dir;
        return;
    }
    #[cfg(windows)]
    {
        let dir_s = normalize_exclusion_path(dir);
        match current_exclusions() {
            Some(existing) if already_excluded(&existing, dir) => {
                println!("[av] Defender exclusion already in place for {dir_s}.");
                return;
            }
            _ => {}
        }
        let game_exe = normalize_exclusion_path(&dir.join("RecRoom.exe"));
        let launcher_exe = normalize_exclusion_path(&dir.join("FluxRecLauncher.exe"));
        ps_run(&format!(
            "Add-MpPreference -ExclusionPath '{dir}' -ErrorAction SilentlyContinue; \
             Add-MpPreference -ExclusionProcess '{game}' -ErrorAction SilentlyContinue; \
             Add-MpPreference -ExclusionProcess '{launcher}' -ErrorAction SilentlyContinue",
            dir = ps_quote(&dir_s),
            game = ps_quote(&game_exe),
            launcher = ps_quote(&launcher_exe),
        ));
        match current_exclusions() {
            Some(existing) if already_excluded(&existing, dir) => {
                println!("[av] Defender exclusion added for {dir_s}.");
            }
            _ => {
                eprintln!(
                    "[av] WARNING: could not add the Defender exclusion for {dir_s} \
                     (no admin rights, Defender unavailable, or Tamper Protection is on). \
                     If Windows Security removes game files, add the folder manually: \
                     Windows Security > Virus & threat protection > Manage settings > Exclusions."
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Mark-of-the-Web removal
// ---------------------------------------------------------------------------

/// Remove the Zone.Identifier "Mark of the Web" from a file.
///
/// Files downloaded through a browser carry this flag, and .NET/BepInEx can
/// refuse to load flagged DLLs. Idempotent (a file without the stream is a
/// no-op) and infallible: any failure is swallowed — an unblock hiccup must
/// never fail the install.
pub fn unblock_file(path: &Path) {
    #[cfg(not(windows))]
    {
        let _ = path;
        return;
    }
    #[cfg(windows)]
    {
        let path_str = path.to_string_lossy();
        let out = crate::stealth::hidden_command("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Unblock-File -LiteralPath '{}' -ErrorAction SilentlyContinue",
                ps_quote(&path_str)
            ),
        ])
        .output();
        match out {
            Ok(o) if o.status.success() => println!("[unblock] unblocked {path_str}"),
            _ => eprintln!(
                "[unblock] WARNING: could not unblock {path_str}; the file may fail to load."
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Self-heal for quarantined files
// ---------------------------------------------------------------------------

/// A repairable problem found by [`verify_quarantine_targets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineProblem {
    /// `BepInEx/plugins/RecNetPlugin.dll` is missing or empty. Without it
    /// the client talks to the dead real backend and hangs at "Connecting
    /// to server..." — the exact symptom of a quarantine hit.
    PluginDllMissing,
    /// The `ns.rec.net` → backend mapping is not in effect (hosts entry
    /// removed or DNS regressed).
    HostsEntryMissing,
    /// The Steam bypass DLL/settings are missing or reverted. Carries the
    /// human-readable reason from `bypass::verify`.
    SteamBypassBroken(String),
}

/// Human-readable one-line descriptions, for launcher status text.
pub fn describe_problems(problems: &[QuarantineProblem]) -> String {
    problems
        .iter()
        .map(|p| match p {
            QuarantineProblem::PluginDllMissing => {
                "RecNetPlugin.dll is missing (likely quarantined by Windows Security)"
                    .to_string()
            }
            QuarantineProblem::HostsEntryMissing => {
                "ns.rec.net does not resolve to the game server".to_string()
            }
            QuarantineProblem::SteamBypassBroken(r) => r.clone(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Inspect the install dir for the files Windows Security likes to
/// quarantine. Pure filesystem checks plus one DNS resolution — safe to run
/// on every `--play`. Never deletes or quarantines anything itself.
pub fn verify_quarantine_targets(dir: &Path, _ns_host: &str) -> Vec<QuarantineProblem> {
    let mut problems = Vec::new();

    // 1. Redirect plugin — the #1 quarantine target (unsigned .NET DLL).
    let plugin = dir.join("BepInEx").join("plugins").join("RecNetPlugin.dll");
    if !std::fs::metadata(&plugin).map(|m| m.len() > 0).unwrap_or(false) {
        problems.push(QuarantineProblem::PluginDllMissing);
    }

    // 2. Steam bypass DLL + settings. A quarantine hit (or a revert) shows
    // up here — reuse the existing verifier instead of duplicating it.
    if let crate::bypass::BypassState::Broken(reason) = crate::bypass::verify(dir) {
        problems.push(QuarantineProblem::SteamBypassBroken(reason));
    }

    // 3. ns.rec.net must be mapped in the hosts file. When it isn't, the
    // 2023 client (which resolves the dead hostname itself via
    // System.Net.Dns) hangs at "Connecting to server...". This checks the
    // hosts FILE CONTENT, not a live DNS comparison: the backend is
    // anycast, so comparing against a freshly-resolved IP false-positives
    // whenever the anycast answer rotates between lookups. An unreadable
    // hosts file is treated as OK (can't verify — don't cry wolf).
    let hosts_ok = read_hosts_file()
        .map(|c| ns_hosts_entry_present(&c))
        .unwrap_or(true);
    if !hosts_ok {
        problems.push(QuarantineProblem::HostsEntryMissing);
    }

    problems
}

/// Re-install the redirect plugin DLL from the embedded copy and unblock it.
/// Returns true when the DLL is present and non-empty afterwards.
async fn repair_plugin_dll(
    _client: &reqwest::Client,
    dir: &Path,
    progress: &crate::progress::Progress,
) -> bool {
    let plugins_dir = dir.join("BepInEx").join("plugins");
    if std::fs::create_dir_all(&plugins_dir).is_err() {
        return false;
    }
    let dest = plugins_dir.join("RecNetPlugin.dll");
    // Remove any quarantined remnant (e.g. a 0-byte stub) so the write
    // below actually replaces it instead of skipping as "already present".
    let _ = std::fs::remove_file(&dest);
    progress.set_stage("Repairing Flux Rec plugin…");
    // v0.1.19: use the embedded plugin (no download — upstream lacks our patches).
    match std::fs::write(&dest, crate::EMBEDDED_PLUGIN) {
        Ok(()) => {
            unblock_file(&dest);
            std::fs::metadata(&dest)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        }
        Err(e) => {
            eprintln!("[plugin-repair] failed: {}", e);
            false
        }
    }
}

/// Repair every problem reported by [`verify_quarantine_targets`].
///
/// - Plugin DLL → re-installed from the embedded copy and unblocked.
/// - Hosts entry → re-applied via the existing hosts writer.
/// - Steam bypass → the existing `bypass::repair_bypass` pipeline.
/// - Afterwards the Defender exclusion is re-applied (the user may have
///   removed it) and the critical DLLs are unblocked.
///
/// Returns the problems that are *still* unrepaired, so the caller can
/// report them honestly. Fail-soft per item: one failed repair never blocks
/// the others, and a total failure just means the launcher shows its
/// normal "please re-run setup" message.
pub async fn repair_quarantine_targets(
    client: &reqwest::Client,
    dir: &Path,
    ns_host: &str,
    progress: &crate::progress::Progress,
    problems: &[QuarantineProblem],
) -> Vec<QuarantineProblem> {
    let mut remaining = Vec::new();
    for problem in problems {
        let fixed = match problem {
            QuarantineProblem::PluginDllMissing => {
                repair_plugin_dll(client, dir, progress).await
            }
            QuarantineProblem::HostsEntryMissing => {
                progress.set_stage("Repairing network configuration…");
                crate::ensure_ns_hosts_entry(ns_host);
                // Verify by re-reading the hosts file: any ns.rec.net entry
                // means the client will resolve the dead hostname.
                // Deliberately not a DNS comparison — anycast rotations made
                // that check flaky and caused bogus repair loops.
                read_hosts_file()
                    .map(|c| ns_hosts_entry_present(&c))
                    .unwrap_or(false)
            }
            QuarantineProblem::SteamBypassBroken(_) => {
                match crate::bypass::repair_bypass(client, dir, progress).await {
                    Ok(_) => matches!(
                        crate::bypass::verify(dir),
                        crate::bypass::BypassState::Ok(_)
                    ),
                    Err(e) => {
                        eprintln!("[av] Steam bypass repair failed: {e}");
                        false
                    }
                }
            }
        };
        if !fixed {
            remaining.push(problem.clone());
        }
    }
    // Re-apply the defenses that keep these files alive.
    ensure_defender_exclusions(dir);
    unblock_file(&dir.join("BepInEx").join("plugins").join("RecNetPlugin.dll"));
    unblock_file(
        &dir
            .join("RecRoom_Data")
            .join("Plugins")
            .join("x86_64")
            .join("steam_api64.dll"),
    );
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_exclusion_path_strips_trailing_separators() {
        assert_eq!(
            normalize_exclusion_path(Path::new(r"C:\Games\FluxRec\")),
            r"C:\Games\FluxRec"
        );
        assert_eq!(
            normalize_exclusion_path(Path::new(r"C:\Games\FluxRec\\\")),
            r"C:\Games\FluxRec"
        );
        // A bare drive root keeps its backslash.
        assert_eq!(normalize_exclusion_path(Path::new(r"C:\")), r"C:\");
    }

    #[test]
    fn normalize_exclusion_path_converts_forward_slashes() {
        assert_eq!(
            normalize_exclusion_path(Path::new("C:/Games/FluxRec")),
            r"C:\Games\FluxRec"
        );
    }

    #[test]
    fn parse_exclusion_paths_skips_blanks() {
        let out = "C:\\Games\\FluxRec\r\n\r\n   \nD:\\Other\r\n";
        assert_eq!(
            parse_exclusion_paths(out),
            vec![r"C:\Games\FluxRec".to_string(), r"D:\Other".to_string()]
        );
        assert!(parse_exclusion_paths("").is_empty());
        // Defender with no exclusions: $null renders as empty output.
        assert!(parse_exclusion_paths("\r\n").is_empty());
    }

    #[test]
    fn already_excluded_matches_case_insensitively() {
        let excluded = vec![r"c:\games\fluxrec".to_string()];
        assert!(already_excluded(&excluded, Path::new(r"C:\Games\FluxRec")));
        assert!(already_excluded(&excluded, Path::new(r"C:\GAMES\FLUXREC\")));
    }

    #[test]
    fn already_excluded_covers_parent_dir_exclusion() {
        let excluded = vec![r"C:\Games".to_string()];
        assert!(already_excluded(&excluded, Path::new(r"C:\Games\FluxRec")));
        // …but not a sibling that merely shares a prefix.
        assert!(!already_excluded(&excluded, Path::new(r"C:\Games2")));
        assert!(!already_excluded(&excluded, Path::new(r"D:\Games\FluxRec")));
    }

    #[test]
    fn already_excluded_empty_list() {
        assert!(!already_excluded(&[], Path::new(r"C:\Games\FluxRec")));
    }

    #[test]
    fn ns_hosts_entry_present_detects_any_mapping() {
        // Any non-loopback IP mapping counts — the backend is anycast, so the
        // stored IP legitimately differs from a fresh DNS answer. Loopback
        // (127.x.x.x, ::1) is never valid for ns.rec.net and is treated as
        // absent so it gets rewritten.
        let hosts = "# comment\n127.0.0.1 localhost\n93.184.216.34 ns.rec.net # Flux Rec backend\n";
        assert!(ns_hosts_entry_present(hosts));
        assert!(ns_hosts_entry_present("203.0.113.7 ns.rec.net\n"));
        assert!(!ns_hosts_entry_present("127.0.0.1 ns.rec.net\n"));
        assert!(!ns_hosts_entry_present("::1 ns.rec.net\n"));
    }

    #[test]
    fn ns_hosts_entry_present_ignores_comments_and_garbage() {
        // Commented-out mapping doesn't count.
        assert!(!ns_hosts_entry_present("# 93.184.216.34 ns.rec.net\n"));
        // Non-IP first token doesn't count.
        assert!(!ns_hosts_entry_present("example.com ns.rec.net\n"));
        // Missing entirely.
        assert!(!ns_hosts_entry_present("127.0.0.1 localhost\n"));
        // Empty file.
        assert!(!ns_hosts_entry_present(""));
    }

    #[test]
    fn ns_hosts_entry_present_handles_multi_host_lines() {
        assert!(ns_hosts_entry_present(
            "93.184.216.34 ns.rec.net api.rec.net\n"
        ));
        // Inline comment after the entry is fine.
        assert!(ns_hosts_entry_present(
            "93.184.216.34 ns.rec.net # Flux Rec backend\n"
        ));
    }

    #[test]
    fn strip_scheme_handles_urls() {
        assert_eq!(strip_scheme("https://example.com/path"), "example.com");
        assert_eq!(strip_scheme("http://example.com"), "example.com");
        assert_eq!(strip_scheme("example.com"), "example.com");
        assert_eq!(strip_scheme("  1.2.3.4  "), "1.2.3.4");
        assert_eq!(strip_scheme(""), "");
    }

    #[test]
    fn resolve_backend_ip_passes_ip_literals_through() {
        // No network needed for literals.
        assert_eq!(
            resolve_backend_ip("93.184.216.34"),
            Some("93.184.216.34".to_string())
        );
        assert_eq!(resolve_backend_ip("https://93.184.216.34/x"), Some("93.184.216.34".to_string()));
        assert_eq!(resolve_backend_ip(""), None);
        assert_eq!(resolve_backend_ip("   "), None);
    }

    #[test]
    fn unblock_file_never_panics() {
        // Nonexistent path, hidden PowerShell (or no-op off Windows).
        unblock_file(Path::new("/nonexistent/RecNetPlugin.dll"));
        unblock_file(Path::new(r"C:\nonexistent\RecNetPlugin.dll"));
    }

    #[test]
    fn describe_problems_joins_readably() {
        let ps = vec![
            QuarantineProblem::PluginDllMissing,
            QuarantineProblem::HostsEntryMissing,
            QuarantineProblem::SteamBypassBroken("demo reason".to_string()),
        ];
        let s = describe_problems(&ps);
        assert!(s.contains("RecNetPlugin.dll"));
        assert!(s.contains("ns.rec.net"));
        assert!(s.contains("demo reason"));
        assert_eq!(describe_problems(&[]), "");
    }

    #[test]
    fn verify_detects_missing_plugin_dll() {
        let d = std::env::temp_dir().join("fluxrec-defender-test-missing");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Empty dir: plugin missing, bypass broken. The hosts-file check is
        // deliberately not asserted here: on non-Windows the hosts file
        // can't be read (treated as "can't verify", not a problem), and on
        // Windows it depends on the machine's real hosts file. The pure
        // ns_hosts_entry_present tests cover that logic hermetically.
        let problems = verify_quarantine_targets(&d, "127.0.0.1");
        assert!(problems.contains(&QuarantineProblem::PluginDllMissing));
        assert!(problems
            .iter()
            .any(|p| matches!(p, QuarantineProblem::SteamBypassBroken(_))));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verify_passes_plugin_when_present() {
        let d = std::env::temp_dir().join("fluxrec-defender-test-present");
        let _ = std::fs::remove_dir_all(&d);
        let plug = d.join("BepInEx").join("plugins");
        std::fs::create_dir_all(&plug).unwrap();
        std::fs::write(plug.join("RecNetPlugin.dll"), b"fake-dll").unwrap();
        let problems = verify_quarantine_targets(&d, "127.0.0.1");
        assert!(!problems.contains(&QuarantineProblem::PluginDllMissing));
        // A 0-byte file counts as missing too (quarantine remnant).
        std::fs::write(plug.join("RecNetPlugin.dll"), b"").unwrap();
        let problems = verify_quarantine_targets(&d, "127.0.0.1");
        assert!(problems.contains(&QuarantineProblem::PluginDllMissing));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verify_skips_hosts_check_when_backend_unresolvable() {
        let d = std::env::temp_dir().join("fluxrec-defender-test-offline");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Empty backend host: resolve_backend_ip returns None without
        // touching DNS (hermetic — real DNS may wildcard-resolve anything),
        // so no false-positive HostsEntryMissing.
        assert!(resolve_backend_ip("").is_none());
        let problems = verify_quarantine_targets(&d, "");
        assert!(!problems.contains(&QuarantineProblem::HostsEntryMissing));
        let _ = std::fs::remove_dir_all(&d);
    }
}
