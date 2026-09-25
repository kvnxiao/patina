//! Crash recovery: converge backward to the pre-apply state via a
//! filesystem probe.
//!
//! [`recover_orphans`] scans the journal directory for *orphan* plans: a
//! `<ts>.plan` with neither a `<ts>.COMMIT` (the apply committed) nor a
//! `<ts>.ROLLED_BACK` (a prior rollback closed it out) sentinel. An orphan
//! indicates a `kill -9` mid-apply: the plan was made durable but the run
//! never reached commit.
//!
//! For each orphan, recovery:
//!
//! 1. Decodes the plan, reusing the version-envelope check so a plan from a
//!    newer binary is refused rather than mis-read.
//! 2. Probes the per-apply backup directory for each operation's target
//!    ([`mirror_backup_path`](super::mirror_backup_path)).
//! 3. **Reverses backward**, never forward. The disposition the plan recorded
//!    for the operation decides the outcome, evaluated in this order:
//!    - `Unchanged`: the apply neither backed up nor wrote this target, so the
//!      live entry is already the pre-apply entry. Leave it in place and do
//!      **not** consult the backup directory.
//!    - a backup exists: the apply overwrote, or was about to overwrite, a
//!      pre-existing entry. Restore the original from the backup.
//!    - no backup and `Create`: the target was absent before the apply. Remove
//!      whatever the apply created there, if anything.
//!    - no backup and `Update`: the apply backs up a pre-existing target before
//!      it writes it, so the write never started. Leave the target in place.
//!
//!    Each outcome leaves the target in its pre-apply state.
//! 4. Deletes the orphan `<ts>.plan` and `<ts>.progress` files once every
//!    operation has been reversed.
//!
//! Recovery is **idempotent**: the second run finds no orphan because the
//! first run removed the plan file. The second run leaves the filesystem
//! unchanged. Within a single run it is also
//! self-idempotent: restoring a backup over an already-restored target
//! rewrites identical bytes, and deleting an already-absent fresh target
//! is a no-op.
//!
//! The advisory progress cursor is **ignored** for the reversal decision:
//! recovery trusts the recorded disposition and the backup directory, not the
//! cursor's last record, which may not reflect how far the apply
//! actually got.
//!
//! # Examples
//!
//! ```no_run
//! use camino::Utf8Path;
//! use patina_core::journal::recover_orphans;
//!
//! let journal_dir = Utf8Path::new("/state/patina/journal");
//! let backups_dir = Utf8Path::new("/state/patina/backups");
//! let report = recover_orphans(journal_dir, backups_dir)?;
//! println!("recovered {} orphan plan(s)", report.recovered_timestamps().len());
//! # Ok::<(), patina_core::journal::JournalError>(())
//! ```

use super::COMMIT_SUFFIX;
use super::Disposition;
use super::JournalError;
use super::PLAN_SUFFIX;
use super::PROGRESS_SUFFIX;
use super::Plan;
use super::PlannedOperation;
use super::probe::mirror_backup_path;
use super::probe::operation_target;
use camino::Utf8Path;

/// Filename suffix for the rollback sentinel written by `patina rollback`.
/// Recovery treats a `<ts>.ROLLED_BACK` plan as already closed,
/// exactly like a committed one, and never re-reverses it.
pub const ROLLED_BACK_SUFFIX: &str = ".ROLLED_BACK";

/// Summary of one [`recover_orphans`] pass: the timestamps of the orphan
/// plans that were reversed and cleaned up, in lexical (chronological)
/// order. An empty list means there was no partial apply to recover.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    recovered: Vec<String>,
}

impl RecoveryReport {
    /// The `<ts>` timestamps of the orphan plans recovered this pass.
    pub fn recovered_timestamps(&self) -> &[String] {
        &self.recovered
    }

    /// Whether this pass found and recovered at least one orphan plan.
    pub fn recovered_any(&self) -> bool {
        !self.recovered.is_empty()
    }
}

/// Recover every orphan plan in `journal_dir`, reversing each backward to
/// the pre-apply filesystem state using backups under `backups_dir`, then
/// deleting the orphan plan and progress files.
///
/// Call this on apply startup, before computing a new plan. After it
/// returns, the engine proceeds with the user's new invocation as if no
/// prior partial work had occurred. Running it again with no intervening
/// apply is a no-op (idempotence).
///
/// # Errors
///
/// - [`JournalError::Filesystem`] if the journal directory cannot be read, or a
///   backup restore / target delete / orphan cleanup fails.
/// - [`JournalError::VersionMismatch`] / [`JournalError::Decode`] /
///   [`JournalError::Truncated`] if an orphan plan cannot be decoded.
pub fn recover_orphans(
    journal_dir: impl AsRef<Utf8Path>,
    backups_dir: impl AsRef<Utf8Path>,
) -> Result<RecoveryReport, JournalError> {
    let journal_dir = journal_dir.as_ref();
    let backups_dir = backups_dir.as_ref();

    let mut timestamps = orphan_timestamps(journal_dir)?;
    // Reverse orphans in chronological order so the report is
    // deterministic and any later-apply backup wins a same-target race in
    // a (pathological) multi-orphan state.
    timestamps.sort();

    let mut recovered = Vec::with_capacity(timestamps.len());
    for timestamp in timestamps {
        reverse_orphan(journal_dir, backups_dir, &timestamp)?;
        recovered.push(timestamp);
    }
    Ok(RecoveryReport { recovered })
}

