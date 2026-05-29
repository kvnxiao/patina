//! Backup-on-overwrite: stash the original bytes of a pre-existing target
//! before the executor overwrites it (REQ-014).

use super::BackupError;
use crate::journal::mirror_backup_path;
use camino::Utf8Path;

/// Back up the pre-existing entry at `target` into the per-apply backup
/// root `<backups_dir>/<ts>/` before the executor overwrites it.
///
/// The original is copied to [`mirror_backup_path`]'s mirrored location —
/// the same map crash recovery reads to restore an overwrite — and the
/// returned `bool` reports whether a backup was actually written:
///
/// - **`Ok(true)`** — the target pre-existed and its bytes were copied to the
///   backup tree. The caller may now safely overwrite the target.
/// - **`Ok(false)`** — the target did not exist, so there was nothing to back
///   up (REQ-014: a freshly created target produces no backup entry). The
///   caller proceeds to create the target.
///
/// Existence is probed with `symlink_metadata`, so a pre-existing symlink
/// at the target counts as present. The stash itself is kind-preserving via
/// the crate-internal `fsx::clone_entry` — the same primitive recovery and
/// rollback restore through — so a regular file round-trips byte-for-byte, a
/// symlink round-trips as a symlink (not flattened to its destination's
/// bytes), and a directory is captured recursively rather than aborting the
/// copy.
///
/// This writes only under `backups_dir`; it never touches the dotfiles
/// repository (REQ-014).
///
/// # Errors
///
/// Returns [`BackupError::Filesystem`] if the backup parent directory
/// cannot be created or the clone fails.
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use patina_core::backups::backup_before_overwrite;
///
/// let backups = Utf8Path::new("/state/patina/backups");
/// let made = backup_before_overwrite(backups, "20260528T120000Z", Utf8Path::new("/home/u/.zshrc"))?;
/// // `made` is true only if `~/.zshrc` already existed.
/// # let _ = made;
/// # Ok::<(), patina_core::backups::BackupError>(())
/// ```
pub fn backup_before_overwrite(
    backups_dir: impl AsRef<Utf8Path>,
    timestamp: &str,
    target: impl AsRef<Utf8Path>,
) -> Result<bool, BackupError> {
    let backups_dir = backups_dir.as_ref();
    let target = target.as_ref();

    // Probe with symlink_metadata so a pre-existing symlink is "present"
    // (matching recovery's probe). A missing target — the fresh-creation
    // case — yields no backup entry.
    if !crate::fsx::entry_present(target) {
        return Ok(false);
    }

    let backup = mirror_backup_path(backups_dir, timestamp, target);
    // `clone_entry` clears any stale backup, creates the parent chain, and
    // preserves the target's kind (file / symlink / directory).
    crate::fsx::clone_entry(target, &backup)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    struct Fixture {
        _temp: TempDir,
        root: Utf8PathBuf,
        backups: Utf8PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let backups = root.join("backups");
        fs_err::create_dir_all(&backups).expect("create backups dir");
        Fixture {
            _temp: temp,
            root,
            backups,
        }
    }

    #[test]
    fn pre_existing_target_is_copied_to_mirrored_backup_path() {
        let f = fixture();
        let target = f.root.join("home").join("u").join(".zshrc");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir target dir");
        fs_err::write(&target, b"original").expect("write original");

        let made =
            backup_before_overwrite(&f.backups, "TS", &target).expect("backup an existing target");

        assert!(made, "an existing target must report a backup was made");
        let backup = mirror_backup_path(&f.backups, "TS", &target);
        let bytes = fs_err::read(&backup).expect("read backup");
        assert_eq!(
            bytes, b"original",
            "the backup must hold the original bytes byte-for-byte"
        );
    }

    #[test]
    fn absent_target_produces_no_backup_entry() {
        let f = fixture();
        let target = f.root.join("home").join("u").join(".gitconfig");

        let made = backup_before_overwrite(&f.backups, "TS", &target)
            .expect("backup of an absent target is a clean no-op");

        assert!(!made, "an absent target must report no backup was made");
        let backup = mirror_backup_path(&f.backups, "TS", &target);
        assert!(
            !backup.exists(),
            "no backup entry may be written for a target that did not pre-exist"
        );
    }

    #[test]
    fn backup_round_trips_through_recovery_restore_path() {
        // The mirror map this writes is the same one recovery reads. Prove
        // the two agree by reading the bytes back from where recovery would
        // look (mirror_backup_path), not from a path this test recomputes
        // independently.
        let f = fixture();
        let target = f.root.join("etc").join("conf");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir");
        fs_err::write(&target, b"v1").expect("write");

        backup_before_overwrite(&f.backups, "TS", &target).expect("backup");

        let recovery_lookup = mirror_backup_path(&f.backups, "TS", &target);
        assert_eq!(
            fs_err::read(&recovery_lookup).expect("recovery reads the same path"),
            b"v1"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pre_existing_symlink_target_is_backed_up_as_a_symlink() {
        // C1 regression: a pre-existing symlink target must be stashed as a
        // symlink, not flattened to its destination's bytes — otherwise
        // rollback/recovery would restore a regular file where a link stood.
        let f = fixture();
        let target = f.root.join("home").join("u").join(".zshrc");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir target dir");
        fs_err::os::unix::fs::symlink("/elsewhere/zshrc", &target).expect("pre-existing symlink");

        let made = backup_before_overwrite(&f.backups, "TS", &target).expect("backup the symlink");

        assert!(made, "a present symlink must report a backup was made");
        let backup = mirror_backup_path(&f.backups, "TS", &target);
        assert!(
            fs_err::symlink_metadata(&backup)
                .expect("stat backup")
                .file_type()
                .is_symlink(),
            "the backup of a symlink must itself be a symlink"
        );
        assert_eq!(
            fs_err::read_link(&backup).expect("read backup link"),
            std::path::Path::new("/elsewhere/zshrc")
        );
    }

    #[test]
    fn pre_existing_directory_target_is_backed_up_not_aborted() {
        // C1 regression: a directory target (symlink-dir / copy-tree on
        // re-apply) must be captured recursively, not error out of the apply
        // the way a plain file copy of a directory would.
        let f = fixture();
        let target = f.root.join("home").join("u").join(".config");
        fs_err::create_dir_all(target.join("nested")).expect("mkdir target tree");
        fs_err::write(target.join("a.conf"), b"a").expect("write a");
        fs_err::write(target.join("nested").join("b.conf"), b"b").expect("write b");

        let made = backup_before_overwrite(&f.backups, "TS", &target)
            .expect("backing up a directory target must not error");

        assert!(made, "a present directory must report a backup was made");
        let backup = mirror_backup_path(&f.backups, "TS", &target);
        assert!(
            fs_err::symlink_metadata(&backup)
                .expect("stat backup")
                .file_type()
                .is_dir(),
            "the backup of a directory must itself be a directory"
        );
        assert_eq!(fs_err::read(backup.join("a.conf")).expect("read a"), b"a");
        assert_eq!(
            fs_err::read(backup.join("nested").join("b.conf")).expect("read b"),
            b"b"
        );
    }
}
