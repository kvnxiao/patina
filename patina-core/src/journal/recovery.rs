//! Crash recovery: converge backward to the pre-apply state via a
//! filesystem probe (REQ-013, T-011).
//!
//! On every apply startup, before computing a fresh plan, the engine
//! calls [`recover_orphans`]. It scans the journal directory for *orphan*
//! plans — a `<ts>.plan` with neither a `<ts>.COMMIT` (the apply
//! committed; T-010) nor a `<ts>.ROLLED_BACK` (a prior rollback closed it
//! out; T-018) sentinel. An orphan is the fingerprint of a `kill -9`
//! mid-apply: the plan was made durable but the run never reached commit.
//!
//! For each orphan, recovery:
//!
//! 1. Decodes the plan, reusing T-010's version-envelope check so a plan from a
//!    newer binary is refused rather than mis-read.
//! 2. Probes the filesystem for each operation's target
//!    ([`probe`](super::probe)) and consults the per-apply backup directory
//!    ([`mirror_backup_path`](super::mirror_backup_path)).
//! 3. **Reverses backward** — never forward (DEC-011). A target with a backup
//!    is an *overwrite*: restore the original bytes from the backup. A target
//!    with no backup is a *fresh creation*: delete it. Either way the
//!    post-recovery state of that target matches pre-apply.
//! 4. Deletes the orphan `<ts>.plan` and `<ts>.progress` files once every
//!    operation has been reversed.
//!
//! Recovery is **idempotent**: the second run finds no orphan (the plan
//! file was removed by the first), so it is a no-op and yields the same
//! filesystem state. Within a single run it is also self-idempotent —
//! restoring a backup over an already-restored target rewrites identical
//! bytes, and deleting an already-absent fresh target is a no-op.
//!
//! The advisory progress cursor is **ignored** for the reversal decision:
//! recovery trusts the filesystem probe and the backup directory, not the
//! cursor's last record, which may lie about how far the apply got
//! (REQ-012, CHK probe-over-cursor scenario).
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
use super::JournalError;
use super::PLAN_SUFFIX;
use super::PROGRESS_SUFFIX;
use super::Plan;
use super::PlannedOperation;
use super::probe::mirror_backup_path;
use super::probe::operation_target;
use camino::Utf8Path;

/// Filename suffix for the rollback sentinel written by `patina rollback`
/// (T-018). Recovery treats a `<ts>.ROLLED_BACK` plan as already closed,
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
    #[must_use = "inspect the recovered timestamps to report or assert what recovery reversed"]
    pub fn recovered_timestamps(&self) -> &[String] {
        &self.recovered
    }

    /// Whether this pass found and recovered at least one orphan plan.
    #[must_use = "a true result means a prior partial apply was rolled back"]
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
        // No journal directory yet means no prior apply — nothing to do.
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
    // succeeds, so a crash mid-recovery leaves the orphan in place and the
    // next startup retries it (still idempotent — restores rewrite the
    // same bytes, deletes of absent targets are no-ops).
    super::remove_if_present(&plan_path)?;
    super::remove_if_present(&journal_dir.join(format!("{timestamp}{PROGRESS_SUFFIX}")))?;
    Ok(())
}

/// Reverse a single planned operation back to its pre-apply state.
///
/// The decision is driven by the backup directory, not the progress
/// cursor: a backup existing for the target means the apply was about to
/// (or did) overwrite a pre-existing entry, so the original is restored; no
/// backup means the target was created fresh, so it is deleted if present.
///
/// Both restore and delete go through the kind-preserving
/// [`crate::fsx`] helpers, so the original is recreated as the same kind it
/// was — a symlink as a symlink, a directory as a directory — and backup
/// presence is probed with [`crate::fsx::entry_present`] so a backed-up
/// symlink whose destination is gone is still seen (`exists` would follow
/// the dead link and wrongly delete the target).
fn reverse_operation(
    backups_dir: &Utf8Path,
    timestamp: &str,
    op: &PlannedOperation,
) -> Result<(), JournalError> {
    let target = Utf8Path::new(operation_target(op));
    let backup = mirror_backup_path(backups_dir, timestamp, target);

    if crate::fsx::entry_present(&backup) {
        // Overwrite case: restore the original entry the engine stashed
        // before mutating, replacing whatever is at the target now (a new
        // symlink, a half-written copy, or the already-restored original).
        crate::fsx::clone_entry(&backup, target).map_err(JournalError::Filesystem)
    } else {
        // Fresh-creation case: there was nothing to back up, so reversing
        // means removing the target the apply created. If the operation
        // never started, the target is already absent and this is a no-op.
        crate::fsx::remove_entry(target).map_err(JournalError::Filesystem)
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

    #[cfg(unix)]
    #[test]
    fn overwrite_of_a_pre_existing_symlink_restores_the_symlink() {
        // C1 regression at the recovery layer: an orphan apply that
        // overwrote a pre-existing *symlink* target must, on recovery,
        // restore the symlink — not leave a regular file holding the
        // destination's bytes. The backup is a symlink (what
        // `backup_before_overwrite` now stashes), and its destination need
        // not exist for the restore to find and recreate the link.
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
