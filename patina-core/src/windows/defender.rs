//! Windows Defender path-exclusion derivation, diffing, validation, and the
//! per-machine ledger: the pure, cross-platform layer.
//!
//! An antivirus exclusion is a permanent blind spot. This feature previews
//! every exclusion, asks for consent before applying it, and records it so it
//! can be reversed. This module owns the parts that decide *which* exact
//! paths Patina would exclude and *how* a run reconciles the current
//! Defender state against that set.
//! All of it is IO-free and hostable on any platform, so the derivation and
//! diff logic is unit-testable on Linux CI with no real Defender in the loop.
//!
//! The Windows-only side lives at the bottom of this file behind
//! `#[cfg(windows)]`. It reads the live exclusion list through
//! `Get-MpPreference` (`HostDefenderProbe`), and launches the elevated helper
//! that performs the add/remove (`launch_defender_helper`). The split mirrors
//! how `windows::elevate` separates the read side from the launch side.
//!
//! # The exclusion set
//!
//! The desired set is `{ repo_root as Folder }` plus, for each managed
//! target the current plan materializes, **one** exclusion whose kind mirrors
//! the config: a [`ExclusionKind::Folder`] for a directory mode
//! (`symlink` / `symlink-tree` / `copy` on a `[[directory]]`) and a
//! [`ExclusionKind::File`] for a file mode (`symlink` / `copy` / template on a
//! `[[file]]`). A `symlink-tree` of forty files contributes the **one**
//! declared target directory, never forty leaf entries. See
//! [`derive_exclusions`], which walks `resolved.operations` directly rather
//! than [`crate::apply::engine::current_managed_targets`] (that helper expands
//! trees to per-leaf keys, the opposite of the 1:1 decision here).
//!
//! # The normalized key
//!
//! `Get-MpPreference` may echo an excluded path back with different casing or a
//! trailing separator than Patina wrote. If the diff compared raw strings,
//! every re-run would see spurious add/remove churn and the
//! deterministic-stdout contract would break. [`Exclusion`]'s `Eq` / `Ord` /
//! `Hash` therefore key on a normalized form: case-folded, separators unified,
//! trailing separator stripped. The original casing is preserved for display
//! and for the add/remove call. Re-run idempotency depends on this
//! normalization.
//!
//! # Who can see the live exclusion list
//!
//! Only an elevated process can. `Get-MpPreference` does not fail for an
//! unelevated caller. It exits `0` and returns the string
//! `"N/A: Must be an administrator to view exclusions"` in place of
//! `ExclusionPath`. Treating that as a one-element path list makes every
//! desired exclusion look absent, and every verification look failed. The read
//! is therefore modelled as [`CurrentExclusions`], which distinguishes a real
//! list from a withheld one.
//!
//! Two consequences shape the rest of the feature:
//!
//! - **The unprivileged diff falls back to the ledger.** With the live list
//!   withheld, [`DefenderLedger`] (Patina's own record of what it applied) is
//!   the only available stand-in for what is present. It is weaker than the
//!   live list, because an exclusion deleted by hand in the Defender UI goes
//!   unnoticed. It does keep an unchanged re-run a no-op, which a
//!   `desired`-is-everything fallback would not.
//! - **Verification happens in the elevated helper, not here.** The helper is
//!   the only party that can re-read the list, so it writes its verdict to a
//!   result file ([`defender_result_path`]) and the Windows-only
//!   `launch_defender_helper` polls for it. A verification attempted from the
//!   unprivileged CLI can only ever conclude "not applied".

use crate::apply::engine::ResolvedPlan;
use crate::config::FileMode;
use crate::windows::is_unc_path;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::Hash;
use std::hash::Hasher;
use tracing::warn;

/// The basename of the per-machine ledger recording Patina-owned exclusions,
/// under the resolved state directory.
const LEDGER_FILENAME: &str = "defender.json";

/// The basename of the request file the unprivileged CLI writes and the
/// elevated helper reads, under the resolved state directory.
const REQUEST_FILENAME: &str = "defender-request.txt";

/// The basename of the result file the elevated helper writes and the
/// unprivileged CLI polls for, under the resolved state directory.
///
/// Duplicated verbatim in `patina-elevate`, which cannot depend on
/// `patina-core`, so the two sides of the receipt protocol agree by hand. Keep
/// them in sync along with the verdict tokens below.
const RESULT_FILENAME: &str = "defender-result.txt";

/// The request-line prefix (character plus one space) marking a path to add.
const REQUEST_ADD_PREFIX: &str = "A ";

/// The request-line prefix (character plus one space) marking a path to remove.
const REQUEST_REMOVE_PREFIX: &str = "R ";

/// Receipt verdict: the helper applied the request and its elevated re-read
/// confirmed the change took.
const RECEIPT_APPLIED: &str = "applied";

/// Receipt verdict: Defender accepted the call but the helper's re-read shows
/// the change did not take.
const RECEIPT_BLOCKED: &str = "blocked";

/// Receipt verdict: the helper could not apply the request at all.
const RECEIPT_FAILED: &str = "failed";

/// Environment variables naming system directories that must never be excluded.
///
/// Read at validation time (empty on non-Windows, where the variables are
/// unset, so the structural checks still run and the denylist simply matches
/// nothing). The `%SystemDrive%` root is covered by the separate drive-root
/// rejection rather than listed here. Duplicated verbatim in
/// `patina-elevate`'s independent validator: the helper cannot depend on
/// `patina-core`, so the trust boundary is re-enforced there with its own copy.
const SYSTEM_DIR_ENV_VARS: [&str; 4] = [
    "SystemRoot",
    "ProgramFiles",
    "ProgramW6432",
    "ProgramFiles(x86)",
];

/// Whether an excluded path names a single file or a whole folder.
///
/// Defender's `-ExclusionPath` API does not itself distinguish the two; a path
/// exclusion covers whatever lives at that path. The kind is carried purely
/// to render an honest preview (the user sees *file* vs *folder* before
/// consenting) and to round-trip through the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExclusionKind {
    /// The excluded path is a single file.
    File,
    /// The excluded path is a directory.
    Folder,
}

impl ExclusionKind {
    /// A stable lowercase label for preview output.
    #[must_use = "the label names the exclusion kind in the preview"]
    pub fn label(self) -> &'static str {
        match self {
            ExclusionKind::File => "file",
            ExclusionKind::Folder => "folder",
        }
    }
}

/// One desired Defender path exclusion: the resolved absolute path plus the
/// kind that path represents.
///
/// Equality, ordering, and hashing use a **normalized key**: case-folded,
/// separators unified, trailing separator stripped. Two exclusions that differ
/// only in letter case or a trailing separator therefore compare equal and
/// collapse in a set. That is the guard that keeps re-runs from churning. The
/// stored `path` keeps its original casing for
/// display and for the eventual add/remove call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exclusion {
    /// The absolute path to exclude, in its original casing.
    pub path: Utf8PathBuf,
    /// Whether the path is a file or a folder.
    pub kind: ExclusionKind,
}

impl Exclusion {
    /// Construct an exclusion from a path and its kind.
    #[must_use = "construct the exclusion to place it in the desired set"]
    pub fn new(path: impl Into<Utf8PathBuf>, kind: ExclusionKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }

    /// The normalized comparison key for this exclusion's path.
    #[must_use = "the key is the identity used for set membership and ordering"]
    pub fn key(&self) -> String {
        normalized_key(&self.path)
    }
}

