//! Atomic inverse-operation replay.
//!
//! [`Replay::entry`] reverts every target of one managed entry to its
//! pre-apply state, and [`Replay::reaped`] restores every target the apply's
//! reap removed. The inverse-operation rule for an entry target has three
//! outcomes, in evaluation order. A target the apply recorded as `Unchanged`
//! is left in place, and is filtered out of the snapshot/roll-forward set
//! before either branch below is reached; the apply touched neither its bytes
//! nor its backup. A target with a backup is restored from it, because the
//! apply overwrote a pre-existing file. A target with no backup is deleted,
//! because the apply created it fresh. A reaped target with a backup is
//! restored from it, and a reaped target without a backup is left alone.
//!
//! Before either call replaces or deletes a live entry that differs from what
//! the record expects, it copies that entry under `<state>/patina/recovered/`
//! through the recovery [`Keeper`] and reports the copy. A live entry that
//! already matches its backup is left in place, so a rollback that stopped
//! partway does not copy or rewrite what it already restored.
//!
//! ## Atomicity mechanism
//!
//! Before mutating any target, each call first **snapshots** every target's
//! current post-apply state into a temporary staging directory beside the
//! backup root. It then reverts the targets in order. If any revert fails,
//! every target reverted so far is rolled forward from its snapshot to the
//! post-apply state it had before the call. The call's targets are therefore
//! left exactly as the last apply left them, with no partial restore. The
//! staging directory is removed on both the success and failure paths.

use super::RollbackError;
use crate::journal::Disposition;
use crate::journal::ExpectedTarget;
use crate::journal::Keeper;
use crate::journal::RECOVERED_DIR;
use crate::journal::RecoveredTarget;
use crate::journal::mirror_backup_path;
use crate::status::classify::target_matches;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Prefix of the staging directory that a [`Replay::entry`] or
/// [`Replay::reaped`] call creates under `<state>/patina/backups/`.
pub(crate) const STAGE_PREFIX: &str = ".rollback-stage-";

/// Reverts the reaped targets and managed entries of one committed apply.
pub(crate) struct Replay<'r> {
    backups_dir: Utf8PathBuf,
    timestamp: &'r str,
    protected: &'r [String],
    keeper: Keeper<'r>,
    on_kept: &'r mut dyn FnMut(&RecoveredTarget),
}

impl<'r> Replay<'r> {
    /// Prepare to revert the apply committed at `timestamp` under the
    /// per-machine state directory `state_dir`. `on_kept` receives each copy
    /// that the replay keeps of a live entry, before the replay replaces or
    /// deletes that entry.
    pub(crate) fn new(
        state_dir: &Utf8Path,
        timestamp: &'r str,
        protected: &'r [String],
        on_kept: &'r mut dyn FnMut(&RecoveredTarget),
    ) -> std::io::Result<Self> {
        Ok(Self {
            backups_dir: state_dir.join("backups"),
            timestamp,
            protected,
            keeper: Keeper::new(state_dir.join(RECOVERED_DIR), timestamp)?,
            on_kept,
        })
    }

    /// Revert every target in one managed entry to its pre-apply state, as
    /// one atomic set.
    ///
    /// `targets` are the entry's recorded targets, and `first_index` is the
    /// position of the first of them in the record. A kept copy of a target is
    /// stored under the target's position in the record. A target the apply
    /// recorded as [`Disposition::Unchanged`] is left in place: the apply
    /// skipped both its write and its backup. For a tree leaf the `Update`
    /// restore reads the whole-tree backup at the leaf's mirror path.
    ///
    /// When a leaf's backup mirror path passes through a symbolic link stashed
    /// in this cycle's backup tree, the apply replaced a whole-directory link
    /// with materialized leaves. The replay restores the root as one unit: it
    /// removes the live directory and clones the stashed link back. It does not
    /// restore a leaf through the link because that path would resolve into the
    /// repository. The replay keeps a copy of the live directory unless every
    /// file and link in it is a recorded leaf of `targets` that matches its
    /// record.
    ///
    /// # Errors
    ///
    /// - [`RollbackError::RollbackPartial`] when a target's revert fails; the
    ///   entry is rolled forward to its post-apply state before returning.
    /// - [`RollbackError::Filesystem`] when removing a leftover staged backup
    ///   (`<backup>.partial.<pid>`) or snapshotting fails, before any target
    ///   has been mutated.
    pub(crate) fn entry(
        &mut self,
        targets: &[ExpectedTarget],
        first_index: usize,
    ) -> Result<(), RollbackError> {
        let entry = targets.first().map_or(0, ExpectedTarget::entry);
        let mut units: Vec<Unit<'_>> = Vec::new();
        for (offset, expected) in targets.iter().enumerate() {
            if expected.disposition() == Disposition::Unchanged {
                continue;
            }
            let target = Utf8PathBuf::from(expected.target());
            let root = stashed_link_ancestor(&self.backups_dir, self.timestamp, &target);
            let is_tree = root.is_some();
            let path = root.unwrap_or(target);
            if self
                .protected
                .iter()
                .any(|target| crate::journal::paths_overlap(path.as_str(), target))
            {
                continue;
            }
            if units.iter().any(|unit| unit.path == path) {
                continue;
            }
            let expected = if is_tree {
                Expected::Tree(leaves_under(&path, targets))
            } else {
                Expected::Target(expected)
            };
            units.push(Unit {
                path,
                index: first_index.saturating_add(offset),
                expected,
            });
        }
        self.revert(&units, &entry.to_string(), |source| {
            RollbackError::RollbackPartial { entry, source }
        })
    }

