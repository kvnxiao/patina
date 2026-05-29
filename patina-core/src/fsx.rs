//! Kind-preserving filesystem helpers shared by the apply, backup,
//! crash-recovery, and rollback paths.
//!
//! The backup/restore contract (REQ-014, REQ-019) requires that stashing a
//! pre-existing target and later restoring it round-trips the target's
//! *kind*: a symbolic link must come back a symbolic link, a directory a
//! directory, a regular file a regular file. A plain `fs::copy` cannot do
//! this — it follows symlinks (so a backed-up symlink would be flattened to
//! a regular file) and errors outright on a directory (so a pre-existing
//! directory target would abort the whole apply). Both directions of the
//! contract therefore route through [`clone_entry`], the single kind-aware
//! primitive, so backup and restore cannot disagree on what "the same
//! entry" means.
//!
//! These helpers are crate-internal plumbing; their only callers are the
//! `apply`, `backups`, `journal`, and `rollback` modules.

use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Whether an entry exists at `path`, detected with `symlink_metadata` so a
/// symbolic link — including a dangling one whose destination no longer
/// exists — is reported present rather than followed.
///
/// Callers deciding "is there a backup to restore from here?" must use this
/// rather than [`Utf8Path::exists`]: `exists` follows the link and would
/// report a backed-up symlink whose original destination is gone as absent,
/// causing restore to delete the target instead of recreating the link.
#[must_use = "the presence result decides whether to restore from or delete the entry"]
pub(crate) fn entry_present(path: &Utf8Path) -> bool {
    fs_err::symlink_metadata(path).is_ok()
}

/// Remove the entry at `path` if present, dispatching on directory vs
/// file/symlink and tolerating absence.
///
/// A symbolic link (even a dangling one) is removed as a link, never
/// followed. An already-absent path is a no-op.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the remove fails for any
/// reason other than the entry already being absent.
pub(crate) fn remove_entry(path: &Utf8Path) -> std::io::Result<()> {
    match fs_err::symlink_metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                fs_err::remove_dir_all(path)
            } else {
                fs_err::remove_file(path)
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Faithfully clone the filesystem entry at `from` to `to`, preserving its
/// kind: a symbolic link is recreated pointing at the same link target, a
/// directory is copied recursively (preserving any symlinks *within* it),
/// and a regular file is byte-copied. Any existing entry at `to` is removed
/// first and `to`'s parent chain is created.
///
/// This is the primitive both directions of the backup contract use:
/// `clone_entry(target, backup)` to stash a pre-existing target before an
/// overwrite (REQ-014), and `clone_entry(backup, target)` to restore it
/// during crash recovery or rollback (REQ-013 / REQ-019). Using one
/// function for both guarantees a symlink backed up is a symlink restored,
/// and a pre-existing directory target is captured rather than aborting the
/// apply.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when reading `from`'s metadata,
/// clearing `to`, creating `to`'s parent, or the copy/link itself fails. A
/// symbolic link whose target is not valid UTF-8 yields
/// [`std::io::ErrorKind::InvalidData`].
pub(crate) fn clone_entry(from: &Utf8Path, to: &Utf8Path) -> std::io::Result<()> {
    let meta = fs_err::symlink_metadata(from)?;
    remove_entry(to)?;
    if let Some(parent) = to.parent()
        && !parent.as_str().is_empty()
    {
        fs_err::create_dir_all(parent)?;
    }
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        symlink_to(&read_link_utf8(from)?, to)
    } else if file_type.is_dir() {
        copy_tree(from, to)
    } else {
        fs_err::copy(from, to).map(|_| ())
    }
}

/// Recursively clone the directory tree at `src` to `dst`, cloning each
/// child through [`clone_entry`] so nested symlinks are preserved rather
/// than followed. `dst` is created if absent.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the tree cannot be read or
/// a child cannot be cloned.
pub(crate) fn copy_tree(src: &Utf8Path, dst: &Utf8Path) -> std::io::Result<()> {
    fs_err::create_dir_all(dst)?;
    for entry in fs_err::read_dir(src)? {
        let entry = entry?;
        let from = Utf8PathBuf::from_path_buf(entry.path()).map_err(|bad| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("non-UTF-8 path under {src}: {}", bad.display()),
            )
        })?;
        let Some(name) = from.file_name() else {
            continue;
        };
        clone_entry(&from, &dst.join(name))?;
    }
    Ok(())
}

/// Create a symbolic link at `target` pointing at `link`, dispatching to the
/// platform-appropriate primitive.
///
/// On Windows the link flavour must match the destination kind, so a
/// directory destination uses a directory link and everything else a file
/// link; Unix has a single `symlink` call.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the link cannot be created.
#[cfg(unix)]
pub(crate) fn symlink_to(link: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    fs_err::os::unix::fs::symlink(link, target)
}

