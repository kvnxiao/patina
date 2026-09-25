use super::Disposition;
use super::ExpectedTarget;
use super::Journal;
use super::OsSyncer;
use super::Plan;
use super::PlannedOperation;
use crate::EngineError;
use camino::Utf8Path;

/// Remove one declaration and preserve or purge its target under the exclusive
/// lock.
#[derive(Debug)]
pub struct Removal<'a> {
    /// Per-machine state directory.
    pub state: &'a Utf8Path,
    /// Absolute target path.
    pub target: &'a Utf8Path,
    /// Replacement bytes, or `None` to delete the target.
    pub content: Option<&'a [u8]>,
    /// Manifest declaring the target.
    pub manifest: &'a Utf8Path,
    /// Manifest bytes after removing the declaration.
    pub edited_manifest: &'a [u8],
    /// Managed targets remaining after removal.
    pub remaining: Vec<ExpectedTarget>,
}

impl Removal<'_> {
    /// Journal both writes before mutating and publish the checkpoint last.
    ///
    /// # Errors
    ///
    /// Return an error if:
    /// - The plan cannot be journaled.
    /// - A backup fails.
    /// - A target or manifest write fails.
    /// - Publishing the checkpoint fails.
    ///
    /// An uncommitted plan remains recoverable through `recover_orphans`.
    pub fn execute(self) -> Result<(), EngineError> {
        let id = super::next_operation_id(self.state)?;
        let disposition = if crate::fsx::entry_present(self.target) {
            Disposition::Update
        } else {
            Disposition::Create
        };
        let plan = Plan::removal(
            self.target.as_str(),
            vec![
                PlannedOperation::copy(self.target.as_str(), self.target.as_str(), disposition),
                PlannedOperation::copy(
                    self.manifest.as_str(),
                    self.manifest.as_str(),
                    Disposition::Update,
                ),
            ],
        );
        let journal =
            Journal::flush_plan_and_fsync(self.state.join("journal"), &id, &plan, &OsSyncer)?;
        let backups = self.state.join("backups");
        for path in [self.target, self.manifest] {
            crate::backups::backup_before_overwrite(&backups, &id, path)?;
        }
        crash_after(0);
        super::remove_if_present(self.target)?;
        if let Some(bytes) = self.content {
            crate::fsx::write_atomic(self.target, bytes).map_err(super::JournalError::from)?;
        }
        crash_after(1);
        crate::fsx::write_atomic(self.manifest, self.edited_manifest)
            .map_err(super::JournalError::from)?;
        crash_after(2);
        journal.commit(&super::checkpoint_record(self.remaining), &OsSyncer)?;
        crash_after(3);
        super::retain_history(self.state);
        Ok(())
    }
}

/// Check whether an interrupted removal may have already edited its manifest.
///
/// # Errors
///
/// Return an error if a pending plan cannot be read or decoded.
pub fn pending_removal(state: &Utf8Path, target: &Utf8Path) -> Result<bool, super::JournalError> {
    let journal = state.join("journal");
    for id in super::orphan_plans(&journal)? {
        let bytes = fs_err::read(journal.join(format!("{id}{}", super::PLAN_SUFFIX)))?;
        if Plan::decode(&bytes)?.removes(target.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg_attr(
    debug_assertions,
    expect(
        clippy::exit,
        reason = "the test seam simulates process termination without unwinding"
    )
)]
fn crash_after(step: u32) {
    #[cfg(debug_assertions)]
    if std::env::var("PATINA_TEST_ABORT_REMOVE_AFTER")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(step)
    {
        std::process::exit(99);
    }
    #[cfg(not(debug_assertions))]
    let _ = step;
}