    /// Restore every target in `reaped` that has a backup in this cycle, as
    /// one atomic set, and leave the others alone.
    ///
    /// A kept copy of the reaped target at position `i` in `reaped` is stored
    /// under `first_index + i`. A target whose mirror path passes through a
    /// symbolic link stashed in this cycle is left alone: restoring the target
    /// at the link's path also restores it, and a restore through the link
    /// would write into the link's destination.
    ///
    /// # Errors
    ///
    /// - [`RollbackError::ReapedPartial`] when a restore fails; every reaped
    ///   target is rolled forward to its post-apply state before returning.
    /// - [`RollbackError::Filesystem`] when removing a leftover staged backup
    ///   or snapshotting fails, before any target has been mutated.
    pub(crate) fn reaped(
        &mut self,
        reaped: &[String],
        first_index: usize,
    ) -> Result<(), RollbackError> {
        let mut units: Vec<Unit<'_>> = Vec::new();
        for (offset, target) in reaped.iter().enumerate().rev() {
            let path = Utf8PathBuf::from(target);
            let covered = stashed_link_ancestor(&self.backups_dir, self.timestamp, &path).is_some();
            let backup = mirror_backup_path(&self.backups_dir, self.timestamp, &path);
            if covered
                || !crate::fsx::entry_present(&backup)
                || self
                    .protected
                    .iter()
                    .any(|target| crate::journal::paths_overlap(path.as_str(), target))
            {
                continue;
            }
            units.push(Unit {
                path,
                index: first_index.saturating_add(offset),
                expected: Expected::Nothing,
            });
        }
        self.revert(&units, "reaped", |source| RollbackError::ReapedPartial {
            source,
        })
    }

    fn revert(
        &mut self,
        units: &[Unit<'_>],
        stage_name: &str,
        partial: impl FnOnce(std::io::Error) -> RollbackError,
    ) -> Result<(), RollbackError> {
        if units.is_empty() {
            return Ok(());
        }
        for unit in units {
            crate::fsx::remove_partial_siblings(&mirror_backup_path(
                &self.backups_dir,
                self.timestamp,
                &unit.path,
            ))?;
        }

        let stage = self
            .backups_dir
            .join(format!("{STAGE_PREFIX}{}-{stage_name}", self.timestamp));
        fs_err::create_dir_all(&stage)?;
        let paths: Vec<&Utf8Path> = units.iter().map(|unit| unit.path.as_path()).collect();
        let snapshots = match snapshot_targets(&stage, &paths) {
            Ok(snapshots) => snapshots,
            Err(err) => {
                remove_stage(&stage);
                return Err(RollbackError::Filesystem(err));
            }
        };

        let mut reverted: Vec<&Snapshot> = Vec::with_capacity(snapshots.len());
        for (unit, snapshot) in units.iter().zip(&snapshots) {
            // A failed revert can leave its target deleted or partial, so the
            // roll-forward includes it.
            reverted.push(snapshot);
            if let Err(source) = self.revert_unit(unit) {
                roll_forward(&reverted);
                remove_stage(&stage);
                return Err(partial(source));
            }
        }

        remove_stage(&stage);
        Ok(())
    }

    fn revert_unit(&mut self, unit: &Unit<'_>) -> std::io::Result<()> {
        let backup = mirror_backup_path(&self.backups_dir, self.timestamp, &unit.path);
        let restore = crate::fsx::entry_present(&backup);
        if crate::fsx::entry_present(&unit.path) {
            if restore && crate::fsx::same_entry(&unit.path, &backup)? {
                return Ok(());
            }
            if !unit.expected.matches(&unit.path)? {
                let kept = self.keeper.keep(unit.index, &unit.path)?;
                (self.on_kept)(&RecoveredTarget::new(unit.path.clone(), kept));
            }
        }
        // Presence is probed with `entry_present` rather than `exists`, so a
        // backed-up symlink whose destination is gone is still restored.
        if restore {
            crate::fsx::clone_entry(&backup, &unit.path)
        } else {
            crate::fsx::remove_entry(&unit.path)
        }
    }
}

/// One filesystem entry that a replay reverts, with the state the record
/// expects at it.
struct Unit<'a> {
    path: Utf8PathBuf,
    /// Record position under which a kept copy of the entry is stored.
    index: usize,
    expected: Expected<'a>,
}

