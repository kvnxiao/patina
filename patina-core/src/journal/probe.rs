//! Filesystem probing and backup-path mirroring for crash recovery.
//!
//! Recovery never trusts the advisory progress cursor;
//! it asks the real filesystem what state each planned target is in and
//! consults the per-apply backup directory to decide how to reverse the
//! operation. This module owns those two pure-ish helpers:
//!
//! - [`mirror_backup_path`] computes where the backup of a given target lives
//!   under `<backups>/<ts>/`. The mapping mirrors the target's absolute path
//!   beneath the timestamped backup root, matching the layout the backup writer
//!   writes. Recovery is the first reader of that layout, so the mapping is
//!   defined here and the backup writer reuses it.
//! - [`classify_target`] reads the target path and reports whether it currently
//!   **exists** (as any kind of entry, including a symlink) or is **absent**.
//!   Recovery pairs that with backup presence to choose between restoring
//!   original bytes and deleting a fresh creation.
//!
//! The probe is deliberately coarse. Recovery only requires
//! that completed operations be reversed to the pre-apply state using
//! backups and inverse ops; it does not require distinguishing a
//! half-written copy from a fully-written one, because the reversal is
//! the same either way — restore the backup (overwrite) or delete the
//! target (fresh creation). A finer pre-state-hash probe can layer on
//! later when the plan records per-operation hashes; the `Probe` enum is
//! `non_exhaustive` to keep that door open.

use super::PlannedOperation;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// The observed filesystem state of a planned target at recovery time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Probe {
    /// The target path currently resolves to an entry on disk — a
    /// regular file, directory, or symlink. (Symlinks are detected via
    /// `symlink_metadata`, so a dangling link still counts as present.)
    Present,
    /// The target path does not exist.
    Absent,
}

/// Probe the filesystem for the current state of an operation's target.
///
/// Uses `symlink_metadata` so a symbolic link — including a dangling one
/// a partially-applied symlink op may have left — is reported as
/// [`Probe::Present`] rather than being followed to a missing destination.
#[must_use = "the probe result decides whether the operation is reversed"]
pub fn classify_target(target: &Utf8Path) -> Probe {
    match fs_err::symlink_metadata(target) {
        Ok(_) => Probe::Present,
        Err(_) => Probe::Absent,
    }
}

/// The absolute target path an operation writes to.
#[must_use = "recovery needs the target path to probe and reverse the operation"]
pub fn operation_target(op: &PlannedOperation) -> &str {
    match op {
        PlannedOperation::Symlink { target, .. }
        | PlannedOperation::Render { target, .. }
        | PlannedOperation::Copy { target, .. } => target,
    }
}

/// Compute the backup path for `target` under the per-apply backup root
/// `<backups_dir>/<ts>/`.
///
/// The target's absolute path is mirrored beneath the timestamped root:
/// the platform's path prefix (the leading `/` on Unix, the `C:\` drive
/// prefix on Windows) is folded into ordinary path components so the
/// backup tree can hold targets from any volume without collision. This
/// is the inverse map the backup writer applies before an overwrite,
/// and the map recovery applies to find the original bytes.
///
/// # Examples
///
/// ```
/// use camino::Utf8Path;
/// use patina_core::journal::mirror_backup_path;
///
/// let backups = Utf8Path::new("/state/patina/backups");
/// let got = mirror_backup_path(backups, "20260528T120000Z", Utf8Path::new("/home/u/.zshrc"));
/// assert!(got.as_str().contains("20260528T120000Z"));
/// assert_eq!(got.file_name(), Some(".zshrc"));
/// ```
#[must_use = "the mirrored path locates the original bytes recovery restores"]
pub fn mirror_backup_path(
    backups_dir: &Utf8Path,
    timestamp: &str,
    target: &Utf8Path,
) -> Utf8PathBuf {
    let mut path = backups_dir.join(timestamp);
    for component in mirror_components(target) {
        path.push(component);
    }
    path
}

/// Decompose an absolute target path into the ordinary directory/file
/// components that mirror beneath the backup root, dropping the platform
/// root/prefix and any `.` / `..` so the mirror is a pure containment of
/// the target beneath `<backups>/<ts>/`.
fn mirror_components(target: &Utf8Path) -> Vec<String> {
    use camino::Utf8Component;

    target
        .components()
        .filter_map(|component| match component {
            // The leading `/` (Unix) and the `C:\` prefix / drive
            // (Windows) are folded away: the backup root supplies the
            // anchor, and a Windows drive letter is preserved as a plain
            // component (`C`) so cross-volume targets do not collide.
            Utf8Component::Prefix(prefix) => {
                let raw = prefix.as_str();
                let cleaned: String = raw.chars().filter(|c| c.is_alphanumeric()).collect();
                if cleaned.is_empty() {
                    None
                } else {
                    Some(cleaned)
                }
            }
            Utf8Component::RootDir | Utf8Component::CurDir | Utf8Component::ParentDir => None,
            Utf8Component::Normal(part) => Some(part.to_owned()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn mirror_nests_target_beneath_timestamped_root() {
        let backups = Utf8Path::new("/state/backups");
        let got = mirror_backup_path(backups, "TS", Utf8Path::new("/home/u/.zshrc"));
        // The timestamp anchors the per-apply directory and the target's
        // own components nest under it in order.
        assert!(got.starts_with("/state/backups/TS"));
        assert_eq!(got.file_name(), Some(".zshrc"));
        // No path-root segment leaks into the mirror.
        assert!(!got.as_str().contains("//"));
    }

    #[test]
    fn mirror_strips_dot_and_parent_segments() {
        let backups = Utf8Path::new("/b");
        // A `..` in the target must not let the mirror escape the backup
        // root — recovery would otherwise look outside `<backups>/<ts>/`.
        let got = mirror_backup_path(backups, "TS", Utf8Path::new("/home/../home/u/x"));
        assert!(got.starts_with("/b/TS"));
        assert!(!got.as_str().contains(".."));
        assert_eq!(got.file_name(), Some("x"));
    }

    #[test]
    fn distinct_targets_mirror_to_distinct_backup_paths() {
        let backups = Utf8Path::new("/b");
        let a = mirror_backup_path(backups, "TS", Utf8Path::new("/home/u/.a"));
        let b = mirror_backup_path(backups, "TS", Utf8Path::new("/home/u/.b"));
        assert_ne!(a, b);
    }

    #[test]
    fn classify_reports_present_for_existing_and_absent_for_missing() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let present = dir.join("here");
        fs_err::write(&present, b"x").expect("write file");
        assert_eq!(classify_target(&present), Probe::Present);
        assert_eq!(classify_target(&dir.join("nope")), Probe::Absent);
    }

    #[test]
    fn operation_target_extracts_the_target_of_each_variant() {
        use crate::journal::Disposition;
        assert_eq!(
            operation_target(&PlannedOperation::symlink(
                "s",
                "/t/sym",
                Disposition::Create
            )),
            "/t/sym"
        );
        assert_eq!(
            operation_target(&PlannedOperation::render(
                "s",
                "/t/ren",
                Disposition::Create
            )),
            "/t/ren"
        );
        assert_eq!(
            operation_target(&PlannedOperation::copy("s", "/t/cp", Disposition::Create)),
            "/t/cp"
        );
    }
}
