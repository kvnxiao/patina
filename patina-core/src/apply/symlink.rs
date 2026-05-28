//! Symbolic-link executors: per-file [`Symlink`] and atomic
//! [`SymlinkDir`] (REQ-005).
//!
//! [`Symlink`](crate::config::FileMode::Symlink) links a single source
//! file to each target; when the source is a directory it walks the tree
//! and creates one symlink per file at the mirrored target path (the
//! default mode's per-file walk — the SPEC reserves atomic directory
//! symlinks for an explicit [`SymlinkDir`]). [`SymlinkDir`] creates one
//! symbolic link per target pointing at the source directory, never
//! walking into it.
//!
//! Cross-platform link creation routes through [`create_symlink`], which
//! picks the right OS primitive (file vs directory link on Windows, the
//! single `symlink` call on Unix) and maps a Windows privilege failure to
//! the typed [`ExecutorError::WindowsSymlinkPermission`].

use super::CompletionRecord;
use super::ExecutorError;
use super::ensure_parent;
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

/// Atomic [`SymlinkDir`](crate::config::FileMode::SymlinkDir) executor:
/// one directory symlink per target, no walk.
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
fn link_file(source: &Utf8Path, target: &Utf8Path) -> Result<CompletionRecord, ExecutorError> {
    ensure_parent(target)?;
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
fn create_symlink(
    source: &Utf8Path,
    target: &Utf8Path,
    kind: LinkKind,
) -> Result<(), ExecutorError> {
    let result = create_symlink_os(source, target, kind);
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
    fn missing_source_is_typed() {
        let (_td, dir) = utf8_tempdir();
        let err = per_file_symlink(&dir.join("absent"), &[dir.join("t")])
            .expect_err("missing source rejected");
        assert!(matches!(err, ExecutorError::SourceMissing { .. }));
    }
}
