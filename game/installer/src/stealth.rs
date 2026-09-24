//! Stealth helpers: hide console windows **without changing any behavior**.
//!
//! Everything the installer does today — downloads, extraction, config
//! writes, shortcut creation, and all `println!`/`eprintln!` logging — keeps
//! running *exactly* as before. These helpers only make windows invisible:
//!
//! - [`hide_own_console`] hides the installer's own console window at
//!   startup. Logging keeps working; it just has nowhere visible to go.
//! - [`hidden_command`] / [`hidden_tokio_command`] build child-process
//!   commands with `CREATE_NO_WINDOW`, so spawned helpers (PowerShell, …)
//!   never flash a console window.
//! - [`launch_hidden`] fire-and-forget-spawns the game exe with no console
//!   window inheritance.
//! - [`ensure_bepinex_console_disabled`] forces `Logging.Console / Enabled`
//!   to `false` in `BepInEx.cfg` — the only switch that stops BepInEx's
//!   preloader from allocating its own console window (via `AllocConsole`)
//!   inside the game process, which no spawn flag can prevent. It also
//!   repairs stale configs left behind by earlier installs.
//!
//! # Safety contract
//!
//! Hiding must never fail the install. Every function here is **infallible**:
//! they return `()` and swallow any failure internally. There is deliberately
//! no `Result` for callers to handle.
//!
//! # Platform gating
//!
//! All Win32 code is `#[cfg(windows)]`-gated behind raw `extern "system"`
//! declarations (no extra crate dependency). On non-Windows targets every
//! function degrades to a no-op / plain constructor so the crate keeps
//! passing `cargo check` on Linux.

use std::path::Path;

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    /// `ShowWindow` command: hide the window.
    pub const SW_HIDE: i32 = 0;
    /// Process creation flag: no console window for the child.
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetConsoleWindow() -> *mut c_void;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn ShowWindow(h_wnd: *mut c_void, n_cmd_show: i32) -> i32;
    }
}

/// Hide the installer's own console window.
///
/// Call as the **first line of `main()`**. If the process has no console
/// window (`GetConsoleWindow` returns null — already hidden, or a windowed
/// subsystem build), this is a silent no-op, **not** an error. All existing
/// `println!`/`eprintln!` output keeps working; it is simply invisible.
///
/// Infallible: never panics, never returns an error.
pub fn hide_own_console() {
    #[cfg(windows)]
    {
        // SAFETY: `GetConsoleWindow` has no preconditions and `ShowWindow`
        // is only called with the non-null handle it returned.
        unsafe {
            let hwnd = win::GetConsoleWindow();
            if !hwnd.is_null() {
                win::ShowWindow(hwnd, win::SW_HIDE);
            }
        }
    }
    // Non-Windows: no console window exists to hide; intentionally a no-op.
}

/// Build a `std::process::Command` that never shows a console window.
///
/// Drop-in replacement for `std::process::Command::new(program)` at every
/// spawn site in `main.rs` (the PowerShell calls in `create_shortcut` /
/// `create_shortcuts`). On Windows it sets `CREATE_NO_WINDOW`
/// (`0x08000000`) via `CommandExt::creation_flags`; piped stdout/stderr
/// (`.output()`) keeps working unchanged. On other platforms it is exactly
/// `Command::new(program)`.
///
/// Infallible: returns the command, no `Result`.
pub fn hidden_command(program: &str) -> std::process::Command {
    #[cfg(windows)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(not(windows))]
    let cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(win::CREATE_NO_WINDOW);
    }
    cmd
}

/// Async twin of [`hidden_command`] for `tokio::process::Command`.
///
/// `main.rs` currently spawns only via `std`; this exists so the GUI
/// launcher (agent 1) and any future async spawn site stay windowless too.
/// Same semantics: `CREATE_NO_WINDOW` on Windows, plain constructor
/// elsewhere. Infallible.
pub fn hidden_tokio_command(program: &str) -> tokio::process::Command {
    #[cfg(windows)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(not(windows))]
    let cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    {
        // NOTE: tokio's Command has an inherent `creation_flags` (forwards
        // to std's CommandExt), so no trait import is needed here.
        cmd.creation_flags(win::CREATE_NO_WINDOW);
    }
    cmd
}