/// Collect the `<ts>` of every plan file in `journal_dir` that has neither
/// a `COMMIT` nor a `ROLLED_BACK` sentinel beside it.
fn orphan_timestamps(journal_dir: &Utf8Path) -> Result<Vec<String>, JournalError> {
    if !journal_dir.exists() {
        // No journal directory yet means no prior apply, so nothing to do.
        return Ok(Vec::new());
    }

    let mut orphans = Vec::new();
    for entry in fs_err::read_dir(journal_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(timestamp) = name.strip_suffix(PLAN_SUFFIX) else {
            continue;
        };
        let committed = journal_dir
            .join(format!("{timestamp}{COMMIT_SUFFIX}"))
            .exists();
        let rolled_back = journal_dir
            .join(format!("{timestamp}{ROLLED_BACK_SUFFIX}"))
            .exists();
        if !committed && !rolled_back {
            orphans.push(timestamp.to_owned());
        }
    }
    Ok(orphans)
}

/// Reverse one orphan plan and delete its plan + progress files.
fn reverse_orphan(
    journal_dir: &Utf8Path,
    backups_dir: &Utf8Path,
    timestamp: &str,
) -> Result<(), JournalError> {
    let plan_path = journal_dir.join(format!("{timestamp}{PLAN_SUFFIX}"));
    let bytes = fs_err::read(&plan_path)?;
    let plan = Plan::decode(&bytes)?;

    for op in plan.operations() {
        reverse_operation(backups_dir, timestamp, op)?;
    }

    // The plan and progress files are removed only after every reversal
    // succeeds. A crash mid-recovery leaves the orphan in place, and the
    // next startup retries it. This retry is still idempotent: restoring
    // a backup rewrites the same bytes, and deleting an absent target is
    // a no-op.
    super::remove_if_present(&plan_path)?;
    super::remove_if_present(&journal_dir.join(format!("{timestamp}{PROGRESS_SUFFIX}")))?;
    Ok(())
}

