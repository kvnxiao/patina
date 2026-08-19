//! Byte-copy executors: single-file [`Copy`] and recursive [`CopyTree`].
//!
//! [`Copy`](crate::config::FileMode::Copy) writes a byte-for-byte copy of
//! the source file at each target.
//! [`CopyTree`](crate::config::FileMode::CopyTree) recursively mirrors a source
//! directory tree to each target, producing one completion record per copied
//! file so the per-object granularity matches the symlink walk.

use super::CompletionRecord;
use super::ExecutorError;
use super::LeafWrite;
use super::clear_foreign_entry;
use super::ensure_parent;
use super::with_sharing_violation_retry;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use ignore::gitignore::Gitignore;

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
        clear_foreign_entry(target)?;
        with_sharing_violation_retry(|| fs_err::copy(source, target)).map_err(|source_err| {
            ExecutorError::Io {
                path: target.to_path_buf(),
                source: source_err,
            }
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
///
/// `write` selects which leaves are (re)written: on a
/// fresh or fully-drifted tree the engine passes [`LeafWrite::All`] and every
/// leaf is copied as before; on a partially-drifted tree it passes
/// [`LeafWrite::Only`] with the plan-time `Update`/`Create` leaves so the
/// clean leaves keep their inode/mtime and are not rewritten. A skipped leaf
/// produces no [`CompletionRecord`]: only the leaves this executor actually
/// wrote are returned, so the orchestrator's progress cursor and hook-failure
/// reversal act solely on real writes.
///
/// `rules` filters the leaf enumeration: an ignored leaf is never copied and
/// never produces a record, and an ignored directory is not descended.
pub(super) fn copy_tree(
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
    write: LeafWrite<'_>,
    rules: &Gitignore,
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

    let relative_files = super::walk_files(source, rules)?;
    let mut records = Vec::new();
    for target in targets {
        for rel in &relative_files {
            if !write.includes(rel) {
                continue;
            }
            let file_source = source.join(rel);
            let file_target = target.join(rel);
            ensure_parent(&file_target)?;
            clear_foreign_entry(&file_target)?;
            with_sharing_violation_retry(|| fs_err::copy(&file_source, &file_target)).map_err(
                |source_err| ExecutorError::Io {
                    path: file_target.clone(),
                    source: source_err,
                },
            )?;
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

        let records = copy_tree(
            &src,
            std::slice::from_ref(&target),
            LeafWrite::All,
            &crate::ignore_rules::none(),
        )
        .expect("copy tree");

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
    fn copy_tree_only_writes_selected_leaves() {
        // Partial tree write: with a `LeafWrite::Only` set
        // naming one of two leaves, only that leaf is copied and only that
        // leaf yields a completion record. The unselected leaf is not written.
        let (_td, dir) = utf8_tempdir();
        let src = dir.join("src");
        fs_err::create_dir_all(&src).expect("mkdir src");
        fs_err::write(src.join("a.txt"), b"a").expect("write a");
        fs_err::write(src.join("b.txt"), b"b").expect("write b");
        let target = dir.join("dest");

        let only: std::collections::BTreeSet<Utf8PathBuf> =
            std::iter::once(Utf8PathBuf::from("a.txt")).collect();
        let records = copy_tree(
            &src,
            std::slice::from_ref(&target),
            LeafWrite::Only(&only),
            &crate::ignore_rules::none(),
        )
        .expect("partial copy tree");

        assert_eq!(records.len(), 1, "only the selected leaf is recorded");
        assert_eq!(
            fs_err::read(target.join("a.txt")).expect("read a"),
            b"a",
            "the selected leaf is written"
        );
        assert!(
            !target.join("b.txt").exists(),
            "the unselected leaf must not be written"
        );
    }

    #[test]
    fn copy_file_replaces_a_symlink_target_with_a_regular_file() {
        // Without the clear, `fs_err::copy` follows the link back to the
        // source: the target stays a link and the repo file is rewritten in
        // place.
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"new bytes").expect("write source");
        let target = dir.join("dst");
        crate::test_util::symlink_file(&source, &target);

        copy_file(&source, std::slice::from_ref(&target)).expect("copy over a symlink");

        let meta = fs_err::symlink_metadata(&target).expect("stat target");
        assert!(
            meta.file_type().is_file(),
            "the target must become a regular file"
        );
        assert_eq!(fs_err::read(&target).expect("read target"), b"new bytes");
        assert_eq!(
            fs_err::read(&source).expect("read source"),
            b"new bytes",
            "the source is untouched"
        );
    }

    #[test]
    fn copy_file_over_a_dangling_symlink_writes_the_target_not_the_link_destination() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let ghost = dir.join("module").join("ghost");
        fs_err::create_dir_all(ghost.parent().expect("ghost parent")).expect("mkdir");
        fs_err::write(&ghost, b"x").expect("write ghost");
        let target = dir.join("dst");
        crate::test_util::symlink_file(&ghost, &target);
        fs_err::remove_file(&ghost).expect("dangle the target link");

        copy_file(&source, std::slice::from_ref(&target)).expect("copy over a dangling symlink");

        assert!(
            fs_err::symlink_metadata(&target)
                .expect("stat target")
                .file_type()
                .is_file(),
            "the target must become a regular file"
        );
        assert_eq!(fs_err::read(&target).expect("read target"), b"payload");
        assert!(
            !ghost.exists(),
            "no file may appear at the dead link's destination"
        );
    }

    #[test]
    fn copy_file_clears_a_directory_target() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let target = dir.join("dst");
        fs_err::create_dir_all(target.join("inner")).expect("mkdir target dir");

        copy_file(&source, std::slice::from_ref(&target)).expect("copy over a directory");

        assert!(
            fs_err::symlink_metadata(&target)
                .expect("stat target")
                .file_type()
                .is_file(),
            "the directory is cleared and the file written at the path"
        );
        assert_eq!(fs_err::read(&target).expect("read target"), b"payload");
    }

    #[test]
    fn copy_file_overwrites_a_regular_file_in_place() {
        // A hard-linked alias shares the target's inode; if the alias reads
        // back the new bytes, the overwrite reused the inode rather than
        // unlinking and recreating the entry.
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"new bytes").expect("write source");
        let target = dir.join("dst");
        fs_err::write(&target, b"old bytes").expect("write target");
        let alias = dir.join("alias");
        fs_err::hard_link(&target, &alias).expect("hard-link the target");

        copy_file(&source, std::slice::from_ref(&target)).expect("copy over a regular file");

        assert_eq!(
            fs_err::read(&alias).expect("read alias"),
            b"new bytes",
            "the hard-linked alias sees the new bytes, so the inode was reused"
        );
    }

    #[test]
    fn copy_tree_replaces_a_symlink_leaf_with_a_regular_file() {
        let (_td, dir) = utf8_tempdir();
        let src = dir.join("src");
        fs_err::create_dir_all(&src).expect("mkdir src");
        fs_err::write(src.join("a.txt"), b"a-bytes").expect("write a");
        let target = dir.join("dest");
        fs_err::create_dir_all(&target).expect("mkdir dest");
        crate::test_util::symlink_file(&src.join("a.txt"), &target.join("a.txt"));

        copy_tree(
            &src,
            std::slice::from_ref(&target),
            LeafWrite::All,
            &crate::ignore_rules::none(),
        )
        .expect("copy tree over a symlink leaf");

        let leaf = target.join("a.txt");
        assert!(
            fs_err::symlink_metadata(&leaf)
                .expect("stat leaf")
                .file_type()
                .is_file(),
            "the leaf must become a regular file"
        );
        assert_eq!(
            fs_err::read(src.join("a.txt")).expect("read source leaf"),
            b"a-bytes",
            "the source leaf is untouched"
        );
    }

    #[test]
    fn copy_tree_rejects_file_source() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("file");
        fs_err::write(&source, b"x").expect("write file");
        let err = copy_tree(
            &source,
            &[dir.join("t")],
            LeafWrite::All,
            &crate::ignore_rules::none(),
        )
        .expect_err("file source rejected");
        assert!(matches!(err, ExecutorError::NotADirectory { .. }));
    }
}