/// The live state a commit record expects at a [`Unit`].
enum Expected<'a> {
    /// The recorded target's link or content.
    Target(&'a ExpectedTarget),
    /// A real directory whose files and links are exactly these recorded
    /// leaves, each matching its record.
    Tree(Vec<&'a ExpectedTarget>),
    /// No entry: the reap removed it.
    Nothing,
}

impl Expected<'_> {
    fn matches(&self, path: &Utf8Path) -> std::io::Result<bool> {
        match self {
            Self::Target(expected) => Ok(target_matches(expected)),
            Self::Tree(leaves) => tree_matches(path, leaves),
            Self::Nothing => Ok(!crate::fsx::entry_present(path)),
        }
    }
}

fn leaves_under<'a>(root: &Utf8Path, targets: &'a [ExpectedTarget]) -> Vec<&'a ExpectedTarget> {
    targets
        .iter()
        .filter(|expected| Utf8Path::new(expected.target()).starts_with(root))
        .collect()
}

/// Whether every file and link under the real directory `dir` is one of
/// `leaves` and matches its record. Links are compared, never followed.
fn tree_matches(dir: &Utf8Path, leaves: &[&ExpectedTarget]) -> std::io::Result<bool> {
    let meta = fs_err::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Ok(leaves
            .iter()
            .any(|leaf| Utf8Path::new(leaf.target()) == dir && target_matches(leaf)));
    }
    for entry in fs_err::read_dir(dir)? {
        let child = Utf8PathBuf::from_path_buf(entry?.path()).map_err(|bad| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("non-UTF-8 path under {dir}: {}", bad.display()),
            )
        })?;
        if !tree_matches(&child, leaves)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A target's staged post-apply state: either a regular file's bytes
/// (staged to `staged_path`), a symlink's link target, or absent.
struct Snapshot {
    target: Utf8PathBuf,
    state: SnapshotState,
}

enum SnapshotState {
    /// The target was a regular file; its bytes are staged at this path.
    File(Utf8PathBuf),
    /// The target was a symbolic link pointing at `link`, with the Windows
    /// flavour captured from the link itself.
    Symlink { link: Utf8PathBuf, dir_flavor: bool },
    /// The target did not exist at snapshot time.
    Absent,
}

/// Find the outermost strict ancestor of `target` whose backup mirror in this
/// cycle is a stashed whole-directory link and whose live counterpart is a
/// real directory of materialized leaves. Return `None` when no ancestor
/// matches.
///
/// Such an ancestor reverts as the unit. A leaf beneath it must never
/// revert individually: the leaf's own mirror path traverses the stashed
/// link into the repository, and after the root link is restored the live
/// leaf path would too. Ancestors are probed from the filesystem root toward
/// `target`, and the probe stops at the first stashed link because a deeper
/// mirror path would traverse it. The live-kind requirement keeps an ordinary
/// stashed link from folding unrelated targets beneath it: after a plain
/// re-link, the live counterpart is still a symlink, not a materialized
/// directory.
pub(crate) fn replaced_root_ancestor(
    backups_dir: &Utf8Path,
    timestamp: &str,
    target: &Utf8Path,
) -> Option<Utf8PathBuf> {
    let ancestor = stashed_link_ancestor(backups_dir, timestamp, target)?;
    let live_is_real_dir = fs_err::symlink_metadata(&ancestor)
        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
    live_is_real_dir.then_some(ancestor)
}

/// Find the outermost strict ancestor of `target` whose backup mirror in this
/// cycle is a symbolic link, whatever the live ancestor now is.
///
/// Ancestors are probed from the filesystem root toward `target`, so no probed
/// mirror path passes through a stashed link.
pub(crate) fn stashed_link_ancestor(
    backups_dir: &Utf8Path,
    timestamp: &str,
    target: &Utf8Path,
) -> Option<Utf8PathBuf> {
    let ancestors: Vec<&Utf8Path> = target.ancestors().skip(1).collect();
    ancestors
        .into_iter()
        .rev()
        .filter(|ancestor| !ancestor.as_str().is_empty())
        .find(|ancestor| {
            let backup = mirror_backup_path(backups_dir, timestamp, ancestor);
            fs_err::symlink_metadata(&backup).is_ok_and(|meta| meta.file_type().is_symlink())
        })
        .map(Utf8Path::to_path_buf)
}