/// Fire-and-forget launch of the game exe with **no console window
/// inheritance**.
///
/// Intended for the GUI's "launching game…" step: spawns `RecRoom.exe`
/// with `CREATE_NO_WINDOW` so the child is never given a console at
/// creation time, and the game boots exactly like a manual double-click
/// launch (no DETACHED_PROCESS — process semantics stay stock).
///
/// Note: `CREATE_NO_WINDOW` cannot stop the game process from allocating a
/// console itself later — BepInEx's preloader calls `AllocConsole` inside
/// the game process. Killing the BepInEx console window is the job of
/// [`ensure_bepinex_console_disabled`]; this helper covers everything else.
///
/// This does not `wait()` — it returns immediately after `spawn()`. Any
/// spawn failure is swallowed (infallible by contract); user-facing feedback
/// is the GUI's job, not this helper's.
///
/// # Example
///
/// ```ignore
/// // Agent 5 wiring (in main.rs / GUI code):
/// if let Some(exe) = find_game_exe(&dir) {
///     stealth::launch_hidden(&exe, &["+forcemode:screen"]);
/// }
/// ```
pub fn launch_hidden(exe: &Path, args: &[&str]) {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW only: the child gets no console window, but
        // process semantics stay exactly like a normal Explorer double-click
        // launch. (DETACHED_PROCESS is deliberately NOT used: it changes how
        // the game attaches to its parent, and the game must boot byte-for-
        // byte like a manual launch.)
        cmd.creation_flags(win::CREATE_NO_WINDOW);
    }
    // Deliberately ignored: hiding/launching must never fail the install.
    let _ = cmd.spawn();
}

/// Force the BepInEx console window off — including repairing stale configs.
///
/// BepInEx 6.0.0-pre.2 (the exact build this installer ships) binds
/// `Logging.Console / Enabled` in `BepInEx/config/BepInEx.cfg` with a
/// **default of `true`**, so a freshly generated config pops the console
/// window at every game launch. The preloader allocates that console
/// itself (via `AllocConsole`) inside the game process, which means **no
/// spawn flag can suppress it** — `CREATE_NO_WINDOW` only stops console
/// inheritance at process creation. This config switch is the only
/// supported kill switch.
///
/// Unlike a purely additive write, this also repairs a stale config left
/// behind by an earlier install: e.g. a first launch that generated
/// `Enabled = true` before any config was written, then carried forward by
/// every update since (the BepInEx zip ships no `BepInEx.cfg`, so updates
/// never clobber it). Every `Enabled` key under every `[Logging.Console]`
/// section is forced to `false`; the section is appended when absent, and
/// the key is inserted when the section exists without it. Section/key
/// matching is case-insensitive; comments and unrelated keys are preserved
/// byte-for-byte.
///
/// Infallible: any I/O failure is swallowed — a config hiccup must never
/// fail the install (the game still launches; the console just might show).
pub fn ensure_bepinex_console_disabled(dir: &Path) {
    let cfg_path = dir.join("BepInEx").join("config").join("BepInEx.cfg");
    if let Some(parent) = cfg_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let existing = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let fixed = force_console_disabled(&existing);
    // Write only on change: never touch a config that is already correct.
    if fixed != existing {
        let _ = std::fs::write(&cfg_path, fixed);
    }
}