impl PartialEq for Exclusion {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Exclusion {}

impl Hash for Exclusion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl PartialOrd for Exclusion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Exclusion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

/// Normalize a path into the comparison key used for exclusion identity.
///
/// Forward slashes are unified to backslashes, trailing separators stripped,
/// and the result ASCII-lowercased. That is the folding Windows itself applies
/// when it matches an excluded path. It is also the exact set of differences
/// `Get-MpPreference` may introduce when it echoes a path back. ASCII
/// case-folding (not full Unicode) matches how the Windows filesystem compares
/// paths in practice.
#[must_use = "the normalized key is the identity used across the diff"]
fn normalized_key(path: &Utf8Path) -> String {
    let unified: String = path
        .as_str()
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();
    unified.trim_end_matches('\\').to_ascii_lowercase()
}

/// The Defender exclusion kind a resolved [`FileMode`] maps to.
///
/// Directory modes (`SymlinkDir` / `SymlinkTree` / `CopyTree`) exclude a
/// folder; file modes (`Symlink` / `Copy` / `TemplateRender`) exclude a file.
/// The `match` is exhaustive with **no wildcard arm**, so adding a future
/// [`FileMode`] variant fails to compile here until its exclusion kind is
/// decided deliberately.
#[must_use = "the kind determines how the exclusion is previewed"]
pub fn exclusion_kind_for(mode: FileMode) -> ExclusionKind {
    match mode {
        FileMode::SymlinkDir | FileMode::SymlinkTree | FileMode::CopyTree => ExclusionKind::Folder,
        FileMode::Symlink | FileMode::Copy | FileMode::TemplateRender => ExclusionKind::File,
    }
}

/// Derive the desired Defender exclusion set from a resolved apply plan.
///
/// The set is the repository root (always, as a [`ExclusionKind::Folder`]) plus
/// one exclusion per declared target of every planned operation, its kind taken
/// from the operation's mode via [`exclusion_kind_for`]. Because the walk is
/// over `resolved.operations`, gated entries contribute nothing and a
/// `symlink-tree` contributes exactly one folder exclusion.
/// `resolved.operations` already excludes `when`-false entries, and carries a
/// tree entry's single declared target directory rather than its expanded
/// leaves.
///
/// A candidate is the repo root or a target. One that fails
/// [`validate_exclusion_path`] is skipped with a warning, rather than aborting
/// the whole run. Such a candidate is a UNC path, a drive-relative path, or a
/// system directory.
#[must_use = "the desired set is the input to plan_defender"]
pub fn derive_exclusions(resolved: &ResolvedPlan) -> BTreeSet<Exclusion> {
    let mut desired = BTreeSet::new();

    match validate_exclusion_path(&resolved.repo_root) {
        Ok(()) => {
            desired.insert(Exclusion::new(
                resolved.repo_root.clone(),
                ExclusionKind::Folder,
            ));
        }
        Err(err) => warn!("skipping repository root as a Defender exclusion: {err}"),
    }

    for op in &resolved.operations {
        let kind = exclusion_kind_for(op.mode);
        for target in &op.targets {
            match validate_exclusion_path(target) {
                Ok(()) => {
                    desired.insert(Exclusion::new(target.clone(), kind));
                }
                Err(err) => warn!("skipping `{target}` as a Defender exclusion: {err}"),
            }
        }
    }

    desired
}

/// The live Defender exclusion list, as far as the reading process could see
/// it.
///
/// `Get-MpPreference` withholds the list from an unelevated caller without
/// failing, so "read it" and "got a list" are different outcomes and the type
/// keeps them apart. See the module's *Who can see the live exclusion list*
/// section for why this distinction drives the whole reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentExclusions {
    /// The list was read: exactly these paths are excluded.
    Known(BTreeSet<Utf8PathBuf>),
    /// Defender withheld the list from this process, so the live state is
    /// unknown here. Only an elevated reader gets [`Known`].
    ///
    /// [`Known`]: CurrentExclusions::Known
    Unreadable,
}

/// How one desired exclusion stands against the live Defender list and the
/// Patina-owned ledger.
///
/// The first three arise when the live list was read; the last two are all that
/// can be said when it was withheld and the ledger is the only evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionState {
    /// Excluded in Defender and recorded in the ledger: Patina owns it.
    Owned,
    /// Excluded in Defender but absent from the ledger: already covered, and
    /// not by Patina.
    ///
    /// It arises when the path was excluded by hand, or by a Patina run whose
    /// ledger write never landed. Worth surfacing because ownership decides
    /// reversibility: a reap reaches only what [`DefenderLedger`] records, so
    /// an unmanaged exclusion outlives it. A successful reconcile adopts
    /// the entry, since the ledger converges on the whole desired set.
    Unmanaged,
    /// Not excluded in Defender. A reconcile would add it.
    Absent,
    /// The live list was withheld; the ledger records this exclusion.
    Recorded,
    /// The live list was withheld; the ledger does not record it.
    Unrecorded,
}

impl ExclusionState {
    /// Whether a reconcile would send an add for this exclusion.
    ///
    /// True for the states that read as "not in Defender as far as this
    /// process can tell". Matching one of those states puts an entry in
    /// [`DefenderDiff::to_add`].
    #[must_use = "the verdict selects how the entry is rendered"]
    pub fn needs_add(self) -> bool {
        matches!(self, Self::Absent | Self::Unrecorded)
    }
}

/// Classifies desired exclusions against one reading of the live list and one
/// ledger.
///
/// Built once per run so the normalized-key sets are computed once rather than
/// rebuilt for every exclusion. [`plan_defender`] derives the whole diff from
/// it, so the listing and the preview cannot disagree about what is already in
/// place.
#[derive(Debug, Clone)]
pub struct ExclusionClassifier {
    /// Normalized keys Defender currently excludes, or `None` when the live
    /// list was withheld from the reading process.
    present: Option<BTreeSet<String>>,
    /// Normalized keys the Patina ledger records.
    recorded: BTreeSet<String>,
}

impl ExclusionClassifier {
    /// Prepare a classifier over a live-list reading and a ledger.
    #[must_use = "construct the classifier to classify with it"]
    pub fn new(current: &CurrentExclusions, ledger: &BTreeSet<Exclusion>) -> Self {
        let present = match current {
            CurrentExclusions::Known(paths) => {
                Some(paths.iter().map(|path| normalized_key(path)).collect())
            }
            CurrentExclusions::Unreadable => None,
        };
        Self {
            present,
            recorded: ledger.iter().map(Exclusion::key).collect(),
        }
    }

    /// Classify one desired exclusion.
    #[must_use = "the state is what the listing renders"]
    pub fn classify(&self, exclusion: &Exclusion) -> ExclusionState {
        let key = exclusion.key();
        let recorded = self.recorded.contains(&key);
        match &self.present {
            Some(present) if present.contains(&key) => {
                if recorded {
                    ExclusionState::Owned
                } else {
                    ExclusionState::Unmanaged
                }
            }
            Some(_) => ExclusionState::Absent,
            None if recorded => ExclusionState::Recorded,
            None => ExclusionState::Unrecorded,
        }
    }

    /// Whether the classifier saw Defender's live list, as opposed to falling
    /// back to the ledger.
    ///
    /// The renderers need this to label their verdicts honestly: an inference
    /// drawn from Patina's own record must never be presented as an
    /// observation of Defender.
    #[must_use = "the answer decides whether the output may claim to have seen Defender's list"]
    pub fn live_list_was_read(&self) -> bool {
        self.present.is_some()
    }
}

/// The reconciliation between the desired exclusion set, the live Defender
/// state, and the Patina-owned ledger.
///
/// [`plan_defender`] computes this; the preview renders it and the elevated
/// helper enacts it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefenderDiff {
    /// Desired exclusions not currently present: the helper adds these.
    pub to_add: Vec<Exclusion>,
    /// Patina-owned exclusions no longer desired but still present: the
    /// helper removes these. Never includes a user-added exclusion.
    pub to_remove: Vec<Exclusion>,
}

impl DefenderDiff {
    /// Whether the diff is a no-op: nothing to add and nothing to remove.
    #[must_use = "an empty diff means the run writes nothing"]
    pub fn is_empty(&self) -> bool {
        self.to_add.is_empty() && self.to_remove.is_empty()
    }
}

