//! Crash recovery: converge backward to the pre-apply state via a
//! filesystem probe.
//!
//! [`recover_orphans`] scans the journal directory for *orphan* plans: a
//! `<ts>.plan` with neither a `<ts>.COMMIT` (the apply committed) nor a
//! `<ts>.ROLLED_BACK` (a prior rollback closed it out) sentinel. An orphan
//! indicates a `kill -9` mid-apply: the plan was made durable but the run
//! never reached commit. [`orphan_plans`] lists the same orphans without
//! reversing them.
//!
//! The mutating commands (`apply` on a path that can write, `rollback`,
//! `remove`, and `promote`) recover under the exclusive lock before their first
//! write, so each works from the pre-apply state.
//!
//! For each orphan, recovery:
//!
//! 1. Decodes the plan, reusing the version-envelope check so a plan from a
//!    newer binary is refused rather than mis-read.
//! 2. Probes the per-apply backup directory for each operation's target
//!    ([`mirror_backup_path`](super::mirror_backup_path)). A backup is present
//!    only once it is complete, because the backup writer stages it in a
//!    `.partial.<pid>` sibling and renames it into place. Recovery removes a
//!    staged sibling left by a killed apply.
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
//!    - no backup and `Remove`: the apply backs up a reaped target before it
//!      removes it, so the removal never started. Leave the target in place.
//!
//!    Each outcome leaves the target in its pre-apply state. Before recovery
//!    overwrites or removes an entry at a target, it copies that entry to
//!    `<state>/recovered/<ts>.<n>/<op index>/<file name>`, so bytes written
//!    after the crash survive. A retry of a recovery that failed partway finds
//!    the copy an earlier pass made for the same `<ts>` and op index, and
//!    reports that copy instead of copying again when the live entry still
//!    matches it or the backup. Otherwise it copies into `<n>` one past the
//!    highest existing number, so no pass overwrites an earlier copy, and
//!    reports the earlier copy before the new one.
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
//! let report = recover_orphans(Utf8Path::new("/state/patina"))?;
//! println!("recovered {} orphan plan(s)", report.recovered_timestamps().len());
//! # Ok::<(), patina_core::journal::JournalError>(())
//! ```

use super::COMMIT_SUFFIX;
use super::Disposition;
use super::JournalError;
use super::PLAN_SUFFIX;
use super::Plan;
use super::PlannedOperation;
use super::probe::mirror_backup_path;
use super::probe::operation_target;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Directory under the state directory that holds the entries recovery copied
/// aside before overwriting or removing them.
pub const RECOVERED_DIR: &str = "recovered";

/// Filename suffix for the rollback sentinel written by `patina rollback`.
/// Recovery treats a `<ts>.ROLLED_BACK` plan as already closed,
/// exactly like a committed one, and never re-reverses it.
pub const ROLLED_BACK_SUFFIX: &str = ".ROLLED_BACK";

/// Summary of one [`recover_orphans`] pass: the timestamps of the orphan
/// plans that were reversed and cleaned up, in lexical (chronological)
/// order, and each target the pass restored or removed.
/// [`recovered_timestamps`](Self::recovered_timestamps) is empty when no
/// interrupted apply was pending.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    recovered: Vec<String>,
    changed: Vec<RecoveredTarget>,
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

    /// Every target this pass restored from a backup or removed, in plan
    /// order.
    pub fn changed_targets(&self) -> &[RecoveredTarget] {
        &self.changed
    }
}

/// One target a recovery pass restored from a backup or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTarget {
    target: Utf8PathBuf,
    kept: Vec<Utf8PathBuf>,
}

impl RecoveredTarget {
    /// The target path as the orphan plan recorded it.
    pub fn target(&self) -> &Utf8Path {
        &self.target
    }

    /// Where recovery copied the entries it found at the target, earliest
    /// first: the first copy any pass kept, then this pass's copy when the
    /// entry differed from it. Empty when the target was absent before
    /// recovery.
    pub fn kept(&self) -> &[Utf8PathBuf] {
        &self.kept
    }
}