/// Pure transform behind [`ensure_bepinex_console_disabled`]: returns the
/// config text with `[Logging.Console] / Enabled` forced to `false`.
///
/// - Section absent → appended at the end (`[Logging.Console]` +
///   `Enabled = false`).
/// - Section present without an `Enabled` key → the key is inserted right
///   after the first section header.
/// - Otherwise every `Enabled = …` assignment inside such sections is
///   rewritten to `Enabled = false` (leading indentation preserved).
///
/// Comment lines (`#` / `;`) and `Enabled` keys under *other* sections are
/// never touched. The file's CRLF/LF line-ending style is preserved.
fn force_console_disabled(cfg: &str) -> String {
    let crlf = cfg.contains("\r\n");
    let nl = if crlf { "\r\n" } else { "\n" };

    let mut lines: Vec<String> = if crlf {
        cfg.split("\r\n").map(str::to_string).collect()
    } else {
        cfg.split('\n').map(str::to_string).collect()
    };
    // `split` yields one trailing "" for a final newline; drop it so the
    // append logic below doesn't stack blank lines (re-added at the end).
    if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
    let mut first_header: Option<usize> = None;
    let mut in_console_section = false;
    let mut enabled_written = false;

    for line in &lines {
        let t = line.trim();
        if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
            in_console_section = t[1..t.len() - 1]
                .trim()
                .eq_ignore_ascii_case("logging.console");
            if in_console_section && first_header.is_none() {
                first_header = Some(out.len());
            }
        } else if in_console_section && !t.starts_with('#') && !t.starts_with(';') {
            if let Some((key, _)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("enabled") {
                    let indent: String =
                        line.chars().take_while(|c| c.is_whitespace()).collect();
                    out.push(format!("{indent}Enabled = false"));
                    enabled_written = true;
                    continue;
                }
            }
        }
        out.push(line.clone());
    }

    match first_header {
        None => {
            if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                out.push(String::new());
            }
            out.push("[Logging.Console]".to_string());
            out.push("Enabled = false".to_string());
        }
        Some(h) => {
            if !enabled_written {
                out.insert(h + 1, "Enabled = false".to_string());
            }
        }
    }

    let mut result = out.join(nl);
    result.push_str(nl);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hide_own_console_is_noop_safe() {
        // Must never panic, with or without a console window.
        hide_own_console();
        hide_own_console();
    }

    #[test]
    fn hidden_command_builds() {
        // Build-only: never spawned, so the test is side-effect free.
        let _ = hidden_command("powershell");
        let _ = hidden_tokio_command("powershell");
    }

    #[test]
    fn launch_hidden_missing_exe_does_not_panic() {
        // Spawning a nonexistent exe must be swallowed, not panic.
        launch_hidden(
            Path::new("/nonexistent/RecRoom.exe"),
            &["+forcemode:screen"],
        );
    }

    #[test]
    fn console_config_appended_when_section_missing() {
        let out = force_console_disabled("");
        assert!(out.contains("[Logging.Console]"));
        assert!(out.contains("Enabled = false"));
        // Appended after existing content with a blank separator line.
        let out = force_console_disabled("[General]\nFoo = 1\n");
        assert!(out.contains("[General]\nFoo = 1\n\n[Logging.Console]\nEnabled = false\n"));
    }

    #[test]
    fn console_config_repairs_stale_enabled_true() {
        let cfg = "[General]\nFoo = 1\n\n[Logging.Console]\nEnabled = true\nLogLevels = All\n";
        let out = force_console_disabled(cfg);
        assert!(out.contains("[Logging.Console]\nEnabled = false\nLogLevels = All\n"));
        assert!(out.contains("[General]\nFoo = 1"));
        assert!(!out.contains("Enabled = true"));
    }

    #[test]
    fn console_config_leaves_disabled_alone() {
        // Already correct: byte-identical, so the helper won't rewrite the file.
        let cfg = "[Logging.Console]\nEnabled = false\n";
        assert_eq!(force_console_disabled(cfg), cfg);
    }

    #[test]
    fn console_config_inserts_key_when_section_lacks_it() {
        let cfg = "[Logging.Console]\nPreventClose = true\n";
        let out = force_console_disabled(cfg);
        assert!(out.contains("[Logging.Console]\nEnabled = false\nPreventClose = true\n"));
    }

    #[test]
    fn console_config_ignores_comments_and_other_sections() {
        let cfg = "# Enabled = true\n[Other]\nEnabled = true\n[Logging.Console]\n# Enabled = true\n";
        let out = force_console_disabled(cfg);
        // Comment lines preserved verbatim; [Other]'s key untouched.
        assert!(out.contains("# Enabled = true"));
        assert!(out.contains("[Other]\nEnabled = true"));
        // The console section gains the key directly after its header.
        assert!(out.contains("[Logging.Console]\nEnabled = false\n# Enabled = true\n"));
    }

    #[test]
    fn console_config_matching_is_case_insensitive() {
        let cfg = "[LOGGING.CONSOLE]\nENABLED = TRUE\n";
        let out = force_console_disabled(cfg);
        assert!(out.contains("Enabled = false"));
        assert!(!out.to_ascii_lowercase().contains("true"));
    }

    #[test]
    fn console_config_preserves_crlf() {
        let cfg = "[Logging.Console]\r\nEnabled = true\r\n";
        let out = force_console_disabled(cfg);
        assert!(out.contains("[Logging.Console]\r\nEnabled = false\r\n"));
    }

    #[test]
    fn console_config_repairs_duplicate_sections() {
        let cfg = "[Logging.Console]\nEnabled = true\n[Other]\nX = 1\n[Logging.Console]\nEnabled = true\n";
        let out = force_console_disabled(cfg);
        assert!(!out.contains("Enabled = true"));
        assert_eq!(out.matches("Enabled = false").count(), 2);
    }

    #[test]
    fn ensure_bepinex_console_disabled_repairs_stale_file() {
        let d = std::env::temp_dir().join("fluxrec-stealth-test-stale");
        let _ = std::fs::remove_dir_all(&d);
        let cfg_dir = d.join("BepInEx").join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("BepInEx.cfg"), "[Logging.Console]\nEnabled = true\n")
            .unwrap();
        ensure_bepinex_console_disabled(&d);
        let back = std::fs::read_to_string(cfg_dir.join("BepInEx.cfg")).unwrap();
        assert!(back.contains("Enabled = false"));
        assert!(!back.contains("Enabled = true"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_bepinex_console_disabled_creates_missing_file() {
        let d = std::env::temp_dir().join("fluxrec-stealth-test-missing");
        let _ = std::fs::remove_dir_all(&d);
        ensure_bepinex_console_disabled(&d);
        let back =
            std::fs::read_to_string(d.join("BepInEx").join("config").join("BepInEx.cfg"))
                .unwrap();
        assert!(back.contains("[Logging.Console]"));
        assert!(back.contains("Enabled = false"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_bepinex_console_disabled_is_idempotent() {
        let d = std::env::temp_dir().join("fluxrec-stealth-test-idem");
        let _ = std::fs::remove_dir_all(&d);
        ensure_bepinex_console_disabled(&d);
        let p = d.join("BepInEx").join("config").join("BepInEx.cfg");
        let first = std::fs::read_to_string(&p).unwrap();
        ensure_bepinex_console_disabled(&d);
        let second = std::fs::read_to_string(&p).unwrap();
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&d);
    }
}