/// Reconcile the desired exclusions against the current Defender state and the
/// Patina-owned ledger.
///
/// With the live list in hand ([`CurrentExclusions::Known`]):
///
/// - `to_add` = the desired exclusions whose path is not already present.
/// - `to_remove` = the ledger entries (Patina-owned) that are no longer desired
///   **and** are actually still present. Anchoring removals to the ledger
///   guarantees a user-added exclusion is never touched: a path Patina did not
///   record is never a removal candidate. Anchoring them to `current` too keeps
///   the diff honest: an already-gone entry is not re-removed.
///
/// With the list withheld ([`CurrentExclusions::Unreadable`]) the ledger stands
/// in for what is present, so `to_add` = `desired - ledger` and `to_remove` =
/// `ledger - desired`. Removals stay ledger-anchored, so the never-touch-a-
/// user-added-exclusion guarantee holds either way. The `current` anchor is
/// lost, which only means a removal may be sent for a path already gone,
/// and `Remove-MpPreference` treats that as a no-op.
///
/// The identity case (desired equals the present set, ledger equals desired)
/// yields an empty diff under both readings. That is the idempotency
/// guarantee re-runs depend on. Deriving `to_add` from the ledger rather
/// than from `desired` outright preserves that guarantee when the list is
/// withheld. The alternative would re-propose every exclusion on every run
/// and raise a UAC prompt each time.
///
/// Both readings run through [`ExclusionClassifier`], which also backs the
/// listing. One implementation of "is this already excluded" means the
/// preview cannot propose an add for an entry the listing calls present.
#[must_use = "the diff must be previewed and enacted"]
pub fn plan_defender(
    desired: &BTreeSet<Exclusion>,
    current: &CurrentExclusions,
    ledger: &BTreeSet<Exclusion>,
) -> DefenderDiff {
    let classifier = ExclusionClassifier::new(current, ledger);

    DefenderDiff {
        to_add: desired
            .iter()
            .filter(|exclusion| classifier.classify(exclusion).needs_add())
            .cloned()
            .collect(),
        // A ledger entry classifies as present exactly when the live list shows
        // it. With the list withheld it always does, because the ledger is the
        // only evidence available. That reproduces the `current` anchor where it
        // exists and drops it where it does not, matching what the two readings
        // above call for.
        to_remove: ledger
            .iter()
            .filter(|exclusion| {
                !desired.contains(*exclusion) && !classifier.classify(exclusion).needs_add()
            })
            .cloned()
            .collect(),
    }
}

/// Failure modes of [`validate_exclusion_path`].
///
/// Every variant is a refusal to exclude a path, carrying enough context for
/// the CLI to name the offending path in a warning.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExclusionPathError {
    /// The path was empty.
    #[error("exclusion path is empty")]
    Empty,
    /// The path contained a `*` or `?` wildcard; only exact paths are allowed.
    #[error("exclusion path `{0}` contains a wildcard; only exact paths are allowed")]
    Wildcard(Utf8PathBuf),
    /// The path was a UNC path (`\\server\share\...`).
    #[error("exclusion path `{0}` is a UNC path; only local absolute paths are allowed")]
    Unc(Utf8PathBuf),
    /// The path was not a drive-letter-absolute Windows path (e.g. it was
    /// drive-relative like `\Users\x` or `C:relative`).
    #[error("exclusion path `{0}` is not an absolute Windows path")]
    NotAbsolute(Utf8PathBuf),
    /// The path was a bare drive root (e.g. `C:\`); excluding an entire drive
    /// is refused.
    #[error("exclusion path `{0}` is a drive root; refusing to exclude an entire drive")]
    DriveRoot(Utf8PathBuf),
    /// The path was inside a protected system directory.
    #[error("exclusion path `{path}` is inside the protected system directory `{dir}`")]
    SystemDir {
        /// The offending exclusion path.
        path: Utf8PathBuf,
        /// The protected directory it fell under.
        dir: Utf8PathBuf,
    },
}

/// Validate that a path is safe to hand to Defender as an exclusion.
///
/// The checks are purely **lexical**: a managed target may not exist yet, so
/// nothing here touches the filesystem. A path passes only when it is a
/// drive-letter-absolute Windows path, is not UNC, contains no wildcard, and
/// is not a bare drive root. It must also fall under no env-derived system
/// directory. Those are `%SystemRoot%`, `%ProgramFiles%`, `%ProgramW6432%`,
/// `%ProgramFiles(x86)%`, and any drive root as a stand-in for
/// `%SystemDrive%`.
///
/// This runs in the unprivileged CLI **and** is independently re-enforced in
/// the elevated helper (the actual trust boundary).
///
/// # Errors
///
/// Returns the [`ExclusionPathError`] naming the first rule the path violates.
pub fn validate_exclusion_path(path: &Utf8Path) -> Result<(), ExclusionPathError> {
    validate_exclusion_path_with(path, &system_dir_denylist())
}

/// The env-derived system-directory denylist for the running process.
///
/// Off Windows every variable is unset, so this is empty and only the
/// structural checks in [`validate_exclusion_path_with`] apply. That keeps
/// the structural rejections testable on Linux CI.
fn system_dir_denylist() -> Vec<Utf8PathBuf> {
    SYSTEM_DIR_ENV_VARS
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
        .map(Utf8PathBuf::from)
        .collect()
}

/// The lexical core of [`validate_exclusion_path`], taking the system-directory
/// denylist explicitly so the denylist rule is testable with injected
/// directories on any platform.
fn validate_exclusion_path_with(
    path: &Utf8Path,
    system_dirs: &[Utf8PathBuf],
) -> Result<(), ExclusionPathError> {
    let raw = path.as_str();
    if raw.is_empty() {
        return Err(ExclusionPathError::Empty);
    }
    if raw.contains('*') || raw.contains('?') {
        return Err(ExclusionPathError::Wildcard(path.to_owned()));
    }
    if is_unc_path(path) {
        return Err(ExclusionPathError::Unc(path.to_owned()));
    }
    if !is_windows_absolute(raw) {
        return Err(ExclusionPathError::NotAbsolute(path.to_owned()));
    }
    if is_drive_root(raw) {
        return Err(ExclusionPathError::DriveRoot(path.to_owned()));
    }
    for dir in system_dirs {
        if is_within(path, dir) {
            return Err(ExclusionPathError::SystemDir {
                path: path.to_owned(),
                dir: dir.to_owned(),
            });
        }
    }
    Ok(())
}

/// Whether `s` is a drive-letter-absolute Windows path (`X:\...` or `X:/...`).
///
/// A pure string check holds the same verdict on every platform, so Linux CI
/// can validate a Windows path string.
fn is_windows_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(&drive), Some(&b':'), Some(&sep)) = (bytes.first(), bytes.get(1), bytes.get(2))
    else {
        return false;
    };
    drive.is_ascii_alphabetic() && (sep == b'\\' || sep == b'/')
}

/// Whether `s` is a bare drive root: a drive letter and colon followed by only
/// separators (`C:`, `C:\`, `C:/`).
fn is_drive_root(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(&drive), Some(&b':')) = (bytes.first(), bytes.get(1)) else {
        return false;
    };
    drive.is_ascii_alphabetic()
        && bytes
            .get(2..)
            .unwrap_or_default()
            .iter()
            .all(|&b| b == b'\\' || b == b'/')
}

/// Whether `path` is equal to or nested under `dir`, comparing on the
/// normalized key so case and separator differences do not defeat the check.
fn is_within(path: &Utf8Path, dir: &Utf8Path) -> bool {
    let p = normalized_key(path);
    let d = normalized_key(dir);
    if d.is_empty() {
        return false;
    }
    p == d || p.starts_with(&format!("{d}\\"))
}

/// Parse the `ExclusionPath` JSON emitted by `Get-MpPreference` into the
/// current exclusion state.
///
/// `Get-MpPreference | Select ExclusionPath | ConvertTo-Json` (Windows
/// PowerShell 5.1, which has no `-AsArray`) collapses to one of three shapes,
/// all handled here:
///
/// - **empty / `null`**: no exclusions are configured, so the [`Known`] set is
///   empty.
/// - **a bare JSON string**: either exactly one exclusion, or Defender's "must
///   be an administrator" placeholder.
/// - **a JSON array of strings**: several exclusions.
///
/// A lone string that is not a drive-absolute Windows path is the withheld-list
/// placeholder, and yields [`Unreadable`]. The verdict keys on the *shape*
/// rather than the placeholder's wording because that message is localized, and
/// a build that changes it must not silently start reporting a bogus exclusion.
///
/// Inside a real (multi-element) list a non-absolute entry is dropped with a
/// warning instead: a hand-added exclusion Patina would never write is no
/// reason to condemn the whole read. Non-string elements and other scalar
/// shapes likewise contribute nothing; only malformed JSON is an error.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] when the input is not valid
/// JSON.
///
/// [`Known`]: CurrentExclusions::Known
/// [`Unreadable`]: CurrentExclusions::Unreadable
pub fn parse_current_exclusions(json: &str) -> Result<CurrentExclusions, serde_json::Error> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(CurrentExclusions::Known(BTreeSet::new()));
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)?;
    let mut paths = BTreeSet::new();
    match value {
        serde_json::Value::String(single) => {
            if !is_windows_absolute(&single) {
                warn!("Defender withheld the exclusion list: {single}");
                return Ok(CurrentExclusions::Unreadable);
            }
            paths.insert(Utf8PathBuf::from(single));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                let Some(path) = item.as_str() else { continue };
                if is_windows_absolute(path) {
                    paths.insert(Utf8PathBuf::from(path));
                } else {
                    warn!("ignoring non-absolute Defender exclusion `{path}`");
                }
            }
        }
        // `null`, a number, a bare object: no usable exclusion paths.
        _ => {}
    }
    Ok(CurrentExclusions::Known(paths))
}

