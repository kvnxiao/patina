//! Symbolic-link executors: per-file [`Symlink`], atomic [`SymlinkDir`],
//! and per-leaf [`SymlinkTree`] (REQ-005, REQ-006).
//!
//! [`Symlink`](crate::config::FileMode::Symlink) links a single source
//! file to each target; when the source is a directory it walks the tree
//! and creates one symlink per file at the mirrored target path (the
//! default mode's per-file walk — the SPEC reserves atomic directory
//! symlinks for an explicit [`SymlinkDir`]). [`SymlinkDir`] creates one
//! symbolic link per target pointing at the source directory, never
//! walking into it. [`SymlinkTree`](crate::config::FileMode::SymlinkTree)
//! walks a directory source and links each leaf file at the mirrored
//! target path, leaving the intermediate target directories real.
//!
//! Cross-platform link creation routes through [`create_symlink`], which
//! picks the right OS primitive (file vs directory link on Windows, the
//! single `symlink` call on Unix) and maps a Windows privilege failure to
//! the typed [`ExecutorError::WindowsSymlinkPermission`].

use super::CompletionRecord;
use super::ExecutorError;
use super::ensure_parent;
use super::with_sharing_violation_retry;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Per-file [`Symlink`](crate::config::FileMode::Symlink) executor.
///
/// A regular-file source links once per target. A directory source walks
/// the tree and links every file at the mirrored target path, so a target
/// gains one symlink per source file.
pub(super) fn per_file_symlink(
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
) -> Result<Vec<CompletionRecord>, ExecutorError> {
    let metadata = fs_err::symlink_metadata(source).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ExecutorError::SourceMissing {
                path: source.to_path_buf(),
            }
        } else {
            ExecutorError::Io {
                path: source.to_path_buf(),
                source: err,
            }
        }
    })?;

    let mut records = Vec::new();
    if metadata.is_dir() {
        // Collect the source files once (deterministic order), then mirror
        // them under every target.
        let relative_files = super::walk_files(source)?;
        for target in targets {
            for rel in &relative_files {
                let file_source = source.join(rel);
                let file_target = target.join(rel);
                records.push(link_file(&file_source, &file_target)?);
            }
        }
    } else {
        for target in targets {
            records.push(link_file(source, target)?);
        }
    }
    Ok(records)
}

/// Per-leaf [`SymlinkTree`](crate::config::FileMode::SymlinkTree) executor:
/// walk the directory source and create one symbolic link per leaf file at
/// the mirrored target path, leaving intermediate target directories real
/// (REQ-006 / DEC-005).
///
/// The source must be a directory; a non-directory source is rejected with
/// [`ExecutorError::NotADirectory`], the same way [`dir_symlink`] rejects a
/// file source. Leaf enumeration uses the shared [`walk_files`] walk, which
/// collects only regular files in deterministic sorted order, so an empty
/// source subdirectory yields no entry — and therefore neither a target
/// directory nor a link. Each leaf is linked through [`link_file`], which
/// creates intermediate target directories on demand as real directories
/// and clears any pre-existing entry first (the engine has already backed
/// up a foreign regular file via `backup_before_overwrite`). One
/// [`CompletionRecord`] is returned per materialized leaf, in walk order
/// within each target.
///
/// [`walk_files`]: super::walk_files
pub(super) fn tree_symlink(
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
) -> Result<Vec<CompletionRecord>, ExecutorError> {
    let metadata = fs_err::symlink_metadata(source).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ExecutorError::SourceMissing {
                path: source.to_path_buf(),
            }
        } else {
            ExecutorError::Io {
                path: source.to_path_buf(),
                source: err,
            }
        }
    })?;
    if !metadata.is_dir() {
        return Err(ExecutorError::NotADirectory {
            path: source.to_path_buf(),
        });
    }

    // Collect the source leaves once (deterministic order), then mirror them
    // under every target as per-leaf links.
    let relative_files = super::walk_files(source)?;
    let mut records = Vec::with_capacity(targets.len() * relative_files.len());
    for target in targets {
        for rel in &relative_files {
            let file_source = source.join(rel);
            let file_target = target.join(rel);
            records.push(link_file(&file_source, &file_target)?);
        }
    }
    Ok(records)
}