/// Snapshot every target's current on-disk state into `stage`, returning one
/// [`Snapshot`] per target in order.
fn snapshot_targets(stage: &Utf8Path, targets: &[&Utf8Path]) -> std::io::Result<Vec<Snapshot>> {
    let mut snapshots = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let captured = match fs_err::symlink_metadata(target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let raw = fs_err::read_link(target)?;
                let link = Utf8PathBuf::from_path_buf(raw).map_err(|bad| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("non-UTF-8 symlink target: {}", bad.display()),
                    )
                })?;
                SnapshotState::Symlink {
                    link,
                    dir_flavor: crate::fsx::symlink_dir_flavor(meta.file_type()),
                }
            }
            Ok(meta) if meta.is_dir() => {
                // A directory target (symlink-dir restored, or a copy-tree
                // root) is staged by recursive copy so it can be rolled
                // forward verbatim.
                let staged = stage.join(format!("{index}.dir"));
                crate::fsx::copy_tree(target, &staged)?;
                SnapshotState::File(staged)
            }
            Ok(_) => {
                let staged = stage.join(format!("{index}.file"));
                fs_err::copy(target, &staged)?;
                SnapshotState::File(staged)
            }
            // A target whose parent is not a directory reports `ENOTDIR`
            // (`NotADirectory`) on Unix and `NotFound` on Windows; either way
            // the target genuinely cannot exist, so there is nothing to
            // snapshot. Treating both alike lets the real restore failure,
            // `create_dir_all` over the non-directory parent in
            // `revert_unit`, drive the partial-revert path identically on
            // every platform.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                SnapshotState::Absent
            }
            Err(err) => return Err(err),
        };
        snapshots.push(Snapshot {
            target: target.to_path_buf(),
            state: captured,
        });
    }
    Ok(snapshots)
}

/// Intentionally discard an IO result on a best-effort recovery path. The
/// set is already being abandoned, and there is no better state to converge
/// on than a best-effort restore. A secondary failure here is therefore
/// deliberately swallowed. This also keeps the `must_use` lint satisfied
/// without a bare `let _`.
fn ignore_io<T>(_result: std::io::Result<T>) {}

/// Roll already-reverted targets forward to the post-apply state captured in
/// their snapshots, so a failed set is left atomically untouched.
fn roll_forward(reverted: &[&Snapshot]) {
    for snapshot in reverted.iter().rev() {
        ignore_io(restore_snapshot(snapshot));
    }
}

/// Restore one target to the post-apply state captured in `snapshot`.
fn restore_snapshot(snapshot: &Snapshot) -> std::io::Result<()> {
    let target = &snapshot.target;
    ignore_io(crate::fsx::remove_entry(target));
    if let Some(parent) = target.parent()
        && !parent.as_str().is_empty()
    {
        fs_err::create_dir_all(parent)?;
    }
    match &snapshot.state {
        SnapshotState::File(staged) => {
            if fs_err::symlink_metadata(staged)?.is_dir() {
                crate::fsx::copy_tree(staged, target)
            } else {
                fs_err::copy(staged, target).map(|_| ())
            }
        }
        SnapshotState::Symlink { link, dir_flavor } => {
            crate::fsx::symlink_to(link, target, *dir_flavor)
        }
        SnapshotState::Absent => Ok(()),
    }
}

