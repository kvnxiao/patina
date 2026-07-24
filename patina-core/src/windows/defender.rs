//! Windows Defender path-exclusion derivation, diffing, validation, and the
//! per-machine ledger — the pure, cross-platform layer.
//!
//! An antivirus exclusion is a permanent blind spot, so this whole feature is
//! deliberately explicit, opt-in, previewed, consented, and reversible. This
//! module owns the parts that decide *which* exact paths Patina would exclude
//! and *how* a run reconciles the current Defender state against that set —
//! all of it IO-free and hostable on any platform, so the derivation and diff
//! logic is unit-testable on Linux CI with no real Defender in the loop.
//!
//! The Windows-only side — reading the live exclusion list through
//! `Get-MpPreference` (`HostDefenderProbe`) and launching the elevated
//! helper that performs the add/remove (`launch_defender_helper`) — lives at
//! the bottom of this file behind `#[cfg(windows)]`, mirroring how
//! `windows::elevate` splits the read side from the launch side.
//!
//! # The exclusion set
//!
//! The desired set is exactly `{ repo_root as Folder }` plus, for each managed
//! target the current plan materializes, **one** exclusion whose kind mirrors
//! the config: a [`ExclusionKind::Folder`] for a directory mode
//! (`symlink` / `symlink-tree` / `copy` on a `[[directory]]`) and a
//! [`ExclusionKind::File`] for a file mode (`symlink` / `copy` / template on a
//! `[[file]]`). A `symlink-tree` of forty files contributes the **one**
//! declared target directory, never forty leaf entries — see
//! [`derive_exclusions`], which walks `resolved.operations` directly rather
//! than [`crate::apply::engine::current_managed_targets`] (that helper expands
//! trees to per-leaf keys, the opposite of the 1:1 decision here).
//!
//! # Why the normalized key matters
//!
//! `Get-MpPreference` may echo an excluded path back with different casing or a
//! trailing separator than Patina wrote. If the diff compared raw strings,
//! every re-run would see spurious add/remove churn and the
//! deterministic-stdout contract would break. [`Exclusion`]'s `Eq` / `Ord` /
//! `Hash` therefore key on a normalized form (case-folded, separators unified,
//! trailing separator stripped) while the original casing is preserved for
//! display and for the add/remove call. This is the single most important
//! correctness point in the feature: it is what makes re-runs idempotent.

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

/// The request-line prefix (character plus one space) marking a path to add.
const REQUEST_ADD_PREFIX: &str = "A ";

/// The request-line prefix (character plus one space) marking a path to remove.
const REQUEST_REMOVE_PREFIX: &str = "R ";

/// Environment variables naming system directories that must never be excluded.
///
/// Read at validation time (empty on non-Windows, where the variables are
/// unset, so the structural checks still run and the denylist simply matches
/// nothing). The `%SystemDrive%` root is covered by the separate drive-root
/// rejection rather than listed here. Duplicated verbatim in
/// `patina-elevate`'s independent validator — the helper cannot depend on
/// `patina-core`, so the trust boundary is re-enforced there with its own copy.
const SYSTEM_DIR_ENV_VARS: [&str; 4] = [
    "SystemRoot",
    "ProgramFiles",
    "ProgramW6432",
    "ProgramFiles(x86)",
];

/// Whether an excluded path names a single file or a whole folder.
///
/// Defender's `-ExclusionPath` API does not itself distinguish the two — a path
/// exclusion covers whatever lives at that path — so the kind is carried purely
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
/// Equality, ordering, and hashing use a **normalized key** (case-folded,
/// separators unified, trailing separator stripped) so two exclusions that
/// differ only in letter case or a
/// trailing separator compare equal and collapse in a set — the guard that
/// keeps re-runs from churning. The stored `path` keeps its original casing for
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
/// and the result ASCII-lowercased — the folding Windows itself applies when it
/// matches an excluded path, and the exact set of differences
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
/// over `resolved.operations` — which already excludes `when`-false entries and
/// carries a tree entry's single declared target directory rather than its
/// expanded leaves — gated entries contribute nothing and a `symlink-tree`
/// contributes exactly one folder exclusion.
///
/// Any candidate (the repo root or a target) that fails
/// [`validate_exclusion_path`] — a UNC path, a drive-relative path, a system
/// directory — is skipped with a warning rather than aborting the whole run.
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

