//! `patina status`: classify every managed target as CLEAN / DRIFTED /
//! MISSING / ORPHANED against the last committed apply (REQ-018, T-017).
//!
//! Status is the read-only mirror of apply. It reads the most recent
//! committed apply record from the journal (the `<ts>.COMMIT` sentinel
//! T-017 taught the commit path to populate), recomputes the *current*
//! repository plan to know which targets are still managed, and compares
//! each recorded target to the live filesystem.
//!
//! ## States (REQ-018 `<done-when>`)
//!
//! - **CLEAN** — target exists and matches the recorded expectation.
//! - **DRIFTED** — target exists but content / link target differs.
//! - **MISSING** — target was applied but no longer exists on disk.
//! - **ORPHANED** — target exists but the *current* plan no longer manages it
//!   (it was in a prior apply, then removed from the repo).
//!
//! ## Multi-target counting (REQ-005 / REQ-018)
//!
//! A `[[file]]` entry with N targets contributes N entries to the report
//! and N to the aggregate counters. The recorded apply already holds one
//! [`ExpectedTarget`](crate::journal::ExpectedTarget) per materialized
//! object, so the per-target shape falls out for free.
//!
//! ## Locking (REQ-023)
//!
//! Status takes the **shared** advisory lock so it never reads a journal a
//! concurrent apply is mid-write on. The shared wait is capped at
//! [`SHARED_TIMEOUT`]; on expiry status warns and proceeds without the
//! lock (REQ-023's read-only escape hatch). The warning text is returned
//! in [`StatusReport::warnings`] so the CLI can route it to stderr.

mod classify;

use crate::error::EngineError;
use crate::journal::LastApply;
use crate::journal::read_latest_commit;
use crate::lock::LockError;
use crate::lock::LockKind;
use crate::lock::SHARED_TIMEOUT;
use crate::lock::acquire as acquire_lock;
use crate::state_dir::resolve as resolve_state_dir;
use camino::Utf8PathBuf;
pub use classify::TargetState;
pub use classify::classify;
use std::collections::BTreeSet;

/// One classified target in a [`StatusReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Canonical absolute target path.
    pub path: Utf8PathBuf,
    /// The target's classification against the last apply.
    pub state: TargetState,
}

/// The full result of a `patina status` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusReport {
    /// Metadata about the last committed apply, or `None` when no apply
    /// has ever committed (a fresh state directory).
    pub last_apply: Option<LastApply>,
    /// One entry per classified target, in recorded apply order.
    pub files: Vec<StatusEntry>,
    /// Count of [`TargetState::Clean`] entries.
    pub clean: usize,
    /// Count of [`TargetState::Drifted`] entries.
    pub drifted: usize,
    /// Count of [`TargetState::Missing`] entries.
    pub missing: usize,
    /// Count of [`TargetState::Orphaned`] entries.
    pub orphaned: usize,
    /// Human-readable warnings (e.g. the lock-timeout escape hatch). The
    /// CLI routes these to stderr.
    pub warnings: Vec<String>,
}

impl StatusReport {
    /// Record one classified entry, bumping the matching counter.
    fn push(&mut self, path: Utf8PathBuf, state: TargetState) {
        match state {
            TargetState::Clean => self.clean += 1,
            TargetState::Drifted => self.drifted += 1,
            TargetState::Missing => self.missing += 1,
            TargetState::Orphaned => self.orphaned += 1,
        }
        self.files.push(StatusEntry { path, state });
    }
}

/// Compute the status report for the resolved dotfiles repository.
///
/// Resolves the repository and state directory, takes the shared lock
/// (warning and proceeding on timeout per REQ-023), reads the last
/// committed apply, recomputes the current plan to know which targets are
/// still managed, and classifies each recorded target.
///
/// # Errors
///
/// Returns an [`EngineError`] when repository / state-directory resolution,
/// the current-plan computation, or the journal read fails. A shared-lock
/// *timeout* is not an error: it is downgraded to a warning and the read
/// proceeds.
pub fn report(current_plan_targets: &BTreeSet<Utf8PathBuf>) -> Result<StatusReport, EngineError> {
    let state_dir = resolve_state_dir()?;
    let journal_dir = state_dir.join("journal");
    let lock_path = state_dir.join("lock");

    let mut warnings = Vec::new();
    // Shared lock with the read-only escape hatch: a timeout means a
    // mutating apply held the lock past SHARED_TIMEOUT, so we warn and read
    // anyway rather than blocking the user (REQ-023).
    let _guard = match acquire_lock(&lock_path, LockKind::Shared, SHARED_TIMEOUT) {
        Ok(guard) => Some(guard),
        Err(LockError::Timeout { path, waited, .. }) => {
            warnings.push(format!(
                "could not acquire the shared lock on `{path}` within {waited:?}; \
                 proceeding with status without it"
            ));
            None
        }
        Err(other) => return Err(EngineError::Lock(other)),
    };

    let record = read_latest_commit(&journal_dir)?;
    let mut report = StatusReport {
        warnings,
        ..StatusReport::default()
    };
    let Some(record) = record else {
        // No apply has committed yet: an empty report (no last_apply) with
        // any accumulated warnings.
        return Ok(report);
    };

    report.last_apply = Some(record.last_apply);
    for expected in &record.targets {
        let path = Utf8PathBuf::from(expected.target());
        let still_managed = current_plan_targets.contains(&manage_key(&path));
        let state = classify(expected, still_managed);
        // A target the current plan dropped *and* that is already gone from
        // disk is fully done — nothing to surface (it would classify
        // Missing, but there is no managed target to be missing).
        if !still_managed && state == TargetState::Missing {
            continue;
        }
        report.push(path, state);
    }

    Ok(report)
}

