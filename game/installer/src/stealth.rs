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
//!   window, so the BepInEx console never pops at game launch.
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

/// Fire-and-forget launch of the game exe with **no console window**.
///
/// Intended for the GUI's "launching game…" step: spawns `RecRoom.exe`
/// with `CREATE_NO_WINDOW` so
///  1. the BepInEx console window never flashes at game launch, and
///  2. the game boots exactly like a manual double-click launch
///     (no DETACHED_PROCESS — process semantics stay stock).
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
}