/// The elevated helper's verdict, as recovered from the result file.
///
/// The helper is the only party that can re-read Defender's exclusion list, so
/// this is the authoritative outcome of an apply. The Windows-only
/// `launch_defender_helper` polls for it and maps it onto a `DefenderOutcome`.
/// Neither is linked: both live behind `#[cfg(windows)]`, so the link would not
/// resolve when the docs are built for another target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefenderReceipt {
    /// The request applied and the helper's elevated re-read confirmed it.
    Applied,
    /// Defender accepted the call but the helper's re-read shows the change did
    /// not take: a silently rejected write.
    Blocked {
        /// The helper's detail, naming the paths and the live Defender status.
        detail: String,
    },
    /// The helper could not apply the request at all.
    Failed {
        /// The helper's detail.
        detail: String,
    },
}

/// Parse the result file the elevated helper writes.
///
/// The body is one line: a verdict token, optionally followed by a space and a
/// single-line detail. Anything else, an empty file included, yields `None`.
/// The poll treats `None` as "no verdict yet" rather than as a failure, so a
/// half-written or unrecognized receipt simply keeps the launcher polling.
#[must_use = "the receipt is the helper's verdict and decides the outcome"]
pub fn parse_receipt(content: &str) -> Option<DefenderReceipt> {
    let line = content.lines().next()?.trim();
    let (verdict, detail) = line.split_once(' ').unwrap_or((line, ""));
    let detail = detail.trim().to_owned();
    match verdict {
        RECEIPT_APPLIED => Some(DefenderReceipt::Applied),
        RECEIPT_BLOCKED => Some(DefenderReceipt::Blocked { detail }),
        RECEIPT_FAILED => Some(DefenderReceipt::Failed { detail }),
        _ => None,
    }
}

/// The per-machine record of the exclusions **Patina** owns.
///
/// Written only by the unprivileged CLI, never by the elevated helper. It lets
/// a reconcile reap a stale Patina exclusion while leaving a user-added
/// exclusion untouched: a path absent from the ledger is never a removal
/// candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefenderLedger {
    /// The Patina-owned exclusions, stored sorted for a deterministic file.
    pub exclusions: Vec<Exclusion>,
}

impl DefenderLedger {
    /// Build a ledger from a desired exclusion set, sorted deterministically.
    #[must_use = "construct the ledger to persist it"]
    pub fn from_set(set: &BTreeSet<Exclusion>) -> Self {
        Self {
            exclusions: set.iter().cloned().collect(),
        }
    }

    /// The ledger's exclusions as a set keyed by the normalized path.
    #[must_use = "the set is the ledger side of the diff"]
    pub fn to_set(&self) -> BTreeSet<Exclusion> {
        self.exclusions.iter().cloned().collect()
    }
}

/// The absolute path of the Defender ledger under a resolved state directory.
#[must_use = "the returned path locates the ledger file"]
pub fn defender_ledger_path(state_dir: &Utf8Path) -> Utf8PathBuf {
    state_dir.join(LEDGER_FILENAME)
}

/// The absolute path of the Defender request file under a resolved state
/// directory.
#[must_use = "the returned path locates the request file"]
pub fn defender_request_path(state_dir: &Utf8Path) -> Utf8PathBuf {
    state_dir.join(REQUEST_FILENAME)
}

/// The absolute path of the Defender result file under a resolved state
/// directory.
///
/// The elevated helper derives the same path as a sibling of the request file
/// it was handed, rather than recomputing the state directory. A `runas` to a
/// different admin has a different `%LOCALAPPDATA%`.
#[must_use = "the returned path locates the helper's result file"]
pub fn defender_result_path(state_dir: &Utf8Path) -> Utf8PathBuf {
    state_dir.join(RESULT_FILENAME)
}

/// Serialize a diff into the request-file body the elevated helper consumes.
///
/// One line per operation: `A <path>` to add, `R <path>` to remove, with the
/// path written verbatim (it is read back as literal data, never interpreted as
/// code). The ordering is the diff's own and deterministic, so the request file
/// is byte-stable for an unchanged plan.
#[must_use = "the serialized request is written to the request file"]
pub fn serialize_request(diff: &DefenderDiff) -> String {
    let mut body = String::new();
    for exclusion in &diff.to_add {
        body.push_str(REQUEST_ADD_PREFIX);
        body.push_str(exclusion.path.as_str());
        body.push('\n');
    }
    for exclusion in &diff.to_remove {
        body.push_str(REQUEST_REMOVE_PREFIX);
        body.push_str(exclusion.path.as_str());
        body.push('\n');
    }
    body
}

/// Errors from reading the live Defender exclusion list.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DefenderError {
    /// Spawning the PowerShell process that runs `Get-MpPreference` failed.
    #[error("failed to run `{command}`: {source}")]
    Command {
        /// The command that could not be spawned.
        command: &'static str,
        /// The spawn error.
        #[source]
        source: std::io::Error,
    },
    /// The PowerShell process ran but exited non-zero (e.g. the Defender module
    /// is unavailable, or the read was restricted).
    #[error("`{command}` failed: {stderr}")]
    CommandFailed {
        /// The command that failed.
        command: &'static str,
        /// The trimmed stderr the process produced.
        stderr: String,
    },
    /// The `Get-MpPreference` JSON output could not be parsed.
    #[error("failed to parse Get-MpPreference output: {source}")]
    Parse {
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
}

/// The host reads that back the exclusion-diff decision, abstracted so the
/// reconcile logic is testable against a fake probe with no live Defender.
///
/// The production implementation (`HostDefenderProbe`, Windows-only) runs
/// `Get-MpPreference`; tests supply a fake returning a fixed set.
pub trait DefenderProbe {
    /// Read the paths Defender currently excludes, or report that the list was
    /// withheld from this process.
    ///
    /// # Errors
    ///
    /// Returns a [`DefenderError`] when the underlying read fails. A list
    /// withheld for lack of elevation is **not** an error. It is
    /// [`CurrentExclusions::Unreadable`], a normal outcome for the
    /// unprivileged CLI.
    fn read_exclusions(&self) -> Result<CurrentExclusions, DefenderError>;
}

#[cfg(windows)]
pub use host::DefenderOutcome;
#[cfg(windows)]
pub use host::HostDefenderProbe;
#[cfg(windows)]
pub use host::launch_defender_helper;

/// The Windows-only host layer: the live `Get-MpPreference` read and the
/// elevated-helper launch.
#[cfg(windows)]
mod host {
    use super::CurrentExclusions;
    use super::DefenderError;
    use super::DefenderProbe;
    use super::DefenderReceipt;
    use super::parse_current_exclusions;
    use super::parse_receipt;
    use crate::apply::resolve_on_path;
    use crate::windows::WindowsError;
    use crate::windows::is_elevated;
    use crate::windows::poll_until;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use std::process::Command;
    use std::time::Duration;
    use winsafe::co;

    /// The verb that asks the shell to launch a target elevated, raising the
    /// UAC consent dialog.
    const RUNAS_VERB: &str = "runas";

    /// The helper subcommand that applies the request file's add/remove set.
    const HELPER_SUBCOMMAND: &str = "apply-defender-exclusions";

    /// How long to wait for the helper's result file before giving up.
    ///
    /// The helper starts Windows PowerShell and runs `Add-MpPreference`,
    /// `Remove-MpPreference`, `Get-MpPreference`, and `Get-MpComputerStatus`; a
    /// cold shell start plus those cmdlets is seconds, not milliseconds. The
    /// bound is deliberately generous. Too short a bound reports
    /// "could not confirm" over an apply that in fact succeeded, the exact
    /// class of false negative this whole path exists to avoid.
    const RECEIPT_DEADLINE: Duration = Duration::from_mins(2);

