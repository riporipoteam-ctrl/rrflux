// Flux Rec launch diagnostics (v0.4.0+).
//
// - Configures Windows Error Reporting local dumps for RecRoom.exe under
//   HKCU (no admin needed), so a native crash leaves a .dmp we can read.
// - Copies the Unity Player.log after the game exits.
// - Writes a per-session diagnostics record beside the executable and
//   under %LOCALAPPDATA%\FluxRec.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Register WER local dumps for RecRoom.exe:
///   HKCU\Software\Microsoft\Windows\Windows Error Reporting\LocalDumps\RecRoom.exe
///   DumpFolder = <data_dir>\dumps, DumpType = 2 (full dump).
/// Fail-soft: logs and continues if the registry can't be written.
pub fn configure_wer_dumps(data_dir: &Path) {
    let dumps = data_dir.join("dumps");
    let _ = std::fs::create_dir_all(&dumps);
    let key = r"HKCU\Software\Microsoft\Windows\Windows Error Reporting\LocalDumps\RecRoom.exe";
    let folder = dumps.to_string_lossy().into_owned();
    let r1 = Command::new("reg")
        .args(["add", key, "/v", "DumpFolder", "/t", "REG_SZ", "/d", &folder, "/f"])
        .output();
    let r2 = Command::new("reg")
        .args(["add", key, "/v", "DumpType", "/t", "REG_DWORD", "/d", "2", "/f"])
        .output();
    let ok = matches!(r1, Ok(o) if o.status.success()) && matches!(r2, Ok(o) if o.status.success());
    crate::util::crash_log(&format!(
        "WER dumps for RecRoom.exe -> {} (registered: {ok})",
        dumps.to_string_lossy()
    ));
}

/// Unity Player.log candidates. Unity writes to
/// %USERPROFILE%\AppData\LocalLow\<Company>\<Product>\Player.log; the exact
/// company folder for the 2022 Rec Room build isn't confirmed from here,
/// so probe the plausible names and copy whichever exists.
fn unity_log_candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(p) = std::env::var("USERPROFILE") {
        roots.push(PathBuf::from(p).join("AppData").join("LocalLow"));
    }
    // %APPDATA% is ...\Roaming; LocalLow sits next to it.
    if let Ok(p) = std::env::var("APPDATA") {
        if let Some(parent) = PathBuf::from(p).parent() {
            roots.push(parent.join("LocalLow"));
        }
    }
    let mut out = Vec::new();
    for root in &roots {
        for company in ["Rec Room", "RecRoom", "Against Gravity", "Rec Room Inc"] {
            out.push(root.join(company).join("Rec Room").join("Player.log"));
        }
    }
    out
}

/// Copy the Unity Player.log (if found) into the data dir for diagnosis.
/// Returns the destination path on success.
pub fn collect_unity_log(data_dir: &Path) -> Option<PathBuf> {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for src in unity_log_candidates() {
        if src.is_file() {
            let dest = data_dir.join(format!("Player.log.{epoch}"));
            match std::fs::copy(&src, &dest) {
                Ok(_) => {
                    crate::util::crash_log(&format!(
                        "copied Unity log {} -> {}",
                        src.to_string_lossy(),
                        dest.to_string_lossy()
                    ));
                    return Some(dest);
                }
                Err(e) => {
                    crate::util::crash_log(&format!(
                        "could not copy Unity log {}: {e}",
                        src.to_string_lossy()
                    ));
                    return None;
                }
            }
        }
    }
    crate::util::crash_log("Unity Player.log not found in LocalLow candidates");
    None
}

/// Append a session diagnostics record. Written beside the executable AND
/// under %LOCALAPPDATA%\FluxRec (usually the same dir — written once then).
pub fn write_session_log(exe_dir: &Path, data_dir: &Path, text: &str) {
    use std::io::Write as _;
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = format!("-- game session (epoch {epoch}) --\n{text}\n");
    let mut dirs = vec![exe_dir, data_dir];
    dirs.dedup_by(|a, b| {
        let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
        let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
        ca == cb
    });
    for d in dirs {
        let _ = std::fs::create_dir_all(d);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.join("game-session.log"))
            .and_then(|mut f| f.write_all(entry.as_bytes()));
    }
}
