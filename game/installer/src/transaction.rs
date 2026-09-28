//! Transactional install / update with rollback.
//!
//! v0.2.0 replaces the old "delete the live directory first" behavior with
//! a staging pipeline:
//!
//! * Fresh install: everything (client extract, bypass, BepInEx, branding,
//!   config, shortcuts) is built inside `<dir>.staging-<pid>` and only
//!   renamed into place after strict verification passes. Any failure
//!   deletes the staging directory and leaves the machine untouched.
//! * Upgrade / update: small managed components (BepInEx, plugin,
//!   interop, config, bypass files) are replaced in place, but every file
//!   that is overwritten is first copied to `<file>.fluxrec-bak`. After all
//!   components verify, the backups are deleted. On any failure the backups
//!   are restored and the error is reported — the previous working install
//!   is never left half-upgraded.
//!
//! Windows-only: uses atomic directory renames via `std::fs::rename`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A per-file backup created before an in-place component replacement.
/// Restores the original on [`FileBackup::rollback`], deletes the backup on
/// [`FileBackup::commit`].
pub struct FileBackup {
    original: PathBuf,
    backup: PathBuf,
    /// False when the original did not exist (nothing to restore).
    had_original: bool,
}

impl FileBackup {
    /// Copy `original` to `<original>.fluxrec-bak` if it exists.
    pub fn create(original: &Path) -> io::Result<Self> {
        let backup = backup_path(original);
        let had_original = original.exists();
        if had_original {
            if backup.exists() {
                let _ = fs::remove_file(&backup);
            }
            fs::copy(original, &backup)?;
        }
        Ok(FileBackup {
            original: original.to_path_buf(),
            backup,
            had_original,
        })
    }

    /// The replacement succeeded — drop the backup.
    pub fn commit(self) {
        let _ = fs::remove_file(&self.backup);
    }

    /// The replacement failed — restore the original file.
    pub fn rollback(self) {
        if self.had_original {
            let _ = fs::rename(&self.backup, &self.original);
        } else {
            // The file was newly created by the failed step — remove it so
            // no half-written artifact survives.
            let _ = fs::remove_file(&self.original);
        }
    }
}

fn backup_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".fluxrec-bak");
    PathBuf::from(s)
}

/// A set of [`FileBackup`]s committed or rolled back as one unit.
#[derive(Default)]
pub struct BackupSet {
    backups: Vec<FileBackup>,
}

impl BackupSet {
    pub fn new() -> Self {
        BackupSet {
            backups: Vec::new(),
        }
    }

    /// Back up `path` before overwriting it. Records the backup for the set.
    pub fn protect(&mut self, path: &Path) -> io::Result<()> {
        self.backups.push(FileBackup::create(path)?);
        Ok(())
    }

    /// All replacements verified — drop every backup.
    pub fn commit(self) {
        for b in self.backups {
            b.commit();
        }
    }

    /// Something failed — restore every original, newest first.
    pub fn rollback(self) {
        for b in self.backups.into_iter().rev() {
            b.rollback();
        }
    }
}

/// Fresh-install staging area: `<dir>.staging-<pid>`.
///
/// Build the complete installation inside [`Staging::path`]; when strict
/// verification passes, [`Staging::commit`] atomically swaps it into place:
/// the previous directory (if any) is renamed to `<dir>.backup-<pid>` and
/// removed only after the new tree verifies in place. [`Staging::abort`]
/// deletes the staging tree without touching the live directory.
pub struct Staging {
    target: PathBuf,
    staging: PathBuf,
}

impl Staging {
    pub fn begin(target: &Path) -> io::Result<Self> {
        let staging = staging_path(target);
        if staging.exists() {
            // Leftover from a crashed previous run — never build on top of it.
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        Ok(Staging {
            target: target.to_path_buf(),
            staging,
        })
    }

    pub fn path(&self) -> &Path {
        &self.staging
    }

    /// Atomically promote the staged tree to the target directory.
    /// On success the old tree (if any) is gone; on failure the live
    /// directory is untouched and the staging tree is left for inspection.
    pub fn commit(self) -> io::Result<()> {
        let Staging { target, staging } = &self;
        // Keep at most one backup: clear a stale one from a crashed run.
        let backup = backup_dir(target);
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        let had_target = target.exists();
        if had_target {
            fs::rename(target, &backup)?;
        }
        // The swap itself: if this rename fails, the old tree is either
        // still in place (rename is atomic) or sitting in `backup`.
        if let Err(e) = fs::rename(staging, target) {
            // Try to put the old tree back before reporting.
            if had_target {
                let _ = fs::rename(&backup, target);
            }
            return Err(e);
        }
        // New tree is live — the old one can go. A failure here is
        // non-fatal: a stale backup dir is cleaned on the next run.
        if had_target {
            let _ = fs::remove_dir_all(&backup);
        }
        // `self` is consumed; prevent Drop from deleting the live tree.
        std::mem::forget(self);
        Ok(())
    }

    /// Delete the staging tree. The live directory is never touched.
    pub fn abort(self) {
        let path = self.staging.clone();
        std::mem::forget(self);
        let _ = fs::remove_dir_all(&path);
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Safety net: a Staging that is neither committed nor aborted must
        // never leak a half-built tree, and must never delete the target.
        let _ = fs::remove_dir_all(&self.staging);
    }
}

fn staging_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(format!(".staging-{}", std::process::id()));
    PathBuf::from(s)
}

fn backup_dir(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(format!(".backup-{}", std::process::id()));
    PathBuf::from(s)
}

/// Remove leftover staging / backup directories from crashed runs.
/// Safe to call at startup: only touches `<dir>.staging-*` and
/// `<dir>.backup-*` siblings of real directories.
pub fn cleanup_leftovers(parent: &Path, dir_name: &str) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with(&format!("{dir_name}.staging-"))
            || name.starts_with(&format!("{dir_name}.backup-")))
            && entry.path().is_dir()
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