/// Atomic [`SymlinkDir`](crate::config::FileMode::SymlinkDir) executor:
/// one directory symlink per target, no walk. A pre-existing entry at the
/// target is cleared first (the engine has already backed it up), so a
/// re-apply — or an apply over an existing target — converges rather than
/// failing with `EEXIST`.
pub(super) fn dir_symlink(
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
) -> Result<Vec<CompletionRecord>, ExecutorError> {
    let metadata = fs_err::symlink_metadata(source).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ExecutorError::SourceMissing {
                path: source.to_path_buf(),
            }
        } else {
            ExecutorError::Io {
                path: source.to_path_buf(),
                source: err,
            }
        }
    })?;
    if !metadata.is_dir() {
        return Err(ExecutorError::NotADirectory {
            path: source.to_path_buf(),
        });
    }

    let mut records = Vec::with_capacity(targets.len());
    for target in targets {
        ensure_parent(target)?;
        // Clear any pre-existing entry before linking. `create_symlink` fails
        // with `EEXIST` (os error 183 on Windows) against an occupied path, so
        // without this a re-apply (the target is already a directory symlink)
        // or a first apply over a pre-existing target would error rather than
        // converge. The engine runs `backup_before_overwrite` ahead of
        // `materialize`, so whatever is here — a real directory, a foreign
        // symlink — is already stashed and rollback can restore it; the removal
        // only clears the path the new link will occupy. Mirrors `link_file`.
        crate::fsx::remove_entry(target).map_err(|source| ExecutorError::Io {
            path: target.to_path_buf(),
            source,
        })?;
        create_symlink(source, target, LinkKind::Directory)?;
        records.push(CompletionRecord::symlink(
            source.to_path_buf(),
            target.to_path_buf(),
            source.to_path_buf(),
        ));
    }
    Ok(records)
}

