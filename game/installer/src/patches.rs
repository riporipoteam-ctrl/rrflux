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

/// Welcome screen: "Welcome to Rec Room" -> "Welcome To Flux Rec"
/// Found in RecRoom_Data/level32 at offset 943476 (2023 client).
/// Both strings are 18 characters — safe same-length replacement.
const WELCOME_PATCH: BytePatch = BytePatch {
    file: "RecRoom_Data/level32",
    from: b"Welcome to Rec Room",
    to: b"Welcome To Flux Rec",
    desc: "welcome screen text",
};

/// Rec Room+ membership price text. The client shows "($10 USD value, ...)" — replace
/// with Flux Rec Tokens. Found in the RR+ UI AssetBundle.
const RRPLUS_USD_PATCH: BytePatch = BytePatch {
    file: "RecRoom_Data/StreamingAssets/aa/StandaloneWindows64/345318383a217ece0ae0ca51345ec71c.bundle",
    from: b"$10 USD value",
    to: b"10000 tokens!",
    desc: "RR+ USD to Flux Rec Tokens",
};

/// Rec Room+ Membership title. Stored as " Room+ Membership" (leading space, "Rec" is a
/// separate UI element). Found in the RR+ UI AssetBundle at offset 1443123.
const RRPLUS_TITLE_PATCH: BytePatch = BytePatch {
    file: "RecRoom_Data/StreamingAssets/aa/StandaloneWindows64/345318383a217ece0ae0ca51345ec71c.bundle",
    from: b" Room+ Membership",
    to: b" Flux Rec+ Member",
    desc: "RR+ Membership title to Flux Rec+",
};

/// "Join Rec Room+" button text. Found in the RR+ UI AssetBundle at offset 1551629.
const RRPLUS_JOIN_PATCH: BytePatch = BytePatch {
    file: "RecRoom_Data/StreamingAssets/aa/StandaloneWindows64/345318383a217ece0ae0ca51345ec71c.bundle",
    from: b"Join Rec Room+",
    to: b"Join Flux Rec+",
    desc: "Join RR+ button to Flux Rec+",
};

/// YouTube board video IDs. The 2023 client hardcodes stale 2023 Rec Room video
/// IDs in GameAssembly.dll. Replace with Ripo Team videos (all IDs are 11 chars,
/// YouTube board: The v0.1.14 patches targeted FinalIK documentation URLs (third-party
/// IK asset tutorial links), NOT the actual YouTube board video IDs. They have been
/// removed. The board requires a `videos` backend worker serving direct MP4 URLs —
/// Unity VideoPlayer cannot play youtube.com/watch URLs directly.
/// (See investigation 2026-09-24: no videos worker exists, zero YouTube handling in backend.)
const YOUTUBE_PATCHES: &[BytePatch] = &[
    // Intentionally empty — do not patch YouTube IDs without locating the actual board video IDs
    // via runtime traffic capture first.
];

/// Apply all client patches. Fail-soft: logs warnings but never fails the install.
pub fn apply_client_patches(dir: &Path) {
    println!("[patches] Applying Flux Rec client patches...");

    // Welcome screen patch
    apply_patch(dir, &WELCOME_PATCH);

    // RR+ USD to tokens patch
    apply_patch(dir, &RRPLUS_USD_PATCH);

    // RR+ Membership title patch
    apply_patch(dir, &RRPLUS_TITLE_PATCH);

    // Join RR+ button patch
    apply_patch(dir, &RRPLUS_JOIN_PATCH);

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