/// Remove a staging directory, swallowing errors. The next rollback removes a
/// leftover stage before it stages anything.
fn remove_stage(stage: &Utf8Path) {
    ignore_io(fs_err::remove_dir_all(stage));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::content_hash;
    use tempfile::TempDir;

    struct Env {
        _temp: TempDir,
        root: Utf8PathBuf,
        backups: Utf8PathBuf,
    }

    fn env() -> Env {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let backups = root.join("backups");
        fs_err::create_dir_all(&backups).expect("mkdir backups");
        Env {
            _temp: temp,
            root,
            backups,
        }
    }

    impl Env {
        fn replay(
            &self,
            ts: &str,
            revert: impl FnOnce(&mut Replay<'_>) -> Result<(), RollbackError>,
        ) -> (Result<(), RollbackError>, Vec<RecoveredTarget>) {
            let mut kept = Vec::new();
            let mut on_kept = |copy: &RecoveredTarget| kept.push(copy.clone());
            let mut replay =
                Replay::new(&self.root, ts, &[], &mut on_kept).expect("prepare the replay");
            let result = revert(&mut replay);
            drop(replay);
            (result, kept)
        }

        fn revert_entry(&self, ts: &str, targets: &[ExpectedTarget]) -> Vec<RecoveredTarget> {
            let (result, kept) = self.replay(ts, |replay| replay.entry(targets, 0));
            result.expect("revert the entry");
            kept
        }
    }

    fn write_backup(backups: &Utf8Path, ts: &str, target: &Utf8Path, bytes: &[u8]) {
        let path = mirror_backup_path(backups, ts, target);
        if let Some(parent) = path.parent() {
            fs_err::create_dir_all(parent).expect("mkdir backup parent");
        }
        fs_err::write(&path, bytes).expect("write backup");
    }

    fn content(path: &Utf8Path, bytes: &[u8], disposition: Disposition) -> ExpectedTarget {
        ExpectedTarget::Content {
            target: path.as_str().to_owned(),
            source: "/repo/source".to_owned(),
            hash: content_hash(bytes),
            entry: 0,
            disposition,
        }
    }

    fn create(path: &Utf8Path, bytes: &[u8]) -> ExpectedTarget {
        content(path, bytes, Disposition::Create)
    }

    fn update(path: &Utf8Path, bytes: &[u8]) -> ExpectedTarget {
        content(path, bytes, Disposition::Update)
    }

    fn kept_bytes(kept: &[RecoveredTarget]) -> Vec<(Utf8PathBuf, Vec<u8>)> {
        kept.iter()
            .flat_map(|copy| {
                copy.kept().iter().map(|path| {
                    (
                        copy.target().to_path_buf(),
                        fs_err::read(path).expect("read the kept copy"),
                    )
                })
            })
            .collect()
    }

    #[test]
    fn fresh_creation_is_deleted() {
        let e = env();
        let target = e.root.join("created");
        fs_err::write(&target, b"new").expect("write target");

        let kept = e.revert_entry("TS", &[create(&target, b"new")]);

        assert!(!target.exists(), "a fresh creation must be deleted");
        assert!(kept.is_empty(), "an unedited target gets no copy: {kept:?}");
    }

    #[test]
    fn overwrite_is_restored_from_backup() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("over");
        fs_err::write(&target, b"new").expect("write post-apply target");
        write_backup(&e.backups, ts, &target, b"original");

        let kept = e.revert_entry(ts, &[update(&target, b"new")]);

        assert_eq!(fs_err::read(&target).expect("read restored"), b"original");
        assert!(kept.is_empty(), "an unedited target gets no copy: {kept:?}");
    }

    #[test]
    fn an_edited_overwrite_is_kept_before_the_restore() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("over");
        fs_err::write(&target, b"edited").expect("edit the target after the apply");
        write_backup(&e.backups, ts, &target, b"original");

        let kept = e.revert_entry(ts, &[update(&target, b"applied")]);

        assert_eq!(fs_err::read(&target).expect("read restored"), b"original");
        assert_eq!(kept_bytes(&kept), [(target, b"edited".to_vec())]);
    }

    #[test]
    fn an_edited_creation_is_kept_before_the_delete() {
        let e = env();
        let target = e.root.join("created");
        fs_err::write(&target, b"edited").expect("edit the target after the apply");

        let kept = e.revert_entry("TS", &[create(&target, b"applied")]);

        assert!(!target.exists(), "a fresh creation must be deleted");
        assert_eq!(kept_bytes(&kept), [(target, b"edited".to_vec())]);
    }

    #[test]
    fn a_target_that_already_matches_its_backup_is_neither_kept_nor_rewritten() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("over");
        fs_err::write(&target, b"original").expect("restore the target by hand");
        let before =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs_err::OpenOptions::new()
            .write(true)
            .open(&target)
            .expect("open the target")
            .file()
            .set_modified(before)
            .expect("backdate the target");
        write_backup(&e.backups, ts, &target, b"original");

        let kept = e.revert_entry(ts, &[update(&target, b"applied")]);

        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(
            fs_err::metadata(&target)
                .expect("stat the target")
                .modified()
                .expect("read the mtime"),
            before,
            "the target must not be rewritten"
        );
    }

    #[test]
    fn a_leftover_staged_backup_is_removed_and_the_backup_restored() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("over");
        fs_err::write(&target, b"new").expect("write post-apply target");
        write_backup(&e.backups, ts, &target, b"original");
        let staged = Utf8PathBuf::from(format!(
            "{}.partial.4242",
            mirror_backup_path(&e.backups, ts, &target)
        ));
        fs_err::write(&staged, b"orig").expect("write a torn staged backup");

        e.revert_entry(ts, &[update(&target, b"new")]);

        assert_eq!(fs_err::read(&target).expect("read restored"), b"original");
        assert!(
            fs_err::symlink_metadata(&staged).is_err(),
            "rollback must remove the leftover staged backup"
        );
    }

    #[test]
    fn multi_target_entry_reverts_every_target() {
        let e = env();
        let ts = "TS";
        let pre_existing = e.root.join("had-backup");
        let fresh = e.root.join("fresh");
        fs_err::write(&pre_existing, b"new").expect("write t1");
        fs_err::write(&fresh, b"new").expect("write t2");
        write_backup(&e.backups, ts, &pre_existing, b"original");

        e.revert_entry(ts, &[update(&pre_existing, b"new"), create(&fresh, b"new")]);

        assert_eq!(
            fs_err::read(&pre_existing).expect("read restored"),
            b"original"
        );
        assert!(!fresh.exists(), "the fresh target must be deleted");
    }

    #[test]
    fn a_failed_second_target_rolls_the_first_forward_and_reports_partial() {
        let e = env();
        let ts = "TS";
        let first = e.root.join("first");
        fs_err::write(&first, b"post-apply-1").expect("write the first target");
        write_backup(&e.backups, ts, &first, b"original-1");
        let blocked_parent = e.root.join("blocked");
        fs_err::write(&blocked_parent, b"a file, not a directory").expect("occupy the parent");
        let second = blocked_parent.join("second");
        write_backup(&e.backups, ts, &second, b"original-2");
        let targets = [&first, &second].map(|target| ExpectedTarget::Content {
            target: target.to_string(),
            source: "/repo/source".to_owned(),
            hash: content_hash(b"post-apply-1"),
            entry: 3,
            disposition: Disposition::Update,
        });

        let (result, _kept) = e.replay(ts, |replay| replay.entry(&targets, 0));

        assert!(
            matches!(result, Err(RollbackError::RollbackPartial { entry: 3, .. })),
            "{result:?}"
        );
        assert_eq!(
            fs_err::read(&first).expect("read the first target"),
            b"post-apply-1",
            "the first target must be rolled forward to its post-apply state"
        );
    }

    #[test]
    fn unchanged_target_is_left_in_place() {
        let e = env();
        let target = e.root.join("unchanged");
        fs_err::write(&target, b"satisfied").expect("write target");

        e.revert_entry(
            "TS",
            &[content(&target, b"satisfied", Disposition::Unchanged)],
        );

        assert_eq!(
            fs_err::read(&target).expect("read untouched"),
            b"satisfied",
            "an Unchanged target must be left in place, not deleted"
        );
    }

    #[test]
    fn mixed_entry_reverts_create_and_update_but_leaves_unchanged() {
        let e = env();
        let ts = "TS";
        let created = e.root.join("created");
        let updated = e.root.join("updated");
        let unchanged = e.root.join("unchanged");
        fs_err::write(&created, b"new").expect("write created");
        fs_err::write(&updated, b"new").expect("write updated");
        fs_err::write(&unchanged, b"satisfied").expect("write unchanged");
        write_backup(&e.backups, ts, &updated, b"original");

        e.revert_entry(
            ts,
            &[
                create(&created, b"new"),
                update(&updated, b"new"),
                content(&unchanged, b"satisfied", Disposition::Unchanged),
            ],
        );

        assert!(!created.exists(), "the Create target must be deleted");
        assert_eq!(fs_err::read(&updated).expect("read restored"), b"original");
        assert_eq!(
            fs_err::read(&unchanged).expect("read untouched"),
            b"satisfied"
        );
    }

    #[test]
    fn a_reaped_target_is_restored_from_its_backup_and_one_without_a_backup_is_left_alone() {
        let e = env();
        let ts = "TS";
        let reaped = e.root.join("reaped");
        write_backup(&e.backups, ts, &reaped, b"reaped-bytes");
        let recreated = e.root.join("recreated");
        fs_err::write(&recreated, b"user-bytes").expect("recreate a path without a backup");
        let list = [reaped.to_string(), recreated.to_string()];

        let (result, kept) = e.replay(ts, |replay| replay.reaped(&list, 0));

        result.expect("restore the reaped set");
        assert_eq!(
            fs_err::read(&reaped).expect("read restored"),
            b"reaped-bytes"
        );
        assert_eq!(
            fs_err::read(&recreated).expect("read untouched"),
            b"user-bytes"
        );
        assert!(kept.is_empty(), "{kept:?}");
    }

    #[test]
    fn a_recreated_reaped_target_is_kept_before_the_restore() {
        let e = env();
        let ts = "TS";
        let reaped = e.root.join("reaped");
        write_backup(&e.backups, ts, &reaped, b"reaped-bytes");
        fs_err::write(&reaped, b"user-bytes").expect("recreate the reaped path");

        let (result, kept) = e.replay(ts, |replay| replay.reaped(&[reaped.to_string()], 5));

        result.expect("restore the reaped set");
        assert_eq!(
            fs_err::read(&reaped).expect("read restored"),
            b"reaped-bytes"
        );
        assert_eq!(kept_bytes(&kept), [(reaped, b"user-bytes".to_vec())]);
    }

    #[test]
    fn a_failed_reaped_restore_rolls_the_set_forward_and_reports_reaped_partial() {
        let e = env();
        let ts = "TS";
        let first = e.root.join("first");
        write_backup(&e.backups, ts, &first, b"first-bytes");
        let blocked_parent = e.root.join("blocked");
        fs_err::write(&blocked_parent, b"a file, not a directory").expect("occupy the parent");
        let second = blocked_parent.join("second");
        write_backup(&e.backups, ts, &second, b"second-bytes");
        let list = [second.to_string(), first.to_string()];

        let (result, _kept) = e.replay(ts, |replay| replay.reaped(&list, 0));

        assert!(
            matches!(result, Err(RollbackError::ReapedPartial { .. })),
            "{result:?}"
        );
        assert!(
            !crate::fsx::entry_present(&first),
            "the restored first target must be rolled forward to its reaped state"
        );
    }

    use crate::test_util::symlink_dir;

    #[test]
    fn protected_leaf_prevents_restoring_its_ancestor_link() {
        let e = env();
        let source = e.root.join("source");
        fs_err::create_dir_all(&source).expect("create source");
        fs_err::write(source.join("a"), "SOURCE").expect("write source");
        let root = e.root.join("out");
        fs_err::create_dir_all(&root).expect("create materialized tree");
        fs_err::write(root.join("a"), "PROMOTED").expect("write protected leaf");
        fs_err::write(root.join("b"), "SIBLING").expect("write sibling");
        let backup = mirror_backup_path(&e.backups, "TS", &root);
        fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir");
        symlink_dir(&source, &backup);
        let protected = vec![root.join("a").to_string()];
        let mut on_kept = |_: &RecoveredTarget| {};
        let mut replay = Replay::new(&e.root, "TS", &protected, &mut on_kept).expect("prepare");
        replay
            .entry(&[create(&root.join("b"), b"SIBLING")], 0)
            .expect("revert sibling entry");
        assert!(fs_err::symlink_metadata(&root).expect("stat root").is_dir());
        assert_eq!(
            fs_err::read_to_string(root.join("a")).expect("read a"),
            "PROMOTED"
        );
        assert_eq!(
            fs_err::read_to_string(source.join("a")).expect("read source"),
            "SOURCE"
        );
    }

    #[test]
    fn reaped_protected_target_is_not_restored_but_other_targets_are() {
        let e = env();
        let a = e.root.join("a");
        let b = e.root.join("b");
        for path in [&a, &b] {
            let backup = mirror_backup_path(&e.backups, "TS", path);
            fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir");
            fs_err::write(backup, "OLD").expect("write backup");
        }
        fs_err::write(&a, "KEPT").expect("write unmanaged target");
        let protected = vec![a.to_string()];
        let mut on_kept = |_: &RecoveredTarget| {};
        let mut replay = Replay::new(&e.root, "TS", &protected, &mut on_kept).expect("prepare");
        replay
            .reaped(&[a.to_string(), b.to_string()], 0)
            .expect("revert reap");
        assert_eq!(fs_err::read_to_string(a).expect("read a"), "KEPT");
        assert_eq!(fs_err::read_to_string(b).expect("read b"), "OLD");
    }

    #[test]
    fn retry_restores_a_missing_tree_root_as_the_backed_up_link() {
        let e = env();
        let ts = "TS";
        let source = e.root.join("source");
        fs_err::create_dir_all(&source).expect("create source");
        fs_err::write(source.join("a"), b"source bytes").expect("write source");
        let root = e.root.join("out");
        let backup = mirror_backup_path(&e.backups, ts, &root);
        fs_err::create_dir_all(backup.parent().expect("backup parent"))
            .expect("create backup parent");
        symlink_dir(&source, &backup);

        e.revert_entry(ts, &[create(&root.join("a"), b"source bytes")]);

        assert_eq!(
            fs_err::read_link(&root).expect("restored root is a link"),
            source.as_std_path()
        );
        assert_eq!(
            fs_err::read(source.join("a")).expect("read source"),
            b"source bytes"
        );
    }

    #[test]
    fn replaced_tree_root_reverts_as_the_link_and_leaves_the_repo_untouched() {
        let e = env();
        let ts = "TS";
        let repo_src = e.root.join("srcdir");
        fs_err::create_dir_all(&repo_src).expect("mkdir repo source");
        fs_err::write(repo_src.join("a.conf"), b"repo bytes").expect("write repo leaf");

        let root = e.root.join("out");
        fs_err::create_dir_all(&root).expect("mkdir live root");
        fs_err::write(root.join("a.conf"), b"repo bytes").expect("write live leaf");

        let root_backup = mirror_backup_path(&e.backups, ts, &root);
        fs_err::create_dir_all(root_backup.parent().expect("backup parent"))
            .expect("mkdir backup tree");
        symlink_dir(&repo_src, &root_backup);

        let kept = e.revert_entry(ts, &[create(&root.join("a.conf"), b"repo bytes")]);

        let meta = fs_err::symlink_metadata(&root).expect("stat reverted root");
        assert!(
            meta.file_type().is_symlink(),
            "the root must revert to the pre-apply whole-directory link"
        );
        assert_eq!(
            fs_err::read_link(&root).expect("readlink reverted root"),
            repo_src.as_std_path(),
            "the restored link points at the repository source"
        );
        assert_eq!(
            fs_err::read(repo_src.join("a.conf")).expect("read repo leaf"),
            b"repo bytes",
            "the repository leaf survives byte-for-byte"
        );
        assert!(kept.is_empty(), "an unedited root gets no copy: {kept:?}");
    }

    #[test]
    fn a_replaced_tree_root_holding_an_unrecorded_file_is_kept_before_the_restore() {
        let e = env();
        let ts = "TS";
        let repo_src = e.root.join("srcdir");
        fs_err::create_dir_all(&repo_src).expect("mkdir repo source");
        let root = e.root.join("out");
        fs_err::create_dir_all(&root).expect("mkdir live root");
        fs_err::write(root.join("a.conf"), b"repo bytes").expect("write live leaf");
        fs_err::write(root.join("notes"), b"user bytes").expect("add a user file");
        let root_backup = mirror_backup_path(&e.backups, ts, &root);
        fs_err::create_dir_all(root_backup.parent().expect("backup parent"))
            .expect("mkdir backup tree");
        symlink_dir(&repo_src, &root_backup);

        let kept = e.revert_entry(ts, &[create(&root.join("a.conf"), b"repo bytes")]);

        let copies: Vec<&Utf8PathBuf> = kept.iter().flat_map(RecoveredTarget::kept).collect();
        let [copy] = copies.as_slice() else {
            panic!("expected one kept copy of the root, got {kept:?}");
        };
        assert_eq!(
            fs_err::read(copy.join("notes")).expect("read the kept user file"),
            b"user bytes"
        );
    }

    #[test]
    fn a_stashed_link_whose_live_counterpart_is_still_a_symlink_does_not_fold() {
        let e = env();
        let ts = "TS";
        let repo_src = e.root.join("srcdir");
        fs_err::create_dir_all(&repo_src).expect("mkdir repo source");
        let root = e.root.join("out");
        symlink_dir(&repo_src, &root);
        let root_backup = mirror_backup_path(&e.backups, ts, &root);
        fs_err::create_dir_all(root_backup.parent().expect("backup parent"))
            .expect("mkdir backup tree");
        symlink_dir(&repo_src, &root_backup);

        assert_eq!(
            replaced_root_ancestor(&e.backups, ts, &root.join("a.conf")),
            None,
            "a live symlink root is not a replaced root"
        );
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_restores_a_dangling_directory_link_dir_flavoured() {
        use std::os::windows::fs::FileTypeExt as _;
        let e = env();
        let dest = e.root.join("dest_dir");
        fs_err::create_dir_all(&dest).expect("mkdir dest");
        let target = e.root.join("link");
        crate::test_util::symlink_dir(&dest, &target);
        fs_err::remove_dir(&dest).expect("dangle the link");
        let stage = e.root.join("stage");
        fs_err::create_dir_all(&stage).expect("mkdir stage");

        let snapshots = snapshot_targets(&stage, &[target.as_path()]).expect("snapshot");
        crate::fsx::remove_entry(&target).expect("clear the live link");
        restore_snapshot(snapshots.first().expect("one snapshot")).expect("restore");

        let meta = fs_err::symlink_metadata(&target).expect("stat restored");
        assert!(
            meta.file_type().is_symlink_dir(),
            "the roll-forward must restore the link dir-flavoured"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_backed_up_symlink_whose_destination_is_gone_is_restored_as_a_symlink() {
        let e = env();
        let ts = "TS";
        let target = e.root.join("link-target");
        fs_err::write(&target, b"new").expect("write post-apply target");
        let backup = mirror_backup_path(&e.backups, ts, &target);
        fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup tree");
        fs_err::os::unix::fs::symlink("/original/dest", &backup).expect("stash original symlink");

        e.revert_entry(ts, &[update(&target, b"new")]);

        let meta = fs_err::symlink_metadata(&target).expect("stat reverted target");
        assert!(
            meta.file_type().is_symlink(),
            "a pre-existing symlink must revert to a symlink, not a regular file"
        );
        assert_eq!(
            fs_err::read_link(&target).expect("readlink reverted target"),
            std::path::Path::new("/original/dest")
        );
    }
}