/// Recover every orphan plan under the per-machine state directory
/// `state_dir`, reversing each backward to the pre-apply filesystem state using
/// the backups under `<state_dir>/backups`, then deleting the orphan plan and
/// progress files.
///
/// Call this under the exclusive lock, before reading the filesystem to plan
/// or roll back. Running it again with no intervening apply is a no-op
/// (idempotence).
///
/// # Errors
///
/// - [`JournalError::Filesystem`] if the journal directory cannot be read, or
///   copying a live entry aside, a backup restore, a target delete, or orphan
///   cleanup fails.
/// - [`JournalError::VersionMismatch`] / [`JournalError::Decode`] /
///   [`JournalError::Truncated`] if an orphan plan cannot be decoded.
pub fn recover_orphans(state_dir: impl AsRef<Utf8Path>) -> Result<RecoveryReport, JournalError> {
    let state_dir = state_dir.as_ref();
    let journal_dir = state_dir.join("journal");

    // Reverse orphans in chronological order so the report is
    // deterministic and any later-apply backup wins a same-target race in
    // a (pathological) multi-orphan state.
    let timestamps = orphan_plans(&journal_dir)?;

    let mut report = RecoveryReport::default();
    for timestamp in timestamps {
        reverse_orphan(state_dir, &timestamp, &mut report.changed)?;
        report.recovered.push(timestamp);
    }
    Ok(report)
}

/// Return the `<ts>` of every orphan plan in `journal_dir`, sorted: a plan file
/// with neither a `COMMIT` nor a `ROLLED_BACK` sentinel beside it.
///
/// Reads only. An apply still running also has a plan without a sentinel, so
/// only a caller holding the lock can tell an orphan from a live plan.
///
/// # Errors
///
/// Returns [`JournalError::Filesystem`] if the journal directory cannot be
/// read.
pub fn orphan_plans(journal_dir: impl AsRef<Utf8Path>) -> Result<Vec<String>, JournalError> {
    let journal_dir = journal_dir.as_ref();
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
    orphans.sort();
    Ok(orphans)
}

/// Reverse one orphan plan and delete its plan + progress files.
fn reverse_orphan(
    state_dir: &Utf8Path,
    timestamp: &str,
    changed: &mut Vec<RecoveredTarget>,
) -> Result<(), JournalError> {
    let journal_dir = state_dir.join("journal");
    let plan_path = journal_dir.join(format!("{timestamp}{PLAN_SUFFIX}"));
    let bytes = fs_err::read(&plan_path)?;
    let plan = Plan::decode(&bytes)?;

    let backups_dir = state_dir.join("backups");
    let mut keeper = Keeper::new(state_dir.join(RECOVERED_DIR), timestamp)?;
    for (index, op) in plan.operations().iter().enumerate() {
        if let Some(recovered) = reverse_operation(&backups_dir, &mut keeper, index, op)? {
            changed.push(recovered);
        }
    }

    // The plan and progress files are removed only after every reversal
    // succeeds. A crash mid-recovery leaves the orphan in place, and the
    // next startup retries it. This retry is still idempotent: restoring
    // a backup rewrites the same bytes, and deleting an absent target is
    // a no-op.
    super::remove_plan_and_progress(&journal_dir, timestamp)
}

/// Reverse a single planned operation back to its pre-apply state.
///
/// The disposition the plan recorded is authoritative at any crash point,
/// because the plan is fsync'd before any mutation. For a tree operation it is
/// the durable per-op aggregate, so a tree whose aggregate is `Unchanged` is
/// left whole.
///
/// The executor backs up a pre-existing target immediately before its write,
/// and a reaped target immediately before its removal. A missing backup
/// therefore separates a `Create` target, which recovery removes, from an
/// `Update` or [`Remove`](PlannedOperation::Remove) target the apply never
/// reached, which recovery leaves in place. A backup the apply was killed while
/// staging is not at the mirror path, so it counts as missing; recovery removes
/// the staged `.partial.<pid>` sibling.
///
/// Before either restores over or removes a live entry, the entry is copied
/// aside through `keeper`. Copy, restore, and delete go through the
/// kind-preserving [`crate::fsx`] helpers. The original is therefore recreated
/// as the same kind it was: a symlink as a symlink, a directory as a
/// directory. Backup presence is probed with [`crate::fsx::entry_present`], so
/// a backed-up symlink whose destination is gone is still seen. `exists` would
/// follow the dead link and miss the backup.
fn reverse_operation(
    backups_dir: &Utf8Path,
    keeper: &mut Keeper<'_>,
    index: usize,
    op: &PlannedOperation,
) -> Result<Option<RecoveredTarget>, JournalError> {
    let disposition = op.disposition();
    if disposition == Some(Disposition::Unchanged) {
        return Ok(None);
    }

    let target = Utf8Path::new(operation_target(op));
    let backup = mirror_backup_path(backups_dir, keeper.timestamp, target);
    crate::fsx::remove_partial_siblings(&backup).map_err(JournalError::Filesystem)?;

    let live = crate::fsx::entry_present(target);
    let restore = crate::fsx::entry_present(&backup);
    let remove = live && disposition == Some(Disposition::Create);
    if !restore && !remove {
        return Ok(keeper.earlier(index, target).map(|kept| RecoveredTarget {
            target: target.to_path_buf(),
            kept: vec![kept],
        }));
    }
    let kept = if live {
        keeper.keep(index, target, restore.then_some(backup.as_path()))?
    } else {
        keeper.earlier(index, target).into_iter().collect()
    };
    if restore {
        crate::fsx::clone_entry(&backup, target)?;
    } else {
        crate::fsx::remove_entry(target)?;
    }
    Ok(Some(RecoveredTarget {
        target: target.to_path_buf(),
        kept,
    }))
}

