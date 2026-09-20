// Flux Rec bootstrap — shared helpers used by both the launcher and the
// persistent backend (same binary, `--backend` selects the backend).

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(
        h_wnd: *mut c_void,
        lp_text: *const u16,
        lp_caption: *const u16,
        u_type: u32,
    ) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn msgbox(text: &str) {
    let (t, c) = (wide(text), wide("Flux Rec"));
    unsafe {
        // MB_ICONERROR
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), 0x10);
    }
}

/// Directory of the currently running executable (the install dir in a
/// normal setup: %LOCALAPPDATA%\FluxRec).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    data_dir_opt().unwrap_or_else(|| PathBuf::from(".").join("FluxRec"))
}

pub fn data_dir_opt() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("APPDATA").map(PathBuf::from))
        .ok()
        .map(|p| p.join("FluxRec"))
}

pub fn crash_log(msg: &str) {
    use std::io::Write as _;
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("-- Flux Rec start (epoch {epoch}) --\n{msg}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("crash.log"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Redact anything that looks like a credential before a log leaves the
/// machine. Defense in depth: request logging no longer records query
/// strings at all, but old log lines may still contain them.
pub fn scrub_secrets(text: &str) -> String {
    let mut out = text.to_string();
    // Redact key=value pairs for known-sensitive keys, case-insensitive.
    // Runs to a fixed point so repeated keys are all covered.
    let keys = [
        "loginToken", "token", "authToken", "refresh_token", "refreshToken",
        "id_token", "idToken", "api_key", "apikey", "secret", "password", "auth",
    ];
    loop {
        let lower = out.to_lowercase();
        let mut changed = false;
        for key in keys {
            let needle = format!("{key}=");
            let mut from = 0;
            while let Some(rel) = lower[from..].find(&needle) {
                let val_start = from + rel + needle.len();
                let val_end = out[val_start..]
                    .find(|c: char| {
                        c == '&' || c == '"' || c == '\'' || c == ' ' || c == '\n' || c == '\r'
                    })
                    .map(|i| val_start + i)
                    .unwrap_or(out.len());
                if val_end > val_start {
                    out.replace_range(val_start..val_end, "***");
                    changed = true;
                    break;
                }
                // Empty value ("token=&...") — step past it to avoid looping.
                from = val_start + 1;
            }
            if changed {
                break;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

/// Show an error popup, log it, and quit. The launcher's one visible
/// failure path. Also uploads the (secret-scrubbed) logs to ix.io so
/// Tim can read them remotely.
pub fn fatal(msg: String) -> ! {
    crash_log(&msg);
    // Upload logs for remote diagnosis (fail-soft: if upload fails, just show local path).
    let log_url = upload_logs();
    let full_msg = match log_url {
        Some(url) => format!("{msg}\n\nLog uploaded for support: {url}"),
        None => format!("{msg}\n\nLog saved to %LOCALAPPDATA%\\FluxRec\\crash.log"),
    };
    msgbox(&full_msg);
    std::process::exit(1);
}

/// Upload crash.log + translator.log + panic.log to ix.io for remote
/// diagnosis, after scrubbing anything secret-looking. Returns the URL on
/// success, None on failure (fail-soft).
pub fn upload_logs() -> Option<String> {
    let dir = data_dir();
    let mut combined = String::new();
    for name in ["crash.log", "translator.log", "panic.log"] {
        if let Ok(content) = std::fs::read_to_string(dir.join(name)) {
            combined.push_str(&format!("=== {name} ===\n{content}\n\n"));
        }
    }
    if combined.is_empty() {
        return None;
    }
    let combined = scrub_secrets(&combined);
    // ix.io: POST with form field 'f:1' containing the text.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client
        .post("https://ix.io")
        .form(&[("f:1", combined)])
        .send()
        .ok()?;
    if resp.status().is_success() {
        resp.text()
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    }
}

fn process_name(pid: u32) -> Option<String> {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let name = line.split(',').next()?.trim_matches('"').to_string();
    if name.is_empty() || name.eq_ignore_ascii_case("INFO:") {
        None
    } else {
        Some(name)
    }
}

/// Who is squatting on the given local port? Returns (pid, image name).
pub fn port_holder(port: u16) -> Option<(u32, String)> {
    let out = Command::new("netstat").args(["-ano"]).output().ok()?;
    let want = format!(":{port}");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        // TCP    0.0.0.0:443    0.0.0.0:0    LISTENING    1234
        if p.len() < 5 || p[0] != "TCP" || p[3] != "LISTENING" {
            continue;
        }
        if !p[1].ends_with(&want) {
            continue;
        }
        if let Ok(pid) = p[4].parse::<u32>() {
            let name = process_name(pid).unwrap_or_else(|| "unknown program".into());
            return Some((pid, name));
        }
    }
    None
}

pub fn describe_bind_error(e: &str) -> String {
    match port_holder(443) {
        Some((pid, name)) => format!(
            "Flux Rec needs local port 443, but \"{name}\" (PID {pid}) is using it.\n\n\
             Close {name} and start Flux Rec again.\n\n\
             (technical: {e})"
        ),
        None => format!("The local game server failed to start:\n{e}"),
    }
}
