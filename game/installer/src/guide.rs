//! v0.1.17: one-time guided Windows Security exclusion setup.
//!
//! Background: Windows Security quarantined installer files on Armin's PC
//! (a Trojan:Win32/Suschil!rfn false positive), breaking installs mid-way.
//! The installer must NEVER change security settings itself, so instead of
//! a programmatic exclusion it shows this one-time guide that walks the
//! user through adding the install folder as an exclusion manually.
//!
//! Pure UI: two message boxes and opening the Windows Security settings
//! page. No Defender APIs are called, no setting is read or written.
//! Everything is fail-soft — a guide that cannot show must never block
//! the install.

use std::path::Path;

/// Marker file: the guide is shown at most once per install dir.
const MARKER: &str = ".defender_guide_shown";

/// Show the one-time exclusion guide unless the marker file says it was
/// already shown. Never blocks the install on any failure.
pub(crate) fn maybe_show_defender_guide(dir: &Path) {
    if dir.join(MARKER).exists() {
        return;
    }
    #[cfg(windows)]
    show_guide_windows(dir);
    // Best-effort marker so the guide only ever nags once. Creating the
    // dir here is harmless — run_install creates it anyway moments later.
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(MARKER), b"v0.1.18\n");
}

/// Windows implementation: OK/Cancel intro, optional settings page, then
/// step-by-step instructions. All failures fall through to the install.
#[cfg(windows)]
fn show_guide_windows(dir: &Path) {
    use crate::message_box;
    use crate::message_box_ok_cancel;

    let dir_s = dir.to_string_lossy();
    let intro = format!(
        "Windows Security sometimes quarantines Flux Rec's files while they \
         download (a false positive), which breaks the install halfway.\n\n\
         To prevent this, add the install folder as an exclusion in Windows \
         Security. It takes about 20 seconds and you only do it once:\n\n    \
         {dir_s}\n\n\
         Press OK and I will open Windows Security for you, then show the steps.\n\
         Press Cancel to skip (the install continues, but files may get quarantined)."
    );
    if !message_box_ok_cancel("Flux Rec Setup", &intro) {
        return;
    }
    open_windows_security_settings();
    let steps = format!(
        "In Windows Security:\n\n\
         1. Click \"Virus & threat protection\"\n\
         2. Under \"Virus & threat protection settings\", click \"Manage settings\"\n\
         3. Scroll down to \"Exclusions\" and click \"Add or remove exclusions\"\n\
         4. Click \"Add an exclusion\" -> \"Folder\"\n\
         5. Select this folder:\n\n    {dir_s}\n\n\
         6. Press OK below when done — the install starts right after."
    );
    message_box("Flux Rec Setup", &steps, false);
}

/// Opens the Windows Security settings page. Fire-and-forget: the return
/// value is intentionally ignored — if it fails, the step-by-step dialog
/// still tells the user where to go.
#[cfg(windows)]
fn open_windows_security_settings() {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let uri = HSTRING::from("ms-settings:windowsdefender");
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(uri.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fluxrec-guide-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn guide_writes_marker_and_skips_second_time() {
        let d = tmp_dir("marker");
        // First call: no marker yet — writes it (dialog is a no-op off Windows).
        maybe_show_defender_guide(&d);
        assert!(d.join(MARKER).exists(), "marker file should be written");
        let meta1 = std::fs::metadata(d.join(MARKER)).unwrap();
        // Second call: marker exists — returns early without touching it.
        maybe_show_defender_guide(&d);
        let meta2 = std::fs::metadata(d.join(MARKER)).unwrap();
        assert_eq!(
            meta1.modified().unwrap(),
            meta2.modified().unwrap(),
            "second call must not rewrite the marker"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn guide_never_blocks_when_dir_unwritable() {
        // A bogus dir must not panic or block: everything is fail-soft.
        let d = std::path::Path::new("/proc/definitely-not-a-dir-fluxrec");
        maybe_show_defender_guide(d);
    }
}