    /// How often to look for the result file while waiting. Short enough that
    /// the common case adds no perceptible delay past the helper's own work.
    const RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

    /// The PowerShell script that reads the exclusion list back as a forced
    /// JSON array. Wrapping in `@(...)` keeps the single-exclusion case an
    /// array shape as far as PowerShell will allow; the parser handles the
    /// scalar collapse regardless.
    const READ_SCRIPT: &str =
        "@(Get-MpPreference | Select-Object -ExpandProperty ExclusionPath) | ConvertTo-Json";

    /// The production [`DefenderProbe`]: reads the live exclusion list via
    /// `Get-MpPreference` through Windows PowerShell.
    #[derive(Debug, Default, Clone, Copy)]
    #[non_exhaustive]
    pub struct HostDefenderProbe;

    impl DefenderProbe for HostDefenderProbe {
        fn read_exclusions(&self) -> Result<CurrentExclusions, DefenderError> {
            // Defender withholds the exclusion list from an unelevated caller,
            // so asking costs a PowerShell start only to be told nothing. The
            // process-token check settles this up front, using the definitive
            // signal rather than the wording of the placeholder Defender
            // would have returned.
            if !is_elevated() {
                return Ok(CurrentExclusions::Unreadable);
            }
            let output = powershell()
                .args(["-Command", READ_SCRIPT])
                .output()
                .map_err(|source| DefenderError::Command {
                    command: "powershell Get-MpPreference",
                    source,
                })?;
            if !output.status.success() {
                return Err(DefenderError::CommandFailed {
                    command: "powershell Get-MpPreference",
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            parse_current_exclusions(&stdout).map_err(|source| DefenderError::Parse { source })
        }
    }

    /// A `powershell.exe` invocation with profile and interactivity disabled
    /// and the execution policy bypassed for this call, resolved on `PATH`
    /// so a relocated shell is still found. Windows PowerShell 5.1 ships
    /// the `Defender` module; `pwsh` (7+) is deliberately not depended on.
    fn powershell() -> Command {
        let shell = resolve_on_path("powershell.exe")
            .map_or_else(|| "powershell.exe".to_owned(), Utf8PathBuf::into_string);
        let mut command = Command::new(shell);
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
        ]);
        command
    }

    /// How the one-time elevated add/remove settled.
    ///
    /// Every variant but [`Declined`] comes from the helper's own elevated
    /// verification, relayed through the result file. The unprivileged CLI
    /// cannot read Defender's exclusion list and so cannot judge this for
    /// itself. The three failure variants stay distinct because they call for
    /// different things from the user: [`Blocked`] means Defender refused the
    /// write, [`Failed`] means the helper never got that far, and
    /// [`Unconfirmed`] means nobody knows.
    ///
    /// [`Applied`]: DefenderOutcome::Applied
    /// [`Declined`]: DefenderOutcome::Declined
    /// [`Blocked`]: DefenderOutcome::Blocked
    /// [`Failed`]: DefenderOutcome::Failed
    /// [`Unconfirmed`]: DefenderOutcome::Unconfirmed
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DefenderOutcome {
        /// The helper ran and its elevated re-read confirms the add/remove
        /// took. The only variant that may update the ledger.
        Applied,
        /// The user dismissed the UAC consent dialog (`ERROR_CANCELLED`), so
        /// the helper never ran.
        Declined,
        /// Defender accepted the call but the helper's re-read shows the
        /// exclusions did not change as requested: the write was silently
        /// rejected (Tamper Protection / managed Defender).
        Blocked {
            /// The helper's detail, naming the paths and the live Defender
            /// status.
            detail: String,
        },
        /// The helper ran but could not apply the request: a path it refused,
        /// an unreadable request file, PowerShell unavailable.
        Failed {
            /// The helper's detail.
            detail: String,
        },
        /// The helper never reported a verdict before the deadline, so whether
        /// the exclusions changed is unknown. Distinct from [`Blocked`]
        /// precisely because claiming Defender refused a write nobody observed
        /// is the failure mode this path is built to avoid.
        ///
        /// [`Blocked`]: DefenderOutcome::Blocked
        Unconfirmed,
    }

    /// Launch the elevated helper to enact the request file, and relay the
    /// verdict it writes to `receipt_path`.
    ///
    /// The `request_path` is passed as a **quoted** `ShellExecuteEx` parameter
    /// (exclusion paths contain spaces). Verification is the helper's job,
    /// because it is the only party elevated enough to re-read the exclusion
    /// list. It re-reads and records the result, and this function waits for
    /// that record
    /// rather than judging for itself.
    ///
    /// Waiting means polling for the result file to appear;
    /// `windows::poll_until` carries why a launcher cannot simply wait on
    /// the process.
    ///
    /// # Errors
    ///
    /// Returns [`WindowsError`] when the running executable cannot be located,
    /// when a previous run's result file cannot be cleared, or when the
    /// `ShellExecuteEx` launch fails. A user who declines consent is reported
    /// as [`DefenderOutcome::Declined`], not as an error.
    pub fn launch_defender_helper(
        request_path: &Utf8Path,
        receipt_path: &Utf8Path,
    ) -> Result<DefenderOutcome, WindowsError> {
        let helper = crate::windows::elevate::helper_path()?;
        let parameters = format!("{HELPER_SUBCOMMAND} \"{request_path}\"");

        // A result file left by an earlier run would be picked up as this
        // run's verdict, including a stale `applied` for a run the user just
        // declined. Refuse to launch rather than risk relaying it.
        clear_stale_receipt(receipt_path)?;

        let info = winsafe::SHELLEXECUTEINFO {
            verb: Some(RUNAS_VERB),
            file: &helper,
            parameters: Some(&parameters),
            show: co::SW::HIDE,
            ..Default::default()
        };

        match winsafe::ShellExecuteEx(&info) {
            Ok(()) => Ok(await_receipt(receipt_path)),
            Err(err) if err == co::ERROR::CANCELLED => Ok(DefenderOutcome::Declined),
            Err(err) => Err(WindowsError::WinApi {
                call: "ShellExecuteEx",
                source: std::io::Error::other(err),
            }),
        }
    }

    /// Remove a previous run's result file, treating an absent one as done.
    fn clear_stale_receipt(receipt_path: &Utf8Path) -> Result<(), WindowsError> {
        crate::fsx::remove_entry(receipt_path).map_err(|source| WindowsError::StaleReceipt {
            path: receipt_path.to_owned(),
            source,
        })
    }

