// Game manifest handling: fetch, parse, diff, and persist.
// The manifest lists every game file with its sha256 and download URL.
// Updates are computed by diffing the remote manifest against the local
// copy — only changed/new files download, removed files get deleted.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    /// Build tag from the mirror (e.g. "0.4.5-tls-patch", "0.5.0-showdown").
    /// Absent in older manifests; `#[serde(default)]` keeps them parseable.
    #[serde(default)]
    pub version: Option<String>,
    pub files: Vec<FileEntry>,
}

pub struct Diff {
    /// Files whose sha256 changed or that are new.
    pub to_download: Vec<FileEntry>,
    /// Paths present locally but gone from the new manifest.
    pub to_delete: Vec<String>,
}

/// Diff the new manifest against the old one (None = nothing local).
pub fn diff(old: Option<&Manifest>, new: &Manifest) -> Diff {
    let old_map: HashMap<&str, &str> = old
        .map(|m| {
            m.files
                .iter()
                .map(|f| (f.path.as_str(), f.sha256.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let new_map: HashMap<&str, &str> = new
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha256.as_str()))
        .collect();

    let to_download = new
        .files
        .iter()
        .filter(|f| old_map.get(f.path.as_str()) != Some(&f.sha256.as_str()))
        .cloned()
        .collect();
    let to_delete = old_map
        .keys()
        .filter(|p| !new_map.contains_key(**p))
        .map(|s| s.to_string())
        .collect();
    Diff {
        to_download,
        to_delete,
    }
}

/// Parse raw manifest bytes.
pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("bad manifest: {e}"))
}

/// Where the local manifest copy + ETag live.
fn manifest_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("manifest.json")
}
fn etag_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("manifest.etag")
}

pub fn load_local(state_dir: &Path) -> Option<(Vec<u8>, Option<String>)> {
    let bytes = std::fs::read(manifest_path(state_dir)).ok()?;
    let etag = std::fs::read_to_string(etag_path(state_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some((bytes, etag))
}

pub fn save_local(state_dir: &Path, bytes: &[u8], etag: Option<&str>) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    std::fs::write(manifest_path(state_dir), bytes).map_err(|e| e.to_string())?;
    if let Some(e) = etag {
        let _ = std::fs::write(etag_path(state_dir), e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            sha256: sha.into(),
            url: format!("https://example.com/{path}"),
            size: None,
        }
    }

    #[test]
    fn diff_finds_changed_new_and_removed() {
        let old = Manifest {
            version: None,
            files: vec![
                entry("a.txt", "aaa"),
                entry("b.txt", "bbb"),
                entry("gone.txt", "ggg"),
            ],
        };
        let new = Manifest {
            version: None,
            files: vec![
                entry("a.txt", "aaa"),   // unchanged
                entry("b.txt", "b2"),    // changed
                entry("c.txt", "ccc"),   // new
            ],
        };
        let d = diff(Some(&old), &new);
        let dl: Vec<&str> = d.to_download.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(dl, vec!["b.txt", "c.txt"]);
        assert_eq!(d.to_delete, vec!["gone.txt"]);
    }

    #[test]
    fn diff_from_nothing_downloads_all() {
        let new = Manifest {
            version: None,
            files: vec![entry("a.txt", "aaa")],
        };
        let d = diff(None, &new);
        assert_eq!(d.to_download.len(), 1);
        assert!(d.to_delete.is_empty());
    }

    #[test]
    fn diff_identical_is_empty() {
        let m = Manifest {
            version: None,
            files: vec![entry("a.txt", "aaa")],
        };
        let d = diff(Some(&m), &m);
        assert!(d.to_download.is_empty());
        assert!(d.to_delete.is_empty());
    }
}