/// Copies live entries aside under `<root>/<timestamp>.<n>/`.
struct Keeper<'a> {
    root: Utf8PathBuf,
    timestamp: &'a str,
    /// The `<timestamp>.<n>` directories earlier passes made, by ascending
    /// `<n>`.
    earlier: Vec<Utf8PathBuf>,
    next: u64,
    dir: Option<Utf8PathBuf>,
}

impl<'a> Keeper<'a> {
    fn new(root: Utf8PathBuf, timestamp: &'a str) -> Result<Self, JournalError> {
        let numbers = copy_numbers(&root, timestamp)?;
        let next = numbers.last().map_or(1, |last| last.saturating_add(1));
        let earlier = numbers
            .into_iter()
            .map(|number| root.join(format!("{timestamp}.{number}")))
            .collect();
        Ok(Self {
            root,
            timestamp,
            earlier,
            next,
            dir: None,
        })
    }

    /// The first copy an earlier pass kept of `target` for op `index`.
    fn earlier(&self, index: usize, target: &Utf8Path) -> Option<Utf8PathBuf> {
        self.earlier
            .iter()
            .map(|dir| copy_path(dir, index, target))
            .find(|kept| crate::fsx::entry_present(kept))
    }

    /// Copy the live entry at `target` aside unless it matches an earlier
    /// pass's copy or `backup`, and return the earlier copy, if any, before
    /// the new one.
    fn keep(
        &mut self,
        index: usize,
        target: &Utf8Path,
        backup: Option<&Utf8Path>,
    ) -> Result<Vec<Utf8PathBuf>, JournalError> {
        let mut copies: Vec<Utf8PathBuf> = self.earlier(index, target).into_iter().collect();
        if let Some(earlier) = copies.first() {
            let unchanged = crate::fsx::same_entry(target, earlier)?
                || match backup {
                    Some(backup) => crate::fsx::same_entry(target, backup)?,
                    None => false,
                };
            if unchanged {
                return Ok(copies);
            }
        }
        let dir = self
            .dir
            .get_or_insert_with(|| self.root.join(format!("{}.{}", self.timestamp, self.next)));
        let kept = copy_path(dir, index, target);
        crate::fsx::clone_entry(target, &kept)?;
        copies.push(kept);
        Ok(copies)
    }
}

/// The `<n>` of every `<timestamp>.<n>` entry under `root`, ascending.
fn copy_numbers(root: &Utf8Path, timestamp: &str) -> std::io::Result<Vec<u64>> {
    if !crate::fsx::entry_present(root) {
        return Ok(Vec::new());
    }
    let prefix = format!("{timestamp}.");
    let mut numbers = Vec::new();
    for entry in fs_err::read_dir(root)? {
        let name = entry?.file_name();
        let number = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|number| number.parse::<u64>().ok());
        numbers.extend(number);
    }
    numbers.sort_unstable();
    Ok(numbers)
}

fn copy_path(dir: &Utf8Path, index: usize, target: &Utf8Path) -> Utf8PathBuf {
    dir.join(index.to_string())
        .join(target.file_name().unwrap_or("entry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    struct Dirs {
        _temp: TempDir,
        root: Utf8PathBuf,
        journal: Utf8PathBuf,
        backups: Utf8PathBuf,
    }

    fn dirs() -> Dirs {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let journal = root.join("journal");
        let backups = root.join("backups");
        fs_err::create_dir_all(&journal).expect("create journal dir");
        fs_err::create_dir_all(&backups).expect("create backups dir");
        Dirs {
            _temp: temp,
            root,
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
        let report =
            recover_orphans(root.join("nope")).expect("recovery on a missing journal dir succeeds");
        assert!(!report.recovered_any());
    }

    #[test]
    fn committed_plan_is_not_an_orphan() {
        let d = dirs();
        let ts = "20260528T100000Z";
        write_plan(&d.journal, ts, &Plan::new(vec![]));
        fs_err::write(d.journal.join(format!("{ts}{COMMIT_SUFFIX}")), []).expect("commit sentinel");

        let report = recover_orphans(&d.root).expect("recovery");
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

        let report = recover_orphans(&d.root).expect("recovery");
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

        let report = recover_orphans(&d.root).expect("recover");
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

        let report = recover_orphans(&d.root).expect("recover");
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

        let report = recover_orphans(&d.root).expect("recover");
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