    /// Wait for the helper's result file and map its verdict onto an outcome.
    ///
    /// A file that is absent, unreadable, or not yet a recognizable verdict is
    /// "no answer yet", so the poll keeps waiting; only the deadline turns that
    /// into [`DefenderOutcome::Unconfirmed`].
    fn await_receipt(receipt_path: &Utf8Path) -> DefenderOutcome {
        let verdict = poll_until(RECEIPT_DEADLINE, RECEIPT_POLL_INTERVAL, || {
            let content = fs_err::read_to_string(receipt_path.as_std_path()).ok()?;
            parse_receipt(&content)
        });
        match verdict {
            Some(DefenderReceipt::Applied) => DefenderOutcome::Applied,
            Some(DefenderReceipt::Blocked { detail }) => DefenderOutcome::Blocked { detail },
            Some(DefenderReceipt::Failed { detail }) => DefenderOutcome::Failed { detail },
            None => DefenderOutcome::Unconfirmed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::engine::ResolvedOperation;
    use crate::apply::engine::TargetDisposition;
    use crate::journal::Disposition;
    use crate::journal::Plan;
    use crate::state_dir::HostOs;
    use crate::variables::Builtins;
    use crate::variables::Resolver;

    /// A Windows-style repo root used across the derivation fixtures. The pure
    /// layer never touches the filesystem, so the path need not exist.
    const REPO: &str = r"C:\Users\kevin\dotfiles";

    fn op(mode: FileMode, targets: &[&str]) -> ResolvedOperation {
        ResolvedOperation {
            mode,
            source: Utf8PathBuf::from(r"C:\Users\kevin\dotfiles\src"),
            targets: targets.iter().map(Utf8PathBuf::from).collect(),
            dispositions: targets
                .iter()
                .map(|_| TargetDisposition {
                    aggregate: Disposition::Create,
                    leaves: Vec::new(),
                })
                .collect(),
            entry_index: 0,
        }
    }

    fn plan(ops: Vec<ResolvedOperation>) -> ResolvedPlan {
        ResolvedPlan {
            repo_root: Utf8PathBuf::from(REPO),
            profile: String::new(),
            plan: Plan::new(Vec::new()),
            operations: ops,
            hooks: Vec::new(),
            state_dir: Utf8PathBuf::from(r"C:\Users\kevin\AppData\Local\patina"),
            host_os: HostOs::current(),
            timestamp: "fixed".to_owned(),
            resolver: Resolver::new(Builtins::current()),
            remote_names: Vec::new(),
            remote_pins: Some(Vec::new()),
        }
    }

    fn set(paths: &[(&str, ExclusionKind)]) -> BTreeSet<Exclusion> {
        paths
            .iter()
            .map(|(p, kind)| Exclusion::new(*p, *kind))
            .collect()
    }

    /// A successfully-read live exclusion list built from raw path strings.
    fn known(paths: &[&str]) -> CurrentExclusions {
        CurrentExclusions::Known(paths.iter().map(Utf8PathBuf::from).collect())
    }

    #[test]
    fn exclusion_kind_covers_every_file_mode() {
        // Exhaustive over all six FileMode variants: directory modes fold to a
        // Folder exclusion, file modes to a File exclusion. If a variant is
        // ever added, `exclusion_kind_for`'s wildcard-free match fails to
        // compile and this table is the place to extend.
        assert_eq!(exclusion_kind_for(FileMode::Symlink), ExclusionKind::File);
        assert_eq!(exclusion_kind_for(FileMode::Copy), ExclusionKind::File);
        assert_eq!(
            exclusion_kind_for(FileMode::TemplateRender),
            ExclusionKind::File
        );
        assert_eq!(
            exclusion_kind_for(FileMode::SymlinkDir),
            ExclusionKind::Folder
        );
        assert_eq!(
            exclusion_kind_for(FileMode::SymlinkTree),
            ExclusionKind::Folder
        );
        assert_eq!(
            exclusion_kind_for(FileMode::CopyTree),
            ExclusionKind::Folder
        );
    }

    #[test]
    fn derive_includes_repo_root_as_a_folder() {
        let desired = derive_exclusions(&plan(vec![]));
        assert_eq!(
            desired,
            set(&[(REPO, ExclusionKind::Folder)]),
            "the repo root must always be present as a Folder exclusion"
        );
    }

    #[test]
    fn derive_maps_each_mode_to_its_kind() {
        let desired = derive_exclusions(&plan(vec![
            op(FileMode::Symlink, &[r"C:\Users\kevin\.gitconfig"]),
            op(FileMode::Copy, &[r"C:\Users\kevin\.ssh\config"]),
            op(
                FileMode::TemplateRender,
                &[r"C:\Users\kevin\.config\starship.toml"],
            ),
            op(FileMode::SymlinkDir, &[r"C:\Users\kevin\.config\nvim"]),
            op(
                FileMode::CopyTree,
                &[r"C:\Users\kevin\AppData\Roaming\Code\User"],
            ),
        ]));
        assert_eq!(
            desired,
            set(&[
                (REPO, ExclusionKind::Folder),
                (r"C:\Users\kevin\.gitconfig", ExclusionKind::File),
                (r"C:\Users\kevin\.ssh\config", ExclusionKind::File),
                (r"C:\Users\kevin\.config\starship.toml", ExclusionKind::File),
                (r"C:\Users\kevin\.config\nvim", ExclusionKind::Folder),
                (
                    r"C:\Users\kevin\AppData\Roaming\Code\User",
                    ExclusionKind::Folder
                ),
            ])
        );
    }

    #[test]
    fn derive_tree_contributes_one_folder_not_leaves() {
        // A symlink-tree entry's ResolvedOperation carries the single declared
        // target directory (leaves live in its dispositions); derive must emit
        // exactly that one folder, never a per-leaf entry.
        let desired = derive_exclusions(&plan(vec![op(
            FileMode::SymlinkTree,
            &[r"C:\Users\kevin\.config\fish"],
        )]));
        assert_eq!(
            desired,
            set(&[
                (REPO, ExclusionKind::Folder),
                (r"C:\Users\kevin\.config\fish", ExclusionKind::Folder),
            ])
        );
    }

    #[test]
    fn derive_multi_target_entry_yields_one_exclusion_per_target() {
        let desired = derive_exclusions(&plan(vec![op(
            FileMode::Symlink,
            &[r"C:\Users\kevin\.bashrc", r"C:\Users\kevin\.bash_profile"],
        )]));
        assert_eq!(
            desired,
            set(&[
                (REPO, ExclusionKind::Folder),
                (r"C:\Users\kevin\.bashrc", ExclusionKind::File),
                (r"C:\Users\kevin\.bash_profile", ExclusionKind::File),
            ])
        );
    }

    #[test]
    fn derive_skips_a_unc_target_but_keeps_the_valid_ones() {
        // An invalid target (UNC here) is skipped with a warning, not fatal;
        // the repo root and the valid target still make the set.
        let desired = derive_exclusions(&plan(vec![op(
            FileMode::Symlink,
            &[r"\\server\share\.gitconfig", r"C:\Users\kevin\.zshrc"],
        )]));
        assert_eq!(
            desired,
            set(&[
                (REPO, ExclusionKind::Folder),
                (r"C:\Users\kevin\.zshrc", ExclusionKind::File),
            ])
        );
    }

    #[test]
    fn plan_add_is_desired_minus_current() {
        let desired = set(&[
            (REPO, ExclusionKind::Folder),
            (r"C:\Users\kevin\.gitconfig", ExclusionKind::File),
        ]);
        let diff = plan_defender(&desired, &known(&[REPO]), &BTreeSet::new());
        assert_eq!(
            diff.to_add,
            vec![Exclusion::new(
                r"C:\Users\kevin\.gitconfig",
                ExclusionKind::File
            )]
        );
        assert!(diff.to_remove.is_empty());
    }

    #[test]
    fn plan_remove_is_ledger_minus_desired_intersect_current() {
        // A previously-owned exclusion no longer desired and still present is
        // reaped.
        let desired = set(&[(REPO, ExclusionKind::Folder)]);
        let stale = Exclusion::new(r"C:\Users\kevin\.oldrc", ExclusionKind::File);
        let ledger = set(&[
            (REPO, ExclusionKind::Folder),
            (r"C:\Users\kevin\.oldrc", ExclusionKind::File),
        ]);
        let current = known(&[REPO, r"C:\Users\kevin\.oldrc"]);
        let diff = plan_defender(&desired, &current, &ledger);
        assert!(diff.to_add.is_empty());
        assert_eq!(diff.to_remove, vec![stale]);
    }

    #[test]
    fn plan_never_removes_a_path_outside_the_ledger() {
        // A user-added exclusion is present in Defender without being desired
        // or recorded in the ledger. It is never a removal candidate.
        let desired = set(&[(REPO, ExclusionKind::Folder)]);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let current = known(&[REPO, r"C:\Users\kevin\user-added"]);
        let diff = plan_defender(&desired, &current, &ledger);
        assert!(
            diff.is_empty(),
            "a user-added exclusion must never be reaped: {diff:?}"
        );
    }

    #[test]
    fn plan_identity_is_an_empty_diff() {
        // Desired == present == ledger ⇒ no writes. The idempotency guarantee.
        let desired = set(&[
            (REPO, ExclusionKind::Folder),
            (r"C:\Users\kevin\.gitconfig", ExclusionKind::File),
        ]);
        let current = known(&[REPO, r"C:\Users\kevin\.gitconfig"]);
        let diff = plan_defender(&desired, &current, &desired);
        assert!(diff.is_empty(), "identity must be a no-op: {diff:?}");
    }

    #[test]
    fn plan_case_and_trailing_separator_differences_are_no_diff() {
        // The key idempotency guard: Get-MpPreference echoing a path back with
        // different case or a trailing separator must produce no churn.
        let desired = set(&[(r"C:\Users\kevin\dotfiles", ExclusionKind::Folder)]);
        let current = known(&[r"c:\users\kevin\DOTFILES\"]);
        let ledger = desired.clone();
        let diff = plan_defender(&desired, &current, &ledger);
        assert!(
            diff.is_empty(),
            "case/trailing-separator differences must not produce a diff: {diff:?}"
        );
    }

    #[test]
    fn plan_unreadable_takes_presence_from_the_ledger() {
        // With the live list withheld, the ledger says what is already there:
        // a desired-but-unrecorded path is added, a recorded one is left alone.
        let desired = set(&[
            (REPO, ExclusionKind::Folder),
            (r"C:\Users\kevin\.gitconfig", ExclusionKind::File),
        ]);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let diff = plan_defender(&desired, &CurrentExclusions::Unreadable, &ledger);
        assert_eq!(
            diff.to_add,
            vec![Exclusion::new(
                r"C:\Users\kevin\.gitconfig",
                ExclusionKind::File
            )]
        );
        assert!(diff.to_remove.is_empty());
    }

    #[test]
    fn plan_unreadable_identity_is_an_empty_diff() {
        // The reason `to_add` is ledger-relative rather than all of `desired`:
        // an unchanged re-run must stay a no-op even unprivileged, or every
        // invocation would raise a UAC prompt for work already done.
        let desired = set(&[
            (REPO, ExclusionKind::Folder),
            (r"C:\Users\kevin\.gitconfig", ExclusionKind::File),
        ]);
        let diff = plan_defender(&desired, &CurrentExclusions::Unreadable, &desired);
        assert!(
            diff.is_empty(),
            "an unchanged plan must not churn when the live list is withheld: {diff:?}"
        );
    }

    #[test]
    fn plan_unreadable_reaps_a_stale_ledger_entry() {
        let desired = set(&[(REPO, ExclusionKind::Folder)]);
        let ledger = set(&[
            (REPO, ExclusionKind::Folder),
            (r"C:\Users\kevin\.oldrc", ExclusionKind::File),
        ]);
        let diff = plan_defender(&desired, &CurrentExclusions::Unreadable, &ledger);
        assert!(diff.to_add.is_empty());
        assert_eq!(
            diff.to_remove,
            vec![Exclusion::new(
                r"C:\Users\kevin\.oldrc",
                ExclusionKind::File
            )]
        );
    }

    #[test]
    fn plan_unreadable_still_never_removes_a_path_outside_the_ledger() {
        // Losing the `current` anchor must not widen removals: they stay
        // ledger-derived, so a user-added exclusion is still untouchable.
        let desired = set(&[(REPO, ExclusionKind::Folder)]);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let diff = plan_defender(&desired, &CurrentExclusions::Unreadable, &ledger);
        assert!(
            diff.is_empty(),
            "a path Patina never recorded must never be reaped: {diff:?}"
        );
    }

    #[test]
    fn validate_accepts_a_normal_absolute_not_yet_existing_path() {
        // A plain absolute Windows path that need not exist passes; validation
        // is purely lexical.
        validate_exclusion_path(Utf8Path::new(r"C:\Users\kevin\.gitconfig"))
            .expect("a normal absolute not-yet-existing path validates");
    }

    #[test]
    fn validate_rejects_unc_wildcard_relative_root_and_empty() {
        assert!(matches!(
            validate_exclusion_path(Utf8Path::new(r"\\server\share\x")),
            Err(ExclusionPathError::Unc(_))
        ));
        assert!(matches!(
            validate_exclusion_path(Utf8Path::new(r"C:\Users\*\x")),
            Err(ExclusionPathError::Wildcard(_))
        ));
        assert!(matches!(
            validate_exclusion_path(Utf8Path::new(r"\Users\x")),
            Err(ExclusionPathError::NotAbsolute(_))
        ));
        assert!(matches!(
            validate_exclusion_path(Utf8Path::new("C:relative")),
            Err(ExclusionPathError::NotAbsolute(_))
        ));
        assert!(matches!(
            validate_exclusion_path(Utf8Path::new(r"C:\")),
            Err(ExclusionPathError::DriveRoot(_))
        ));
        assert!(matches!(
            validate_exclusion_path(Utf8Path::new("")),
            Err(ExclusionPathError::Empty)
        ));
    }

    #[test]
    fn validate_rejects_a_path_under_an_injected_system_dir() {
        // The env-derived denylist rule, tested with an injected directory so it
        // runs on any platform (the process env is untouched).
        let system_dirs = vec![Utf8PathBuf::from(r"C:\Windows")];
        assert!(matches!(
            validate_exclusion_path_with(Utf8Path::new(r"C:\Windows\System32\x"), &system_dirs),
            Err(ExclusionPathError::SystemDir { .. })
        ));
        // Case-insensitively too.
        assert!(matches!(
            validate_exclusion_path_with(Utf8Path::new(r"c:\windows\system32"), &system_dirs),
            Err(ExclusionPathError::SystemDir { .. })
        ));
        // A sibling that merely shares a prefix is not "within".
        validate_exclusion_path_with(Utf8Path::new(r"C:\WindowsApps\x"), &system_dirs)
            .expect("a prefix that is not a path-component boundary must not match");
    }

    #[test]
    fn classify_splits_a_present_exclusion_by_whether_the_ledger_records_it() {
        // The distinction the Unmanaged state exists for: both entries are
        // excluded in Defender, and only one of them is Patina's.
        let ours = Exclusion::new(REPO, ExclusionKind::Folder);
        let theirs = Exclusion::new(r"C:\Users\kevin\.gitconfig", ExclusionKind::File);
        let current = known(&[REPO, r"C:\Users\kevin\.gitconfig"]);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let classifier = ExclusionClassifier::new(&current, &ledger);

        assert_eq!(classifier.classify(&ours), ExclusionState::Owned);
        assert_eq!(classifier.classify(&theirs), ExclusionState::Unmanaged);
    }

    #[test]
    fn classify_reports_absent_whether_or_not_the_ledger_records_it() {
        // A ledger entry Defender no longer excludes is still absent: the live
        // list wins when it is available.
        let gone = Exclusion::new(REPO, ExclusionKind::Folder);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let classifier = ExclusionClassifier::new(&known(&[]), &ledger);
        assert_eq!(classifier.classify(&gone), ExclusionState::Absent);

        let classifier = ExclusionClassifier::new(&known(&[]), &BTreeSet::new());
        assert_eq!(classifier.classify(&gone), ExclusionState::Absent);
    }

    #[test]
    fn classify_falls_back_to_the_ledger_when_the_list_is_withheld() {
        // Unmanaged is undetectable here: without the live list there is nothing
        // to notice an unrecorded-but-present exclusion against.
        let recorded = Exclusion::new(REPO, ExclusionKind::Folder);
        let other = Exclusion::new(r"C:\Users\kevin\.gitconfig", ExclusionKind::File);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let classifier = ExclusionClassifier::new(&CurrentExclusions::Unreadable, &ledger);

        assert_eq!(classifier.classify(&recorded), ExclusionState::Recorded);
        assert_eq!(classifier.classify(&other), ExclusionState::Unrecorded);
    }

    #[test]
    fn classify_matches_a_present_path_across_case_and_trailing_separator() {
        // Classification keys on the normalized path, like the diff, so
        // `Get-MpPreference` echoing a path back differently cannot demote an
        // owned exclusion to Absent.
        let ours = Exclusion::new(r"C:\Users\kevin\dotfiles", ExclusionKind::Folder);
        let ledger = set(&[(r"C:\Users\kevin\dotfiles", ExclusionKind::Folder)]);
        let classifier = ExclusionClassifier::new(&known(&[r"c:\users\kevin\DOTFILES\"]), &ledger);
        assert_eq!(classifier.classify(&ours), ExclusionState::Owned);
    }

    #[test]
    fn parse_handles_zero_one_and_many_shapes() {
        // null / empty ⇒ a known-empty list, not an unreadable one: Defender
        // answered, it just has nothing configured.
        assert_eq!(
            parse_current_exclusions("null").expect("null parses"),
            known(&[])
        );
        assert_eq!(
            parse_current_exclusions("   ").expect("blank parses"),
            known(&[])
        );
        // A bare string ⇒ one (the PowerShell 5.1 single-element collapse).
        assert_eq!(
            parse_current_exclusions("\"C:\\\\Users\\\\kevin\\\\dotfiles\"")
                .expect("scalar parses"),
            known(&[r"C:\Users\kevin\dotfiles"])
        );
        // An array ⇒ many.
        assert_eq!(
            parse_current_exclusions("[\"C:\\\\a\", \"C:\\\\b\"]").expect("array parses"),
            known(&[r"C:\a", r"C:\b"])
        );
    }

    #[test]
    fn parse_reports_the_withheld_list_as_unreadable() {
        // The regression this type exists for. Read unprivileged,
        // `Get-MpPreference` exits 0 and returns this placeholder in place of
        // the list; taking it for an exclusion path made every desired path
        // look absent and every verification look failed.
        assert_eq!(
            parse_current_exclusions("\"N/A: Must be an administrator to view exclusions\"")
                .expect("the placeholder parses as JSON"),
            CurrentExclusions::Unreadable
        );
    }

    #[test]
    fn parse_treats_any_lone_non_path_string_as_unreadable() {
        // The verdict keys on the shape, not the wording, because the
        // placeholder is localized, so a translated build must reach the same
        // conclusion.
        for withheld in [
            "\"Nicht verfügbar\"",
            "\"管理者である必要があります\"",
            "\"\"",
        ] {
            assert_eq!(
                parse_current_exclusions(withheld).expect("a JSON string parses"),
                CurrentExclusions::Unreadable,
                "`{withheld}` is not a path, so the list was not read"
            );
        }
    }

    #[test]
    fn parse_drops_a_non_absolute_entry_from_a_real_list() {
        // A multi-element list is a genuine answer. An entry Patina would never
        // write (an env-var-relative exclusion added by hand) is skipped rather
        // than condemning the whole read as unreadable.
        assert_eq!(
            parse_current_exclusions("[\"C:\\\\a\", \"%USERPROFILE%\\\\x\", \"C:\\\\b\"]")
                .expect("array parses"),
            known(&[r"C:\a", r"C:\b"])
        );
    }

    #[test]
    fn parse_rejects_malformed_json() {
        parse_current_exclusions("{not json").expect_err("malformed JSON must be rejected");
    }

    #[test]
    fn receipt_parses_each_verdict_with_its_detail() {
        assert_eq!(parse_receipt("applied\n"), Some(DefenderReceipt::Applied));
        assert_eq!(
            parse_receipt("blocked exclusions not applied (TamperProtected=True)\n"),
            Some(DefenderReceipt::Blocked {
                detail: "exclusions not applied (TamperProtected=True)".to_owned()
            })
        );
        assert_eq!(
            parse_receipt("failed refusing to exclude `C:\\`\n"),
            Some(DefenderReceipt::Failed {
                detail: "refusing to exclude `C:\\`".to_owned()
            })
        );
    }

    #[test]
    fn receipt_verdict_without_a_detail_parses() {
        assert_eq!(
            parse_receipt("blocked"),
            Some(DefenderReceipt::Blocked {
                detail: String::new()
            })
        );
    }

    #[test]
    fn receipt_yields_none_for_anything_unrecognized() {
        // `None` means "no verdict yet", which keeps the launcher polling. An
        // empty or truncated file must land here rather than being read as a
        // verdict the helper never wrote.
        for not_a_verdict in ["", "\n", "applie", "garbage line", "APPLIED"] {
            assert_eq!(
                parse_receipt(not_a_verdict),
                None,
                "`{not_a_verdict}` must not resolve to a verdict"
            );
        }
    }

    #[test]
    fn ledger_round_trips_sorted_and_deterministic() {
        let desired = set(&[
            (r"C:\Users\kevin\dotfiles", ExclusionKind::Folder),
            (r"C:\Users\kevin\.gitconfig", ExclusionKind::File),
        ]);
        let ledger = DefenderLedger::from_set(&desired);
        let json = serde_json::to_string(&ledger).expect("serialize ledger");
        let back: DefenderLedger = serde_json::from_str(&json).expect("deserialize ledger");
        assert_eq!(ledger, back);
        // Sorted by the normalized key, so serialization is byte-stable.
        let second =
            serde_json::to_string(&DefenderLedger::from_set(&desired)).expect("re-serialize");
        assert_eq!(json, second, "the ledger file must be deterministic");
        assert_eq!(ledger.to_set(), desired);
    }

    #[test]
    fn serialize_request_writes_add_then_remove_lines() {
        let diff = DefenderDiff {
            to_add: vec![Exclusion::new(
                r"C:\Users\kevin\.gitconfig",
                ExclusionKind::File,
            )],
            to_remove: vec![Exclusion::new(
                r"C:\Users\kevin\.oldrc",
                ExclusionKind::File,
            )],
        };
        assert_eq!(
            serialize_request(&diff),
            "A C:\\Users\\kevin\\.gitconfig\nR C:\\Users\\kevin\\.oldrc\n"
        );
    }

    #[test]
    fn exclusion_kind_label_is_the_lowercase_kind_name() {
        assert_eq!(ExclusionKind::File.label(), "file");
        assert_eq!(ExclusionKind::Folder.label(), "folder");
    }

    #[test]
    fn exclusions_collapse_in_a_hash_set_on_the_normalized_key() {
        // Hash/Eq key on the normalized path, so entries differing only in case
        // and a trailing separator are one member of a HashSet, the same
        // identity the BTreeSet diff relies on, exercised through Hash.
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Exclusion::new(REPO, ExclusionKind::Folder));
        set.insert(Exclusion::new(
            REPO.to_ascii_lowercase() + "\\",
            ExclusionKind::Folder,
        ));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn derive_skips_an_invalid_repository_root_but_keeps_valid_targets() {
        // A repo root that fails validation (here UNC) is dropped from the
        // desired set with a warning rather than aborting the derivation.
        let mut resolved = plan(vec![op(FileMode::Symlink, &[r"C:\Users\kevin\.gitconfig"])]);
        resolved.repo_root = Utf8PathBuf::from(r"\\server\share\dotfiles");
        let desired = derive_exclusions(&resolved);
        assert!(
            !desired.iter().any(|e| e.path == resolved.repo_root),
            "the UNC repo root must be skipped"
        );
        assert!(
            desired.contains(&Exclusion::new(
                r"C:\Users\kevin\.gitconfig",
                ExclusionKind::File
            )),
            "a valid target must still be excluded"
        );
    }

    #[test]
    fn is_drive_root_guards_input_without_a_drive_and_colon() {
        // The early-return guard: a string too short to carry a drive letter and
        // colon is not a drive root. `validate_exclusion_path` only ever reaches
        // `is_drive_root` with a drive-absolute path, so this guard is exercised
        // directly.
        assert!(is_drive_root("C:"));
        assert!(is_drive_root(r"C:\"));
        assert!(!is_drive_root("C"));
        assert!(!is_drive_root(r"C:\Users"));
    }

    #[test]
    fn is_within_is_false_for_an_empty_directory() {
        // An empty directory key can never contain a path; the guard prevents a
        // vacuous prefix match against "".
        assert!(!is_within(
            Utf8Path::new(r"C:\Users\kevin"),
            Utf8Path::new("")
        ));
        assert!(is_within(
            Utf8Path::new(r"C:\Users\kevin\x"),
            Utf8Path::new(r"C:\Users\kevin")
        ));
    }

    #[test]
    fn state_dir_paths_are_distinct_files_under_the_state_dir() {
        let state_dir = Utf8Path::new(r"C:\Users\kevin\AppData\Local\patina");
        let ledger = defender_ledger_path(state_dir);
        let request = defender_request_path(state_dir);
        let result = defender_result_path(state_dir);
        for path in [&ledger, &request, &result] {
            assert_eq!(path.parent(), Some(state_dir));
        }
        let distinct: BTreeSet<&Utf8PathBuf> = [&ledger, &request, &result].into_iter().collect();
        assert_eq!(distinct.len(), 3, "the three files must not collide");
    }

    #[test]
    fn the_result_file_keeps_the_basename_the_helper_writes() {
        // `patina-elevate` cannot depend on this crate, so it spells this
        // basename itself and derives the path as a sibling of the request file
        // it was handed. Renaming the constant here alone would break the
        // handoff silently; pinning the literal here makes that break loud.
        let state_dir = Utf8Path::new(r"C:\Users\kevin\AppData\Local\patina");
        assert_eq!(
            defender_result_path(state_dir).file_name(),
            Some("defender-result.txt")
        );
    }
}