/// Create a symbolic link at `target` pointing at `link`, dispatching to the
/// platform-appropriate primitive.
///
/// On Windows the link flavour must match the destination kind, so a
/// directory destination uses a directory link and everything else a file
/// link; Unix has a single `symlink` call.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the link cannot be created.
#[cfg(windows)]
pub(crate) fn symlink_to(link: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    if fs_err::symlink_metadata(link).is_ok_and(|meta| meta.is_dir()) {
        fs_err::os::windows::fs::symlink_dir(link, target)
    } else {
        fs_err::os::windows::fs::symlink_file(link, target)
    }
}

/// Read the link target of the symbolic link at `path` as a UTF-8 path.
fn read_link_utf8(path: &Utf8Path) -> std::io::Result<Utf8PathBuf> {
    let raw = fs_err::read_link(path)?;
    Utf8PathBuf::from_path_buf(raw).map_err(|bad| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("non-UTF-8 symlink target at {path}: {}", bad.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        (td, path)
    }

    #[test]
    fn clone_entry_round_trips_a_regular_file() {
        let (_td, dir) = utf8_tempdir();
        let from = dir.join("src.txt");
        fs_err::write(&from, b"payload").expect("write src");
        let to = dir.join("nested").join("dst.txt");

        clone_entry(&from, &to).expect("clone file");

        assert!(
            fs_err::symlink_metadata(&to)
                .expect("stat dst")
                .file_type()
                .is_file()
        );
        assert_eq!(fs_err::read(&to).expect("read dst"), b"payload");
    }

    #[test]
    fn clone_entry_preserves_a_directory_tree() {
        let (_td, dir) = utf8_tempdir();
        let from = dir.join("tree");
        fs_err::create_dir_all(from.join("nested")).expect("mkdir nested");
        fs_err::write(from.join("a.txt"), b"a").expect("write a");
        fs_err::write(from.join("nested").join("b.txt"), b"b").expect("write b");
        let to = dir.join("copy");

        clone_entry(&from, &to).expect("clone tree");

        assert!(
            fs_err::symlink_metadata(&to)
                .expect("stat dst")
                .file_type()
                .is_dir(),
            "a directory source must clone to a directory, not error"
        );
        assert_eq!(fs_err::read(to.join("a.txt")).expect("read a"), b"a");
        assert_eq!(
            fs_err::read(to.join("nested").join("b.txt")).expect("read b"),
            b"b"
        );
    }

    #[test]
    fn clone_entry_overwrites_an_existing_destination() {
        let (_td, dir) = utf8_tempdir();
        let from = dir.join("src");
        fs_err::write(&from, b"new").expect("write src");
        let to = dir.join("dst");
        fs_err::write(&to, b"old").expect("write existing dst");

        clone_entry(&from, &to).expect("clone over existing");
        assert_eq!(fs_err::read(&to).expect("read dst"), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn clone_entry_preserves_a_symlink_as_a_symlink() {
        // The C1 regression: a symlink source must clone to a symlink with
        // the same link target, not be flattened to a regular file by a
        // following copy. The link's destination need not exist (a dangling
        // link round-trips just the same).
        let (_td, dir) = utf8_tempdir();
        let from = dir.join("link");
        fs_err::os::unix::fs::symlink("/some/where/original", &from).expect("create symlink");
        let to = dir.join("backup-of-link");

        clone_entry(&from, &to).expect("clone symlink");

        let meta = fs_err::symlink_metadata(&to).expect("stat clone");
        assert!(
            meta.file_type().is_symlink(),
            "a symlink source must clone to a symlink"
        );
        assert_eq!(
            fs_err::read_link(&to).expect("read clone link"),
            std::path::Path::new("/some/where/original"),
            "the cloned link must point at the same target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn entry_present_is_true_for_a_dangling_symlink() {
        // `Utf8Path::exists` follows the link and would say false; the whole
        // point of `entry_present` is to report the link itself as present.
        let (_td, dir) = utf8_tempdir();
        let link = dir.join("dangling");
        fs_err::os::unix::fs::symlink("/no/such/destination", &link).expect("create dangling link");

        assert!(!link.exists(), "exists() follows the dead link");
        assert!(entry_present(&link), "entry_present sees the link itself");
    }

    #[test]
    fn remove_entry_tolerates_absence_and_clears_each_kind() {
        let (_td, dir) = utf8_tempdir();
        remove_entry(&dir.join("never-existed")).expect("absent is a no-op");

        let file = dir.join("f");
        fs_err::write(&file, b"x").expect("write file");
        remove_entry(&file).expect("remove file");
        assert!(!entry_present(&file));

        let subdir = dir.join("d");
        fs_err::create_dir_all(subdir.join("inner")).expect("mkdir tree");
        fs_err::write(subdir.join("inner").join("g"), b"y").expect("write inner");
        remove_entry(&subdir).expect("remove dir tree");
        assert!(!entry_present(&subdir));
    }
}