/// Reverse a single planned operation back to its pre-apply state.
///
/// The disposition the plan recorded is authoritative at any crash point,
/// because the plan is fsync'd before any mutation. For a tree operation it is
/// the durable per-op aggregate, so a tree whose aggregate is `Unchanged` is
/// left whole.
///
/// The executor backs up a pre-existing target immediately before its write.
/// A missing backup therefore separates a `Create` target, which recovery
/// removes, from an `Update` target the apply never reached, which recovery
/// leaves in place.
///
/// Both restore and delete go through the kind-preserving [`crate::fsx`]
/// helpers. The original is therefore recreated as the same kind it was: a
/// symlink as a symlink, a directory as a directory. Backup presence is
/// probed with [`crate::fsx::entry_present`], so a backed-up symlink whose
/// destination is gone is still seen. `exists` would follow the dead link and
/// miss the backup.
fn reverse_operation(
    backups_dir: &Utf8Path,
    timestamp: &str,
    op: &PlannedOperation,
) -> Result<(), JournalError> {
    let disposition = op.disposition();
    if disposition == Disposition::Unchanged {
        return Ok(());
    }

    let target = Utf8Path::new(operation_target(op));
    let backup = mirror_backup_path(backups_dir, timestamp, target);

    if crate::fsx::entry_present(&backup) {
        return crate::fsx::clone_entry(&backup, target).map_err(JournalError::Filesystem);
    }
    match disposition {
        Disposition::Create => crate::fsx::remove_entry(target).map_err(JournalError::Filesystem),
        Disposition::Update | Disposition::Unchanged => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    struct Dirs {
        _temp: TempDir,
        journal: Utf8PathBuf,
        backups: Utf8PathBuf,
    }

    fn dirs() -> Dirs {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let journal = root.join("journal");
        let backups = root.join("backups");
        fs_err::create_dir_all(&journal).expect("create journal dir");
        fs_err::create_dir_all(&backups).expect("create backups dir");
        Dirs {
            _temp: temp,
            journal,
            backups,
        }
    }

    fn write_plan(journal: &Utf8Path, ts: &str, plan: &Plan) {
        let bytes = plan.encode().expect("encode plan");
        fs_err::write(journal.join(format!("{ts}{PLAN_SUFFIX}")), bytes).expect("write plan");
    }

    #[test]
    fn missing_journal_dir_is_a_clean_no_op() {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let report = recover_orphans(root.join("nope"), root.join("backups"))
            .expect("recovery on a missing journal dir succeeds");
        assert!(!report.recovered_any());
    }

    #[test]
    fn committed_plan_is_not_an_orphan() {
        let d = dirs();
        let ts = "20260528T100000Z";
        write_plan(&d.journal, ts, &Plan::new(vec![]));
        fs_err::write(d.journal.join(format!("{ts}{COMMIT_SUFFIX}")), []).expect("commit sentinel");

        let report = recover_orphans(&d.journal, &d.backups).expect("recovery");
        assert!(
            !report.recovered_any(),
            "a committed plan must be left alone"
        );
        // The committed plan file is untouched by recovery.
        assert!(d.journal.join(format!("{ts}{PLAN_SUFFIX}")).exists());
    }

    #[test]
    fn rolled_back_plan_is_not_an_orphan() {
        let d = dirs();
        let ts = "20260528T100000Z";
        write_plan(&d.journal, ts, &Plan::new(vec![]));
        fs_err::write(d.journal.join(format!("{ts}{ROLLED_BACK_SUFFIX}")), [])
            .expect("rolled-back sentinel");

        let report = recover_orphans(&d.journal, &d.backups).expect("recovery");
        assert!(!report.recovered_any());
    }

    #[test]
    fn unchanged_marked_orphan_target_is_left_in_place() {
        // An orphan plan whose target is marked `Unchanged` must be
        // preserved by recovery, even though no backup exists for it.
        // Apply skipped the write and the backup, so the live entry
        // already is the pre-apply entry.
        let d = dirs();
        let root = d.journal.parent().expect("journal has a parent");
        let ts = "20260528T130000Z";

        let target = root.join("home").join(".gitconfig");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir home");
        fs_err::write(&target, b"matches-the-source").expect("write unchanged target");

        // No backup is stashed for an Unchanged target.
        assert!(
            !crate::fsx::entry_present(&mirror_backup_path(&d.backups, ts, &target)),
            "the fixture must have no backup for the Unchanged target"
        );

        write_plan(
            &d.journal,
            ts,
            &Plan::new(vec![PlannedOperation::copy(
                "src",
                target.as_str(),
                Disposition::Unchanged,
            )]),
        );

        let report = recover_orphans(&d.journal, &d.backups).expect("recover");
        assert!(
            report.recovered_any(),
            "the orphan plan is still consumed and cleaned up"
        );
        assert!(
            target.exists(),
            "the Unchanged target must still exist after recovery"
        );
        assert_eq!(
            fs_err::read(&target).expect("read preserved target"),
            b"matches-the-source",
            "the Unchanged target must be byte-for-byte unchanged"
        );
    }

    #[test]
    fn create_marked_orphan_target_with_no_backup_is_deleted() {
        // An orphan plan whose target is marked `Create` with no backup is
        // a fresh creation the crashed apply made, so recovery removes it.
        // This delete behavior already existed; this test re-confirms it
        // still holds now that the `Unchanged` arm runs first.
        let d = dirs();
        let root = d.journal.parent().expect("journal has a parent");
        let ts = "20260528T140000Z";

        let target = root.join("home").join(".vimrc");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir home");
        fs_err::write(&target, b"freshly-created").expect("write create target");

        assert!(
            !crate::fsx::entry_present(&mirror_backup_path(&d.backups, ts, &target)),
            "the fixture must have no backup for the Create target"
        );

        write_plan(
            &d.journal,
            ts,
            &Plan::new(vec![PlannedOperation::copy(
                "src",
                target.as_str(),
                Disposition::Create,
            )]),
        );

        let report = recover_orphans(&d.journal, &d.backups).expect("recover");
        assert!(report.recovered_any(), "the orphan must be recovered");
        assert!(
            !target.exists(),
            "the Create target with no backup must be deleted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_of_a_pre_existing_symlink_restores_the_symlink() {
        // C1 regression at the recovery layer: an orphan apply overwrote a
        // pre-existing *symlink* target. Recovery must restore the
        // symlink, not leave a regular file holding the destination's
        // bytes. The backup is a symlink, the kind `backup_before_overwrite`
        // now stashes. Its destination does not need to exist for the
        // restore to find and recreate the link.
        let d = dirs();
        let root = d.journal.parent().expect("journal has a parent");
        let ts = "20260528T120000Z";

        let target = root.join("home").join(".zshrc");
        fs_err::create_dir_all(target.parent().expect("target parent")).expect("mkdir home");

        let backup = mirror_backup_path(&d.backups, ts, &target);
        fs_err::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup tree");
        fs_err::os::unix::fs::symlink("/orig/dest", &backup).expect("stash original as symlink");

        // The crashed apply left a fresh regular file where the link was.
        fs_err::write(&target, b"new-content").expect("write orphan target");

        write_plan(
            &d.journal,
            ts,
            &Plan::new(vec![PlannedOperation::copy(
                "src",
                target.as_str(),
                crate::journal::Disposition::Create,
            )]),
        );

        let report = recover_orphans(&d.journal, &d.backups).expect("recover");
        assert!(report.recovered_any(), "the orphan must be recovered");

        let meta = fs_err::symlink_metadata(&target).expect("stat restored target");
        assert!(
            meta.file_type().is_symlink(),
            "the pre-existing symlink must be restored as a symlink, not a regular file"
        );
        assert_eq!(
            fs_err::read_link(&target).expect("readlink restored target"),
            std::path::Path::new("/orig/dest")
        );
    }
}