/// The reconciliation between the desired exclusion set, the live Defender
/// state, and the Patina-owned ledger.
///
/// [`plan_defender`] computes this; the preview renders it and the elevated
/// helper enacts it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefenderDiff {
    /// Desired exclusions not currently present — the helper adds these.
    pub to_add: Vec<Exclusion>,
    /// Patina-owned exclusions no longer desired but still present — the
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
/// - `to_add` = the desired exclusions whose path is not already present.
/// - `to_remove` = the ledger entries (Patina-owned) that are no longer desired
///   **and** are actually still present. Anchoring removals to the ledger is
///   what guarantees a user-added exclusion is never touched: a path Patina did
///   not record is never a removal candidate. Anchoring them to `current` too
///   keeps the diff honest — an already-gone entry is not re-removed.
///
/// The identity case (desired equals the present set, ledger equals desired)
/// yields an empty diff, which is the idempotency guarantee re-runs depend on.
#[must_use = "the diff must be previewed and enacted"]
pub fn plan_defender(
    desired: &BTreeSet<Exclusion>,
    current: &BTreeSet<Utf8PathBuf>,
    ledger: &BTreeSet<Exclusion>,
) -> DefenderDiff {
    let current_keys: BTreeSet<String> = current.iter().map(|p| normalized_key(p)).collect();

    let to_add: Vec<Exclusion> = desired
        .iter()
        .filter(|exclusion| !current_keys.contains(&exclusion.key()))
        .cloned()
        .collect();

    let to_remove: Vec<Exclusion> = ledger
        .iter()
        .filter(|exclusion| {
            !desired.contains(*exclusion) && current_keys.contains(&exclusion.key())
        })
        .cloned()
        .collect();

    DefenderDiff { to_add, to_remove }
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
/// The checks are purely **lexical** — a managed target may not exist yet, so
/// nothing here touches the filesystem. A path passes only when it is a
/// drive-letter-absolute Windows path, is not UNC, contains no wildcard, is not
/// a bare drive root, and does not fall under an env-derived system directory
/// (`%SystemRoot%`, `%ProgramFiles%`, `%ProgramW6432%`, `%ProgramFiles(x86)%`,
/// or any drive root as a stand-in for `%SystemDrive%`).
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
/// structural checks in [`validate_exclusion_path_with`] apply — which is what
/// keeps the structural rejections testable on Linux CI.
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
/// A pure string check so it holds the same verdict on every platform: this is
/// what lets Linux CI validate a Windows path string.
fn is_windows_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(&drive), Some(&b':'), Some(&sep)) = (bytes.first(), bytes.get(1), bytes.get(2))
    else {
        return false;
    };
    drive.is_ascii_alphabetic() && (sep == b'\\' || sep == b'/')
}

/// Whether `s` is a bare drive root — a drive letter and colon followed by only
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

/// Parse the `ExclusionPath` JSON emitted by `Get-MpPreference` into the set of
/// currently-excluded paths.
///
/// `Get-MpPreference | Select ExclusionPath | ConvertTo-Json` (Windows
/// PowerShell 5.1, which has no `-AsArray`) collapses to one of three shapes,
/// all handled here:
///
/// - **empty / `null`** — no exclusions are configured → an empty set.
/// - **a bare JSON string** — exactly one exclusion.
/// - **a JSON array of strings** — several exclusions.
///
/// Non-string array elements and other unexpected scalar shapes yield no paths
/// rather than an error; only malformed JSON is an error.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] when the input is not valid
/// JSON.
pub fn parse_exclusion_paths(json: &str) -> Result<BTreeSet<Utf8PathBuf>, serde_json::Error> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(BTreeSet::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)?;
    let mut paths = BTreeSet::new();
    match value {
        serde_json::Value::String(single) => {
            paths.insert(Utf8PathBuf::from(single));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let serde_json::Value::String(path) = item {
                    paths.insert(Utf8PathBuf::from(path));
                }
            }
        }
        // `null`, a number, a bare object: no usable exclusion paths.
        _ => {}
    }
    Ok(paths)
}

/// The per-machine record of the exclusions **Patina** owns.
///
/// Written only by the unprivileged CLI, never by the elevated helper. It is
/// what lets a reconcile reap a stale Patina exclusion while leaving a
/// user-added exclusion untouched — a path absent from the ledger is never a
/// removal candidate.
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