/// Recompute the set of canonical target paths the *current* repository
/// plan manages. Used to distinguish a still-managed target (classified
/// against its content) from one the repo has since dropped (ORPHANED).
///
/// Delegates to [`crate::apply::engine::current_managed_targets`], the single
/// `when`-aware, `symlink-tree`-aware managed-set computation shared with the
/// apply-time orphan reap so status and apply agree on which targets are
/// still managed. That computation keys each target by its declared
/// **location** via [`manage_key`] rather than a full canonicalization: a
/// full canonicalization would follow an already-materialized symlink target
/// through to the repo source, so the target would never appear to be its
/// own managed location and every applied symlink would falsely report as
/// ORPHANED at status time. It drops `when`-false entries (so a flipped-off
/// entry's prior target classifies ORPHANED, CHK-019) and expands a
/// `symlink-tree` entry into one key per live source leaf (so a deleted
/// source leaf's prior target classifies ORPHANED, CHK-014).
///
/// # Errors
///
/// Returns an [`EngineError`] when repository discovery, profile resolution,
/// module enumeration, manifest parsing, or a `when` predicate evaluation
/// fails.
pub fn current_plan_targets() -> Result<BTreeSet<Utf8PathBuf>, EngineError> {
    crate::apply::engine::current_managed_targets()
}

/// Compute the cross-time comparison key for a target path.
///
/// The "still managed?" test must match a path recorded at apply time
/// against one re-derived at status time, but the two are canonicalized
/// under different filesystem conditions:
///
/// - On Windows the filesystem form carries a `\\?\` verbatim prefix the
///   apply-time lexical form may lack.
/// - A symlink target that did not exist at apply time exists at status time,
///   so a full canonicalization would *follow the link* and resolve to the repo
///   source instead of the link's own location.
///
/// The key sidesteps both by canonicalizing only the **parent** directory
/// (which exists at both times and is never the symlink itself) and
/// re-joining the final component verbatim, then stripping any verbatim
/// prefix. Applied symmetrically to recorded and current-plan paths, the
/// same declared target yields the same key regardless of when it was
/// computed.
///
/// Public so the SPEC-0002 `remove` / `promote` commands can match a
/// user-supplied target path against a journaled
/// [`ExpectedTarget::target`](crate::ExpectedTarget::target) under the same
/// cross-time key, rather than re-deriving the parent-canonical+verbatim-leaf
/// technique at the call site.
#[must_use = "the manage key is the cross-time comparison key for a target path"]
pub fn manage_key(path: &camino::Utf8Path) -> Utf8PathBuf {
    let parent_key = match path.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent
            .canonicalize_utf8()
            .map_or_else(|_| parent.to_owned(), |p| crate::paths::simplified(&p)),
        _ => return crate::paths::simplified(path),
    };
    match path.file_name() {
        Some(name) => parent_key.join(name),
        None => parent_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn push_increments_the_matching_counter() {
        let mut report = StatusReport::default();
        report.push(Utf8PathBuf::from("/a"), TargetState::Clean);
        report.push(Utf8PathBuf::from("/b"), TargetState::Clean);
        report.push(Utf8PathBuf::from("/c"), TargetState::Drifted);
        assert_eq!(report.clean, 2);
        assert_eq!(report.drifted, 1);
        assert_eq!(report.missing, 0);
        assert_eq!(report.files.len(), 3);
    }

    #[test]
    fn manage_key_is_stable_across_existence_for_a_real_dir() {
        let (_td, dir) = {
            let td = TempDir::new().expect("tempdir");
            let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf8 temp path");
            (td, path)
        };
        // The parent (dir) exists; the child does not. Both an absent and a
        // freshly-created child must produce the same key, so a target that
        // appears between apply and status stays "managed".
        let child = dir.join("child");
        let absent_key = manage_key(&child);
        fs_err::write(&child, b"x").expect("create child");
        let present_key = manage_key(&child);
        assert_eq!(absent_key, present_key);
    }
}
