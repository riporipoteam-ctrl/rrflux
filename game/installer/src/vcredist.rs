//! Visual C++ 2022 (x64) runtime detection and silent installation.
//!
//! Root cause of the v0.1.7 crash-on-launch on real PCs: the Goldberg Steam
//! emulator DLL is built with VS2022 and cannot load without the VC++ 2022
//! x64 runtime. Cloud CI runners have it preinstalled; home PCs often don't.
//! A DLL that fails to load at process start = instant crash, so the
//! installer must guarantee the runtime BEFORE swapping in the emulator.

/// Official Microsoft evergreen download for the VC++ 2022 x64 redist.
/// Verified 2026-09-23: `aka.ms` (Microsoft's own shortener) 301-redirects
/// to `download.visualstudio.microsoft.com/.../VC_redist.x64.exe`, and it is
/// the link published on Microsoft's "latest supported VC++ redist" docs page.
pub const VCREDIST_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
/// Silent install flags for vc_redist.x64.exe (documented by Microsoft).
pub const VCREDIST_ARGS: &[&str] = &["/install", "/quiet", "/norestart"];

/// Registry key holding the x64 VC++ runtime install state.
pub const VC_RUNTIME_KEY: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64";
/// Minimum runtime the vs22-built emulator needs: VS2022 == 14.30+.
/// (VS2015–2019 report 14.0–14.29 under the same major.)
pub const MIN_VC_MAJOR: u32 = 14;
pub const MIN_VC_MINOR: u32 = 30;

/// Parse the `Version` registry value (`v14.44.35211.0`) into (major, minor).
/// Pure and unit-tested.
pub fn parse_vc_version(s: &str) -> Option<(u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// True when a parsed version string meets the minimum runtime.
pub fn version_meets_minimum(s: &str) -> bool {
    match parse_vc_version(s) {
        Some((major, minor)) => {
            major > MIN_VC_MAJOR || (major == MIN_VC_MAJOR && minor >= MIN_VC_MINOR)
        }
        None => false,
    }
}

/// True when the VC++ 2022 x64 runtime is installed.
#[cfg(windows)]
pub fn is_installed() -> bool {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey(VC_RUNTIME_KEY);
    let key = match key {
        Ok(k) => k,
        Err(_) => return false,
    };
    let installed: u32 = key.get_value("Installed").unwrap_or(0);
    if installed != 1 {
        return false;
    }
    let version: String = match key.get_value("Version") {
        Ok(v) => v,
        Err(_) => return false,
    };
    version_meets_minimum(&version)
}

/// Non-Windows builds (Linux CI/dev): nothing to install, report present so
/// callers skip the install step.
#[cfg(not(windows))]
pub fn is_installed() -> bool {
    true
}

/// Ensure the runtime is present, downloading + silently installing it when
/// missing. Must run BEFORE the emulator DLL is swapped in.
#[cfg(windows)]
pub async fn ensure_installed(
    client: &reqwest::Client,
    progress: &crate::progress::Progress,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    if is_installed() {
        println!("[vcredist] VC++ 2022 x64 runtime present.");
        return Ok(());
    }
    println!("[vcredist] VC++ 2022 x64 runtime missing — installing silently.");
    progress.set_stage("Installing VC++ runtime…");
    let dest = std::env::temp_dir().join("vc_redist.x64.exe");
    crate::download(client, VCREDIST_URL, &dest, None, None, "vcredist", None).await?;
    let status = std::process::Command::new(&dest)
        .args(VCREDIST_ARGS)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status()
        .map_err(|e| format!("could not launch VC++ runtime installer: {e}"))?;
    let _ = std::fs::remove_file(&dest);
    // 0 = success, 3010 = success (reboot deferred by /norestart),
    // 1638 = a newer version is already installed.
    let code = status.code();
    let ok = status.success() || code == Some(3010) || code == Some(1638);
    if !ok {
        return Err(format!(
            "VC++ runtime installer failed (exit {status}); the game cannot start without it"
        ));
    }
    if !is_installed() {
        return Err(
            "VC++ runtime still not detected after install — please restart Windows and retry"
                .to_string(),
        );
    }
    println!("[vcredist] VC++ 2022 x64 runtime installed.");
    Ok(())
}

/// Non-Windows: no-op.
#[cfg(not(windows))]
pub async fn ensure_installed(
    _client: &reqwest::Client,
    _progress: &crate::progress::Progress,
) -> Result<(), String> {
    Ok(())
}

/// True when this process runs with administrator rights.
#[cfg(windows)]
pub fn is_admin() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = Default::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Non-Windows: report admin so callers never try to elevate.
#[cfg(not(windows))]
pub fn is_admin() -> bool {
    true
}

/// Re-launch this exact binary (same args) asking Windows for elevation via
/// the "runas" verb. Returns Ok when the elevated child was spawned; the
/// caller should then exit(0). If the user cancels the UAC prompt, no child
/// runs — the caller exiting is still the correct behavior.
#[cfg(windows)]
pub fn relaunch_elevated() -> Result<(), String> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own exe: {e}"))?;
    let params: String = std::env::args()
        .skip(1)
        .map(|a| {
            if a.chars().any(|c| c.is_whitespace() || c == '"') {
                format!("\"{}\"", a.replace('"', "\"\""))
            } else {
                a
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let exe_h = HSTRING::from(exe.to_string_lossy().as_ref());
    let params_h = HSTRING::from(params.as_str());
    let ret = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(exe_h.as_ptr()),
            PCWSTR(params_h.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Success = value > 32. (Note: a UAC cancel also surfaces here as an
    // error code > 32; either way the parent must exit.)
    if (ret.0 as usize) <= 32 {
        return Err(format!(
            "elevation request failed (code {})",
            ret.0 as usize
        ));
    }
    Ok(())
}

/// Non-Windows: elevation is meaningless.
#[cfg(not(windows))]
pub fn relaunch_elevated() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vc_version_handles_registry_format() {
        assert_eq!(parse_vc_version("v14.44.35211.0"), Some((14, 44)));
        assert_eq!(parse_vc_version("14.30.30704.0"), Some((14, 30)));
        assert_eq!(parse_vc_version("v14.29.30133.0"), Some((14, 29)));
    }

    #[test]
    fn parse_vc_version_rejects_garbage() {
        assert_eq!(parse_vc_version(""), None);
        assert_eq!(parse_vc_version("v14"), None);
        assert_eq!(parse_vc_version("abc"), None);
        assert_eq!(parse_vc_version("vX.44.1.0"), None);
    }

    #[test]
    fn version_minimum_is_vs2022() {
        assert!(version_meets_minimum("v14.30.30704.0"));
        assert!(version_meets_minimum("v14.44.35211.0"));
        assert!(version_meets_minimum("v15.0.0.0")); // future major still fine
        assert!(!version_meets_minimum("v14.29.30133.0")); // VS2019: not enough
        assert!(!version_meets_minimum("v14.0.24215.0")); // VS2015: not enough
        assert!(!version_meets_minimum("garbage"));
    }

    #[test]
    fn vcredist_url_is_official_microsoft() {
        assert!(VCREDIST_URL.starts_with("https://aka.ms/"));
        assert!(VCREDIST_URL.ends_with("vc_redist.x64.exe"));
    }

    #[test]
    fn vcredist_args_are_silent() {
        assert!(VCREDIST_ARGS.contains(&"/quiet"));
        assert!(VCREDIST_ARGS.contains(&"/norestart"));
    }
}
