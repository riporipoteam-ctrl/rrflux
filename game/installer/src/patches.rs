//! Client-side text patches applied after the game client is installed.
//!
//! These are same-length byte replacements — no file size changes, no format
//! corruption. All patches are FAIL-SOFT: if a patch cannot be applied (file
//! missing, pattern not found), it logs a warning and continues. The game must
//! always end up installed and playable.

use std::fs;
use std::path::Path;

/// A same-length byte replacement patch.
struct BytePatch {
    /// File path relative to the install dir.
    file: &'static str,
    /// The exact bytes to find.
    from: &'static [u8],
    /// The replacement bytes (must be same length as `from`).
    to: &'static [u8],
    /// Human-readable description for logging.
    desc: &'static str,
}

/// Welcome screen: "Welcome to Rec Room" -> "Welcome to Flux Rec"
/// Found in RecRoom_Data/level32 at offset 943476 (2023 client).
/// Both strings are 18 characters — safe same-length replacement.
const WELCOME_PATCH: BytePatch = BytePatch {
    file: "RecRoom_Data/level32",
    from: b"Welcome to Rec Room",
    to: b"Welcome to Flux Rec",
    desc: "welcome screen text",
};

/// YouTube board video IDs. The 2023 client hardcodes stale 2023 Rec Room video
/// IDs in GameAssembly.dll. Replace with Ripo Team videos (all IDs are 11 chars,
/// YouTube video IDs are fixed length — safe same-length replacement).
///
/// Verified Ripo Team videos (public/embeddable as of 2026-09-24):
/// - 9yxvLdJXh5Q: Official Song "Dust And Destiny" Country Song
/// - dlLAVqbClVU: TheHauntingOfChris 7: The Final Tape
/// - nsrNhZWOA2U: RipoHangout Reveal Trailer
/// - wl2YtvlA6Z8: RipoHangout 10.0 Update Trailer
/// - cfcx07aXfAQ: HorizonOfDespair RRS OFFICIAL TRAILER
const YOUTUBE_PATCHES: &[BytePatch] = &[
    // NOTE: The old IDs below are placeholders — the actual stale 2023 IDs must
    // be extracted from GameAssembly.dll. These will be filled in once the
    // exact byte sequences are confirmed.
];

/// Apply all client patches. Fail-soft: logs warnings but never fails the install.
pub fn apply_client_patches(dir: &Path) {
    println!("[patches] Applying Flux Rec client patches...");

    // Welcome screen patch
    apply_patch(dir, &WELCOME_PATCH);

    // YouTube patches (if any are defined)
    for patch in YOUTUBE_PATCHES {
        apply_patch(dir, patch);
    }

    println!("[patches] Client patches complete.");
}

/// Apply a single byte patch. Returns true if applied, false if skipped.
fn apply_patch(dir: &Path, patch: &BytePatch) -> bool {
    // Safety: from and to must be same length
    if patch.from.len() != patch.to.len() {
        eprintln!(
            "[patches] WARNING: {} patch has mismatched lengths ({} vs {}); skipping.",
            patch.desc,
            patch.from.len(),
            patch.to.len()
        );
        return false;
    }

    let path = dir.join(patch.file);
    let data = match fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[patches] WARNING: cannot read {} for {} patch ({}); skipping.",
                patch.file, patch.desc, e
            );
            return false;
        }
    };

    // Find the pattern
    let pos = match find_subsequence(&data, patch.from) {
        Some(p) => p,
        None => {
            eprintln!(
                "[patches] WARNING: pattern not found in {} for {} patch; skipping (may already be patched).",
                patch.file, patch.desc
            );
            return false;
        }
    };

    // Apply the patch
    let mut patched = data;
    patched[pos..pos + patch.from.len()].copy_from_slice(patch.to);

    if let Err(e) = fs::write(&path, &patched) {
        eprintln!(
            "[patches] WARNING: cannot write {} for {} patch ({}); skipping.",
            patch.file, patch.desc, e
        );
        return false;
    }

    println!("[patches] Applied {} patch to {} (offset {}).", patch.desc, patch.file, pos);
    true
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_patch_is_same_length() {
        assert_eq!(WELCOME_PATCH.from.len(), WELCOME_PATCH.to.len());
        assert_eq!(WELCOME_PATCH.from, b"Welcome to Rec Room");
        assert_eq!(WELCOME_PATCH.to, b"Welcome to Flux Rec");
    }

    #[test]
    fn find_subsequence_works() {
        let hay = b"hello Welcome to Rec Room world";
        assert_eq!(find_subsequence(hay, b"Welcome to Rec Room"), Some(6));
        assert_eq!(find_subsequence(hay, b"not here"), None);
    }
}