/// Link a single file source at a single target, returning its record.
///
/// A pre-existing entry at `target` is removed before the link is created:
/// `create_symlink` fails with `EEXIST` (Unix) / os error 183 (Windows)
/// against an occupied path. The apply engine always runs
/// `backup_before_overwrite` ahead of `materialize`, so any pre-existing
/// regular file has already been stashed and rollback can restore it; the
/// removal here only clears the path the new link will occupy.
fn link_file(source: &Utf8Path, target: &Utf8Path) -> Result<CompletionRecord, ExecutorError> {
    ensure_parent(target)?;
    crate::fsx::remove_entry(target).map_err(|source| ExecutorError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    create_symlink(source, target, LinkKind::File)?;
    Ok(CompletionRecord::symlink(
        source.to_path_buf(),
        target.to_path_buf(),
        source.to_path_buf(),
    ))
}

/// Whether a link points at a file or a directory (the distinction
/// Windows requires; Unix ignores it).
#[derive(Debug, Clone, Copy)]
enum LinkKind {
    File,
    Directory,
}

/// Create a symbolic link at `target` pointing at `source`, picking the
/// OS-appropriate primitive and mapping a Windows privilege error to the
/// typed [`ExecutorError::WindowsSymlinkPermission`].
///
/// The OS primitive runs through [`with_sharing_violation_retry`] so the
/// forward-apply symlink write honours REQ-010's retry policy on Windows
/// (symlink creation is one of the "all file writes" the requirement names).
/// Off Windows the wrapper is a pass-through. The retry only fires on
/// `ERROR_SHARING_VIOLATION` (Win32 32), so it never masks the
/// `PermissionDenied` signal that [`classify_symlink_error`] maps to the
/// Developer-Mode / elevation error.
fn create_symlink(
    source: &Utf8Path,
    target: &Utf8Path,
    kind: LinkKind,
) -> Result<(), ExecutorError> {
    let result = with_sharing_violation_retry(|| create_symlink_os(source, target, kind));
    result.map_err(|err| classify_symlink_error(target, err))
}

#[cfg(unix)]
fn create_symlink_os(source: &Utf8Path, target: &Utf8Path, _kind: LinkKind) -> std::io::Result<()> {
    fs_err::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_symlink_os(source: &Utf8Path, target: &Utf8Path, kind: LinkKind) -> std::io::Result<()> {
    match kind {
        LinkKind::File => fs_err::os::windows::fs::symlink_file(source, target),
        LinkKind::Directory => fs_err::os::windows::fs::symlink_dir(source, target),
    }
}

/// Map a symlink-creation IO error to a typed [`ExecutorError`]. On
/// Windows a `PermissionDenied` is the Developer-Mode / elevation
/// signal; everywhere else (and for non-permission Windows errors) it is
/// a plain IO failure.
fn classify_symlink_error(target: &Utf8Path, err: std::io::Error) -> ExecutorError {
    if cfg!(windows) && err.kind() == std::io::ErrorKind::PermissionDenied {
        return ExecutorError::WindowsSymlinkPermission {
            target: target.to_path_buf(),
            source: err,
        };
    }
    ExecutorError::Io {
        path: target.to_path_buf(),
        source: err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("create tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
        (td, canonical)
    }

    /// Read a link's target and canonicalize it, so the assertion is
    /// independent of the platform's readlink representation (Windows
    /// returns the verbatim `\\?\` form; Unix returns the plain path).
    /// The CHK contract is "readlink target equals the canonical source",
    /// which holds when both sides are canonicalized.
    fn read_link_canonical(target: &Utf8Path) -> Utf8PathBuf {
        let raw = fs_err::read_link(target.as_std_path()).expect("read_link target");
        let link_target = Utf8PathBuf::from_path_buf(raw).expect("link target is utf-8");
        link_target
            .canonicalize_utf8()
            .expect("canonicalize link target")
    }

    fn canonical(path: &Utf8Path) -> Utf8PathBuf {
        path.canonicalize_utf8().expect("canonicalize path")
    }

    #[test]
    fn file_source_links_each_target() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("zshrc");
        fs_err::write(&source, b"export X=1").expect("write source");
        let t1 = dir.join("home1").join(".zshrc");
        let t2 = dir.join("home2").join(".zshrc");

        let records =
            per_file_symlink(&source, &[t1.clone(), t2.clone()]).expect("symlinks created");

        assert_eq!(records.len(), 2);
        assert_eq!(read_link_canonical(&t1), canonical(&source));
        assert_eq!(read_link_canonical(&t2), canonical(&source));
    }

    #[test]
    fn directory_source_walks_into_per_file_links() {
        let (_td, dir) = utf8_tempdir();
        let src_dir = dir.join("config");
        fs_err::create_dir_all(src_dir.join("nested")).expect("mkdir nested");
        fs_err::write(src_dir.join("a.conf"), b"a").expect("write a");
        fs_err::write(src_dir.join("nested").join("b.conf"), b"b").expect("write b");
        let target = dir.join("dest");

        let records =
            per_file_symlink(&src_dir, std::slice::from_ref(&target)).expect("walked links");

        // One symlink per source file, mirrored under the target.
        assert_eq!(records.len(), 2);
        assert_eq!(
            read_link_canonical(&target.join("a.conf")),
            canonical(&src_dir.join("a.conf"))
        );
        assert_eq!(
            read_link_canonical(&target.join("nested").join("b.conf")),
            canonical(&src_dir.join("nested").join("b.conf"))
        );
    }

    #[test]
    fn dir_symlink_creates_single_link_no_walk() {
        let (_td, dir) = utf8_tempdir();
        let src_dir = dir.join("nvim");
        fs_err::create_dir_all(&src_dir).expect("mkdir source");
        fs_err::write(src_dir.join("init.lua"), b"-- cfg").expect("write child");
        let target = dir.join(".config").join("nvim");

        let records = dir_symlink(&src_dir, std::slice::from_ref(&target)).expect("dir symlink");

        assert_eq!(records.len(), 1);
        // The target itself is the link; no per-file walk occurred.
        assert_eq!(read_link_canonical(&target), canonical(&src_dir));
        let target_meta = fs_err::symlink_metadata(&target).expect("target metadata");
        assert!(target_meta.file_type().is_symlink());
    }

    #[test]
    fn dir_symlink_rejects_file_source() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("not-a-dir");
        fs_err::write(&source, b"x").expect("write file");
        let err = dir_symlink(&source, &[dir.join("t")]).expect_err("file source rejected");
        assert!(matches!(err, ExecutorError::NotADirectory { .. }));
    }

    #[test]
    fn pre_existing_regular_file_target_is_replaced_by_link() {
        // CHK-033 apply leg: a target that already exists as a regular file
        // is cleared before linking (the engine has already backed it up), so
        // `create_symlink` does not fail with EEXIST / os-183.
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("zshrc");
        fs_err::write(&source, b"export X=1").expect("write source");
        let target = dir.join("home").join(".zshrc");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir home");
        fs_err::write(&target, b"original").expect("write pre-existing target");

        let records =
            per_file_symlink(&source, std::slice::from_ref(&target)).expect("link replaces file");

        assert_eq!(records.len(), 1);
        assert!(
            fs_err::symlink_metadata(&target)
                .expect("stat target")
                .file_type()
                .is_symlink(),
            "the pre-existing regular file must be replaced by a symlink"
        );
        assert_eq!(read_link_canonical(&target), canonical(&source));
    }

    #[test]
    fn missing_source_is_typed() {
        let (_td, dir) = utf8_tempdir();
        let err = per_file_symlink(&dir.join("absent"), &[dir.join("t")])
            .expect_err("missing source rejected");
        assert!(matches!(err, ExecutorError::SourceMissing { .. }));
    }

    #[test]
    fn tree_symlink_links_each_leaf_and_keeps_intermediate_dirs_real() {
        // CHK-012 executor leg: a directory source with `a.conf` and
        // `sub/b.conf` yields one symlink per leaf at the mirrored target
        // path, and the intermediate target directories are real, not links.
        let (_td, dir) = utf8_tempdir();
        let src_dir = dir.join("d");
        fs_err::create_dir_all(src_dir.join("sub")).expect("mkdir sub");
        fs_err::write(src_dir.join("a.conf"), b"a").expect("write a");
        fs_err::write(src_dir.join("sub").join("b.conf"), b"b").expect("write b");
        let target = dir.join("dest");

        let records = tree_symlink(&src_dir, std::slice::from_ref(&target)).expect("tree links");

        assert_eq!(records.len(), 2, "one record per leaf");
        let a = target.join("a.conf");
        let b = target.join("sub").join("b.conf");
        assert_eq!(read_link_canonical(&a), canonical(&src_dir.join("a.conf")));
        assert_eq!(
            read_link_canonical(&b),
            canonical(&src_dir.join("sub").join("b.conf"))
        );
        // The intermediate directories that host the leaves are real
        // directories, never symbolic links.
        for intermediate in [&target, &target.join("sub")] {
            let meta = fs_err::symlink_metadata(intermediate).expect("stat intermediate dir");
            assert!(
                meta.file_type().is_dir() && !meta.file_type().is_symlink(),
                "intermediate target dir {intermediate} must be a real directory"
            );
        }
    }

    #[test]
    fn tree_symlink_skips_empty_source_subdirectories() {
        // REQ-006: an empty source subdirectory produces neither a target
        // directory nor a link.
        let (_td, dir) = utf8_tempdir();
        let src_dir = dir.join("d");
        fs_err::create_dir_all(src_dir.join("empty")).expect("mkdir empty");
        fs_err::write(src_dir.join("a.conf"), b"a").expect("write a");
        let target = dir.join("dest");

        let records = tree_symlink(&src_dir, std::slice::from_ref(&target)).expect("tree links");

        assert_eq!(records.len(), 1, "only the one real leaf is linked");
        assert!(
            !target.join("empty").exists(),
            "an empty source subdir must produce no target directory"
        );
    }

    #[test]
    fn tree_symlink_replaces_pre_existing_regular_file_leaf() {
        // CHK-013 executor leg: a leaf-target path that already holds a
        // regular file is cleared (the engine has already backed it up) and
        // replaced by the link, so `create_symlink` does not fail with
        // EEXIST / os-183.
        let (_td, dir) = utf8_tempdir();
        let src_dir = dir.join("d");
        fs_err::create_dir_all(&src_dir).expect("mkdir source");
        fs_err::write(src_dir.join("a.conf"), b"new").expect("write source leaf");
        let target = dir.join("dest");
        fs_err::create_dir_all(&target).expect("mkdir target");
        fs_err::write(target.join("a.conf"), b"original").expect("write pre-existing leaf");

        let records = tree_symlink(&src_dir, std::slice::from_ref(&target)).expect("tree links");

        assert_eq!(records.len(), 1);
        let leaf = target.join("a.conf");
        assert!(
            fs_err::symlink_metadata(&leaf)
                .expect("stat leaf")
                .file_type()
                .is_symlink(),
            "the pre-existing regular file leaf must be replaced by a symlink"
        );
        assert_eq!(
            read_link_canonical(&leaf),
            canonical(&src_dir.join("a.conf"))
        );
    }

    #[test]
    fn tree_symlink_rejects_file_source() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("not-a-dir");
        fs_err::write(&source, b"x").expect("write file");
        let err = tree_symlink(&source, &[dir.join("t")]).expect_err("file source rejected");
        assert!(matches!(err, ExecutorError::NotADirectory { .. }));
    }

    #[test]
    fn tree_symlink_missing_source_is_typed() {
        let (_td, dir) = utf8_tempdir();
        let err =
            tree_symlink(&dir.join("absent"), &[dir.join("t")]).expect_err("missing source typed");
        assert!(matches!(err, ExecutorError::SourceMissing { .. }));
    }

    #[test]
    fn tree_symlink_re_apply_over_unchanged_source_is_a_noop() {
        // REQ-006 idempotency: a second materialize over the same source and
        // target re-creates the identical links without error (the
        // pre-existing link at each leaf is cleared and re-linked).
        let (_td, dir) = utf8_tempdir();
        let src_dir = dir.join("d");
        fs_err::create_dir_all(src_dir.join("sub")).expect("mkdir sub");
        fs_err::write(src_dir.join("a.conf"), b"a").expect("write a");
        fs_err::write(src_dir.join("sub").join("b.conf"), b"b").expect("write b");
        let target = dir.join("dest");

        let first = tree_symlink(&src_dir, std::slice::from_ref(&target)).expect("first apply");
        let second = tree_symlink(&src_dir, std::slice::from_ref(&target)).expect("second apply");

        assert_eq!(first.len(), second.len());
        let a = target.join("a.conf");
        let b = target.join("sub").join("b.conf");
        assert_eq!(read_link_canonical(&a), canonical(&src_dir.join("a.conf")));
        assert_eq!(
            read_link_canonical(&b),
            canonical(&src_dir.join("sub").join("b.conf"))
        );
    }
}
