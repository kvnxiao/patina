//! Filesystem probing and backup-path mirroring for crash recovery.
//!
//! Recovery never trusts the advisory progress cursor. It asks the
//! filesystem what state each planned target is in, and checks the
//! per-apply backup directory to decide how to reverse the operation.
//! Two pure helpers cover those operations:
//!
//! - [`mirror_backup_path`] computes where the backup of a given target lives
//!   under `<backups>/<ts>/`. The mapping mirrors the target's absolute path
//!   beneath the timestamped backup root, matching the layout the backup writer
//!   writes. Recovery is the first reader of that layout, so the mapping is
//!   defined here and the backup writer reuses it.
//! - [`classify_target`] reads the target path and reports whether it currently
//!   **exists** (as any kind of entry, including a symlink) or is **absent**.
//!
//! The probe is deliberately coarse. Recovery only requires
//! that completed operations be reversed to the pre-apply state using
//! backups and inverse ops. It does not need to distinguish a
//! half-written copy from a fully-written one, because the reversal is
//! the same either way: restore the backup, or, without one, delete a
//! `Create` target and leave an `Update` or `Remove` target in place. A finer
//! pre-state-hash probe can be added later, once the plan records
//! per-operation hashes. The `Probe` enum is `non_exhaustive` to allow that
//! extension without a breaking change.

use super::PlannedOperation;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// The observed filesystem state of a planned target at recovery time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Probe {
    /// The target path currently resolves to an entry on disk: a
    /// regular file, directory, or symlink. (Symlinks are detected via
    /// `symlink_metadata`, so a dangling link still counts as present.)
    Present,
    /// The target path does not exist.
    Absent,
}

/// Probe the filesystem for the current state of an operation's target.
///
/// Uses `symlink_metadata` so a symbolic link is reported as
/// [`Probe::Present`], not followed to a missing destination. This
/// includes a dangling link a partially-applied symlink op may have left.
pub fn classify_target(target: &Utf8Path) -> Probe {
    match fs_err::symlink_metadata(target) {
        Ok(_) => Probe::Present,
        Err(_) => Probe::Absent,
    }
}

/// The absolute target path an operation writes to.
pub(super) fn operation_target(op: &PlannedOperation) -> &str {
    match op {
        PlannedOperation::Symlink { target, .. }
        | PlannedOperation::Render { target, .. }
        | PlannedOperation::Copy { target, .. }
        | PlannedOperation::Remove { target } => target,
    }
}

/// Compute the backup path for `target` under the per-apply backup root
/// `<backups_dir>/<ts>/`.
///
/// The target's absolute path is mirrored beneath the timestamped root. The
/// platform's path prefix is folded into ordinary path components, so the
/// backup tree can hold targets from any volume without collision. That prefix
/// is the leading `/` on Unix, and the `C:\` drive prefix on Windows. The
/// backup writer applies this mapping before an overwrite, and recovery
/// applies it to find the original bytes.
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
/// components that mirror beneath the backup root. The platform root, and any
/// `.` / `..`, are dropped, so the mirror contains only the target's own path
/// beneath `<backups>/<ts>/`. A platform prefix maps to the components
/// returned by [`prefix_components`].
fn mirror_components(target: &Utf8Path) -> Vec<String> {
    use camino::Utf8Component;

    let mut mirrored = Vec::new();
    for component in target.components() {
        match component {
            Utf8Component::Prefix(prefix) => mirrored.extend(prefix_components(prefix)),
            Utf8Component::RootDir | Utf8Component::CurDir | Utf8Component::ParentDir => {}
            Utf8Component::Normal(part) => mirrored.push(part.to_owned()),
        }
    }
    mirrored
}

/// Prefix backup components that do not represent drive letters.
///
/// A drive prefix maps to one letter. The marker prevents a one-letter UNC
/// host from colliding with that drive.
const UNC_MARKER: &str = "__unc__";

/// Marker for a `\\.\<device>` prefix.
const DEVICE_MARKER: &str = "__device__";

/// Marker for a `\\?\<name>` prefix that is neither a drive nor a UNC share.
const VERBATIM_MARKER: &str = "__verbatim__";

/// Map a platform path prefix to backup-path components.
///
/// Disk prefixes retain the existing single-letter layout. UNC prefixes use
/// separate marker, host, and share components to prevent flattened names
/// such as `\\srv\a` and `\\sr\va` from colliding.
fn prefix_components(prefix: camino::Utf8PrefixComponent<'_>) -> Vec<String> {
    use camino::Utf8Prefix;

    match prefix.kind() {
        Utf8Prefix::Disk(letter) | Utf8Prefix::VerbatimDisk(letter) => {
            vec![char::from(letter).to_string()]
        }
        Utf8Prefix::UNC(host, share) | Utf8Prefix::VerbatimUNC(host, share) => {
            vec![UNC_MARKER.to_owned(), host.to_owned(), share.to_owned()]
        }
        Utf8Prefix::DeviceNS(device) => vec![DEVICE_MARKER.to_owned(), device.to_owned()],
        Utf8Prefix::Verbatim(name) => vec![VERBATIM_MARKER.to_owned(), name.to_owned()],
    }
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
        // root; recovery would otherwise look outside `<backups>/<ts>/`.
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

    #[cfg(windows)]
    #[test]
    fn two_unc_shares_that_flatten_alike_mirror_to_distinct_paths() {
        let backups = Utf8Path::new("C:/state/backups");
        let a = mirror_backup_path(backups, "TS", Utf8Path::new(r"\\srv\a\conf.toml"));
        let b = mirror_backup_path(backups, "TS", Utf8Path::new(r"\\sr\va\conf.toml"));
        assert_ne!(
            a, b,
            "the host/share separator must survive the mirror, or one backup \
             overwrites the other"
        );
        assert_eq!(a.file_name(), Some("conf.toml"));
        assert_eq!(b.file_name(), Some("conf.toml"));
    }

    #[cfg(windows)]
    #[test]
    fn a_unc_host_named_like_a_drive_does_not_collide_with_that_drive() {
        let backups = Utf8Path::new("C:/b");
        let unc = mirror_backup_path(backups, "TS", Utf8Path::new(r"\\C\share\conf.toml"));
        let disk = mirror_backup_path(backups, "TS", Utf8Path::new(r"C:\share\conf.toml"));
        assert_ne!(unc, disk, "a UNC host named `C` is not drive `C:`");
    }

    #[cfg(windows)]
    #[test]
    fn each_drive_keeps_its_single_letter_component() {
        let backups = Utf8Path::new("C:/b");
        let c = mirror_backup_path(backups, "TS", Utf8Path::new(r"C:\u\.zshrc"));
        let d = mirror_backup_path(backups, "TS", Utf8Path::new(r"D:\u\.zshrc"));
        assert_ne!(c, d, "cross-volume targets must not collide");
        assert!(
            c.starts_with(r"C:/b\TS\C\u"),
            "a drive must mirror to its bare letter, got: {c}"
        );
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
        assert_eq!(
            operation_target(&PlannedOperation::remove("/t/rm")),
            "/t/rm"
        );
    }
}
