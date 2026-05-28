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
/// at the target counts as present. The copy itself uses `fs_err::copy`,
/// mirroring how recovery restores the bytes, so a regular-file original
/// round-trips byte-for-byte.
///
/// This writes only under `backups_dir`; it never touches the dotfiles
/// repository (REQ-014).
///
/// # Errors
///
/// Returns [`BackupError::Filesystem`] if the backup parent directory
/// cannot be created or the copy fails.
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
    // (matching recovery's classify_target). A missing target — the
    // fresh-creation case — yields no backup entry.
    if fs_err::symlink_metadata(target).is_err() {
        return Ok(false);
    }

    let backup = mirror_backup_path(backups_dir, timestamp, target);
    if let Some(parent) = backup.parent() {
        fs_err::create_dir_all(parent)?;
    }
    fs_err::copy(target, &backup)?;
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
}
