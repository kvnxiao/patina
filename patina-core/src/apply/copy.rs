//! Byte-copy executors: single-file [`Copy`] and recursive [`CopyTree`]
//! (REQ-005).
//!
//! [`Copy`](crate::config::FileMode::Copy) writes a byte-for-byte copy of
//! the source file at each target.
//! [`CopyTree`](crate::config::FileMode::CopyTree) recursively mirrors a source
//! directory tree to each target, producing one completion record per copied
//! file so the per-object granularity matches the symlink walk.

use super::CompletionRecord;
use super::ExecutorError;
use super::ensure_parent;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Single-file [`Copy`](crate::config::FileMode::Copy) executor: copy the
/// source bytes to each target.
pub(super) fn copy_file(
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
) -> Result<Vec<CompletionRecord>, ExecutorError> {
    if !source.exists() {
        return Err(ExecutorError::SourceMissing {
            path: source.to_path_buf(),
        });
    }
    let mut records = Vec::with_capacity(targets.len());
    for target in targets {
        ensure_parent(target)?;
        fs_err::copy(source, target).map_err(|source_err| ExecutorError::Io {
            path: target.to_path_buf(),
            source: source_err,
        })?;
        records.push(CompletionRecord::copy(
            source.to_path_buf(),
            target.to_path_buf(),
        ));
    }
    Ok(records)
}

/// Recursive [`CopyTree`](crate::config::FileMode::CopyTree) executor:
/// mirror the source directory tree to each target, one record per
/// copied file.
pub(super) fn copy_tree(
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

    let relative_files = super::walk_files(source)?;
    let mut records = Vec::new();
    for target in targets {
        for rel in &relative_files {
            let file_source = source.join(rel);
            let file_target = target.join(rel);
            ensure_parent(&file_target)?;
            fs_err::copy(&file_source, &file_target).map_err(|source_err| ExecutorError::Io {
                path: file_target.clone(),
                source: source_err,
            })?;
            records.push(CompletionRecord::copy(file_source, file_target));
        }
    }
    Ok(records)
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

    #[test]
    fn copy_file_writes_bytes_to_each_target() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("agent.toml");
        fs_err::write(&source, b"name = 1").expect("write source");
        let t1 = dir.join("claude").join("agent.toml");
        let t2 = dir.join("codex").join("agent.toml");

        let records = copy_file(&source, &[t1.clone(), t2.clone()]).expect("copies");

        assert_eq!(records.len(), 2);
        assert_eq!(fs_err::read(&t1).expect("read t1"), b"name = 1");
        assert_eq!(fs_err::read(&t2).expect("read t2"), b"name = 1");
        // Copies are regular files, not symlinks.
        assert!(
            !fs_err::symlink_metadata(&t1)
                .expect("t1 metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn copy_file_missing_source_is_typed() {
        let (_td, dir) = utf8_tempdir();
        let err = copy_file(&dir.join("absent"), &[dir.join("t")]).expect_err("missing source");
        assert!(matches!(err, ExecutorError::SourceMissing { .. }));
    }

    #[test]
    fn copy_tree_mirrors_nested_files() {
        let (_td, dir) = utf8_tempdir();
        let src = dir.join("src");
        fs_err::create_dir_all(src.join("nested")).expect("mkdir nested");
        fs_err::write(src.join("top.txt"), b"top").expect("write top");
        fs_err::write(src.join("nested").join("deep.txt"), b"deep").expect("write deep");
        let target = dir.join("dest");

        let records = copy_tree(&src, std::slice::from_ref(&target)).expect("copy tree");

        assert_eq!(records.len(), 2);
        assert_eq!(
            fs_err::read(target.join("top.txt")).expect("read top"),
            b"top"
        );
        assert_eq!(
            fs_err::read(target.join("nested").join("deep.txt")).expect("read deep"),
            b"deep"
        );
    }

    #[test]
    fn copy_tree_rejects_file_source() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("file");
        fs_err::write(&source, b"x").expect("write file");
        let err = copy_tree(&source, &[dir.join("t")]).expect_err("file source rejected");
        assert!(matches!(err, ExecutorError::NotADirectory { .. }));
    }
}