/// Serialize a diff into the request-file body the elevated helper consumes.
///
/// One line per operation: `A <path>` to add, `R <path>` to remove, with the
/// path written verbatim (it is read back as literal data, never interpreted as
/// code). The ordering is the diff's own — deterministic — so the request file
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
    /// Read the set of paths Defender currently excludes.
    ///
    /// # Errors
    ///
    /// Returns a [`DefenderError`] when the underlying read fails.
    fn read_exclusions(&self) -> Result<BTreeSet<Utf8PathBuf>, DefenderError>;
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
    use super::DefenderDiff;
    use super::DefenderError;
    use super::DefenderProbe;
    use super::normalized_key;
    use super::parse_exclusion_paths;
    use crate::apply::resolve_on_path;
    use crate::windows::WindowsError;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use std::collections::BTreeSet;
    use std::process::Command;
    use winsafe::co;

    /// The verb that asks the shell to launch a target elevated, raising the
    /// UAC consent dialog.
    const RUNAS_VERB: &str = "runas";

    /// The helper subcommand that applies the request file's add/remove set.
    const HELPER_SUBCOMMAND: &str = "apply-defender-exclusions";

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
        fn read_exclusions(&self) -> Result<BTreeSet<Utf8PathBuf>, DefenderError> {
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
            parse_exclusion_paths(&stdout).map_err(|source| DefenderError::Parse { source })
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
    /// Mirrors [`crate::windows::elevate::ElevationOutcome`]: [`Applied`] lets
    /// the CLI rewrite the ledger; [`Declined`] is the exit-5 UAC-declined
    /// path; [`Blocked`] is the exit-1 path when the helper ran but a
    /// post-run re-read shows the change did not take (Tamper Protection /
    /// managed Defender).
    ///
    /// [`Applied`]: DefenderOutcome::Applied
    /// [`Declined`]: DefenderOutcome::Declined
    /// [`Blocked`]: DefenderOutcome::Blocked
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DefenderOutcome {
        /// The helper ran and a re-read confirms the desired add/remove took.
        Applied,
        /// The user dismissed the UAC consent dialog (`ERROR_CANCELLED`).
        Declined,
        /// The helper ran but the re-read shows the exclusions did not change
        /// as requested — the write was silently rejected.
        Blocked,
    }

    /// Launch the elevated helper to enact `diff`, then re-read Defender state
    /// to classify the outcome.
    ///
    /// The `request_path` is passed as a **quoted** `ShellExecuteEx` parameter
    /// (exclusion paths contain spaces). After the helper returns, this
    /// re-reads the live exclusion list and confirms every add is now present
    /// and every removal now absent — the mandatory verification that catches a
    /// Tamper-Protected write which returned success but changed nothing.
    ///
    /// # Errors
    ///
    /// Returns [`WindowsError`] when the running executable cannot be located
    /// or the `ShellExecuteEx` launch fails for a reason other than the
    /// user declining consent (reported as [`DefenderOutcome::Declined`],
    /// not an error).
    pub fn launch_defender_helper(
        request_path: &Utf8Path,
        diff: &DefenderDiff,
    ) -> Result<DefenderOutcome, WindowsError> {
        let helper = crate::windows::elevate::helper_path()?;
        let parameters = format!("{HELPER_SUBCOMMAND} \"{request_path}\"");

        let info = winsafe::SHELLEXECUTEINFO {
            verb: Some(RUNAS_VERB),
            file: &helper,
            parameters: Some(&parameters),
            show: co::SW::HIDE,
            ..Default::default()
        };

        match winsafe::ShellExecuteEx(&info) {
            Ok(()) => Ok(verify_outcome(diff)),
            Err(err) if err == co::ERROR::CANCELLED => Ok(DefenderOutcome::Declined),
            Err(err) => Err(WindowsError::WinApi {
                call: "ShellExecuteEx",
                source: std::io::Error::other(err),
            }),
        }
    }

    /// Re-read Defender state and classify whether `diff` took effect.
    ///
    /// A failed re-read is conservatively treated as
    /// [`DefenderOutcome::Blocked`] — the CLI must not report success it
    /// cannot confirm.
    fn verify_outcome(diff: &DefenderDiff) -> DefenderOutcome {
        let Ok(current) = HostDefenderProbe.read_exclusions() else {
            return DefenderOutcome::Blocked;
        };
        let current_keys: BTreeSet<String> = current.iter().map(|p| normalized_key(p)).collect();
        let adds_present = diff
            .to_add
            .iter()
            .all(|exclusion| current_keys.contains(&exclusion.key()));
        let removes_absent = diff
            .to_remove
            .iter()
            .all(|exclusion| !current_keys.contains(&exclusion.key()));
        if adds_present && removes_absent {
            DefenderOutcome::Applied
        } else {
            DefenderOutcome::Blocked
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
        }
    }

    fn set(paths: &[(&str, ExclusionKind)]) -> BTreeSet<Exclusion> {
        paths
            .iter()
            .map(|(p, kind)| Exclusion::new(*p, *kind))
            .collect()
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
        let current: BTreeSet<Utf8PathBuf> = [Utf8PathBuf::from(REPO)].into_iter().collect();
        let diff = plan_defender(&desired, &current, &BTreeSet::new());
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
        let current: BTreeSet<Utf8PathBuf> = [
            Utf8PathBuf::from(REPO),
            Utf8PathBuf::from(r"C:\Users\kevin\.oldrc"),
        ]
        .into_iter()
        .collect();
        let diff = plan_defender(&desired, &current, &ledger);
        assert!(diff.to_add.is_empty());
        assert_eq!(diff.to_remove, vec![stale]);
    }

    #[test]
    fn plan_never_removes_a_path_outside_the_ledger() {
        // A user-added exclusion (present, not desired, not in the ledger) is
        // never a removal candidate.
        let desired = set(&[(REPO, ExclusionKind::Folder)]);
        let ledger = set(&[(REPO, ExclusionKind::Folder)]);
        let current: BTreeSet<Utf8PathBuf> = [
            Utf8PathBuf::from(REPO),
            Utf8PathBuf::from(r"C:\Users\kevin\user-added"),
        ]
        .into_iter()
        .collect();
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
        let current: BTreeSet<Utf8PathBuf> = [
            Utf8PathBuf::from(REPO),
            Utf8PathBuf::from(r"C:\Users\kevin\.gitconfig"),
        ]
        .into_iter()
        .collect();
        let diff = plan_defender(&desired, &current, &desired);
        assert!(diff.is_empty(), "identity must be a no-op: {diff:?}");
    }

    #[test]
    fn plan_case_and_trailing_separator_differences_are_no_diff() {
        // The key idempotency guard: Get-MpPreference echoing a path back with
        // different case or a trailing separator must produce no churn.
        let desired = set(&[(r"C:\Users\kevin\dotfiles", ExclusionKind::Folder)]);
        let current: BTreeSet<Utf8PathBuf> = [Utf8PathBuf::from(r"c:\users\kevin\DOTFILES\")]
            .into_iter()
            .collect();
        let ledger = desired.clone();
        let diff = plan_defender(&desired, &current, &ledger);
        assert!(
            diff.is_empty(),
            "case/trailing-separator differences must not produce a diff: {diff:?}"
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
    fn parse_handles_zero_one_and_many_shapes() {
        // null / empty ⇒ none.
        assert!(
            parse_exclusion_paths("null")
                .expect("null parses")
                .is_empty()
        );
        assert!(
            parse_exclusion_paths("   ")
                .expect("blank parses")
                .is_empty()
        );
        // A bare string ⇒ one (the PowerShell 5.1 single-element collapse).
        assert_eq!(
            parse_exclusion_paths("\"C:\\\\Users\\\\kevin\\\\dotfiles\"").expect("scalar parses"),
            [Utf8PathBuf::from(r"C:\Users\kevin\dotfiles")]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        // An array ⇒ many.
        let many = parse_exclusion_paths("[\"C:\\\\a\", \"C:\\\\b\"]").expect("array parses");
        assert_eq!(
            many,
            [Utf8PathBuf::from(r"C:\a"), Utf8PathBuf::from(r"C:\b")]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn parse_rejects_malformed_json() {
        parse_exclusion_paths("{not json").expect_err("malformed JSON must be rejected");
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
        // and a trailing separator are one member of a HashSet — the same
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
        assert_eq!(ledger.parent(), Some(state_dir));
        assert_eq!(request.parent(), Some(state_dir));
        assert_ne!(ledger, request, "ledger and request must be distinct files");
    }
}
