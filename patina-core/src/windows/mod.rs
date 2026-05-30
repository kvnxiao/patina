//! Windows Developer Mode detection and the symlink-elevation gate
//! decision (REQ-007, read side).
//!
//! REQ-007 requires that, on Windows, an apply whose plan contains any
//! `symlink` / `symlink-dir` operation only proceeds when the host can
//! create symbolic links without elevation — which on Windows means
//! Developer Mode is enabled (the `AllowDevelopmentWithoutDevLicense`
//! registry flag is `1`), or the invoking process is already elevated.
//!
//! Per DEC-008 the engine crate owns the *capability* — the IO-free
//! reads of the registry flag, the process-token elevation check, and
//! the OS-build query — while the *orchestration* (the UAC prompt, the
//! decline → exit-5 path, re-driving `execute_plan`) lives in
//! `patina-cli`. This module is therefore the read side only: it exposes
//! the typed queries plus the pure gate-decision function. The elevation
//! launch and the engine gate wiring land in T-009.
//!
//! Everything here compiles on every platform. The Windows-specific
//! registry and token reads live in the [`registry`] submodule behind
//! `#[cfg(windows)]`; on every other platform the entry points reduce to
//! the stubs documented on each function, so the macOS/Linux CI builds
//! clean and the gate-decision logic is unit-testable on Linux against a
//! fake [`DevModeProbe`].
//!
//! # Examples
//!
//! ```
//! use patina_core::windows::{is_unc_path, DevModeStatus};
//! use camino::Utf8Path;
//!
//! // Pure helpers run on every platform.
//! assert!(is_unc_path(Utf8Path::new(r"\\server\share\dotfiles")));
//! assert!(!is_unc_path(Utf8Path::new("/home/user/dotfiles")));
//!
//! // On a non-Windows host the registry is never touched.
//! # #[cfg(not(windows))]
//! assert_eq!(patina_core::windows::dev_mode_status(), DevModeStatus::NotWindows);
//! ```

use crate::apply::engine::ResolvedPlan;
use crate::config::FileMode;
use camino::Utf8Path;

#[cfg(windows)]
pub(crate) mod registry;

/// Result of querying the Windows Developer Mode registry flag.
///
/// The flag in question is `AllowDevelopmentWithoutDevLicense` under
/// `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevModeStatus {
    /// Running on Windows and the registry flag is present and set to 1.
    Enabled,
    /// Running on Windows and the registry flag is absent or set to 0.
    Disabled,
    /// Running on Windows, but the OS build predates Developer Mode
    /// (Windows 10 1703) so the flag has no meaning.
    Unsupported,
    /// Not running on Windows; the concept does not apply.
    NotWindows,
}

/// Errors from the Windows registry / token reads in [`registry`].
///
/// These only ever arise on Windows; on other platforms the entry points
/// return a fixed value and never produce a [`WindowsError`]. The variant
/// carries the failing Win32 call's name so the CLI can surface an
/// actionable message (REQ-007 names the registry path on a post-helper
/// re-read failure).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WindowsError {
    /// A Win32 call backing one of the registry / token reads failed.
    #[error("Windows API call `{call}` failed: {source}")]
    WinApi {
        /// The Win32 / winsafe call that failed (e.g. `RegOpenKeyEx`).
        call: &'static str,
        /// The underlying winsafe system error.
        #[source]
        source: std::io::Error,
    },
}

/// Whether the running process can create symbolic links without a
/// per-operation elevation prompt.
///
/// On non-Windows hosts this is always [`DevModeStatus::NotWindows`] and
/// **no registry access is attempted** — the read returns immediately.
///
/// On Windows it reads `AllowDevelopmentWithoutDevLicense`. A successful
/// read of `1` yields [`DevModeStatus::Enabled`]; an absent key or a `0`
/// value yields [`DevModeStatus::Disabled`]; a read on a pre-1703 build
/// yields [`DevModeStatus::Unsupported`]. A failed read is treated as
/// [`DevModeStatus::Disabled`] — the safe default, since it routes the
/// caller into the elevation flow rather than silently skipping a symlink
/// the user asked for.
#[must_use = "the Developer Mode status decides whether the symlink-elevation gate fires"]
pub fn dev_mode_status() -> DevModeStatus {
    #[cfg(windows)]
    {
        if !windows_build_supports_dev_mode() {
            return DevModeStatus::Unsupported;
        }
        match registry::read_dev_mode_flag() {
            Ok(Some(1)) => DevModeStatus::Enabled,
            // Everything else is Disabled: an absent key, an explicit 0,
            // any non-1 value, or a failed read. Treating a read error as
            // Disabled is the safe default — it routes the caller into the
            // elevation flow rather than silently skipping a requested
            // symlink.
            Ok(_) | Err(_) => DevModeStatus::Disabled,
        }
    }
    #[cfg(not(windows))]
    {
        DevModeStatus::NotWindows
    }
}

/// Whether the current process is running with an elevated token.
///
/// On non-Windows hosts this is always `false` (Unix elevation is
/// orthogonal to symlink creation, which never needs root). On Windows it
/// inspects the process token's `TokenElevation` information; a failed
/// query is reported as `false` (not elevated), the conservative default
/// that keeps the elevation gate engaged.
#[must_use = "the elevation state suppresses the Developer Mode prompt when already elevated"]
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        registry::process_is_elevated().unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Whether the running OS is Windows 10 build 1703 (15063) or newer, the
/// first build to support Developer Mode symlink creation.
///
/// On non-Windows hosts this is always `false` — Developer Mode is a
/// Windows-only concept and callers should never reach the build check on
/// another platform.
#[must_use = "the build floor distinguishes Unsupported from Disabled on Windows"]
pub fn windows_build_supports_dev_mode() -> bool {
    #[cfg(windows)]
    {
        registry::build_number().is_some_and(|build| build >= registry::DEV_MODE_MIN_BUILD)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Whether `path` is a Windows UNC path (begins with `\\`).
///
/// A pure prefix check defined on every platform so `doctor`'s UNC
/// finding (REQ-006) is unit-testable on the macOS/Linux CI. UNC paths
/// cannot host symbolic links, so the finding warns regardless of the
/// host the check runs on.
///
/// # Examples
///
/// ```
/// use patina_core::windows::is_unc_path;
/// use camino::Utf8Path;
///
/// assert!(is_unc_path(Utf8Path::new(r"\\fileserver\share\dotfiles")));
/// assert!(!is_unc_path(Utf8Path::new("/home/user/dotfiles")));
/// ```
#[must_use = "the UNC verdict drives the doctor finding"]
pub fn is_unc_path(path: &Utf8Path) -> bool {
    path.as_str().starts_with(r"\\")
}

/// Whether the resolved plan contains any operation that creates a
/// symbolic link ([`FileMode::Symlink`] or [`FileMode::SymlinkDir`]).
///
/// This is the predicate that gates the whole Developer Mode flow: only a
/// plan that creates symbolic links can require Developer Mode, so a plan
/// of pure copies / renders never prompts (REQ-007 done-when).
#[must_use = "the symlink predicate gates the Developer Mode flow"]
pub fn plan_has_symlink_op(plan: &ResolvedPlan) -> bool {
    plan.operations
        .iter()
        .any(|op| matches!(op.mode, FileMode::Symlink | FileMode::SymlinkDir))
}

/// The host-state inputs the symlink-elevation gate decision needs.
///
/// Abstracting these two reads behind a trait lets T-009's gate logic be
/// unit-tested on Linux against a fake probe, with no real registry or
/// process token in the loop. The production implementation
/// ([`HostDevModeProbe`]) wires the trait to [`dev_mode_status`] and
/// [`is_elevated`].
pub trait DevModeProbe {
    /// The Developer Mode registry status on this host.
    fn dev_mode_status(&self) -> DevModeStatus;
    /// Whether the current process is already elevated.
    fn is_elevated(&self) -> bool;
}

/// The production [`DevModeProbe`]: delegates to the real host reads.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct HostDevModeProbe;

impl DevModeProbe for HostDevModeProbe {
    fn dev_mode_status(&self) -> DevModeStatus {
        dev_mode_status()
    }

    fn is_elevated(&self) -> bool {
        is_elevated()
    }
}

/// The outcome of the symlink-elevation gate decision.
///
/// T-009 maps these variants to the CLI orchestration: `Proceed` runs the
/// apply unchanged; `ProceedElevatedWarning` runs it but emits the
/// "avoid running Patina elevated" warning (REQ-007 done-when);
/// `RequireElevation` drives the UAC prompt and, on decline, exits 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// No symlink in the plan, not on Windows, or Developer Mode is
    /// already enabled — run the apply with no prompt.
    Proceed,
    /// Developer Mode is disabled but the process is already elevated, so
    /// symlink creation will succeed; proceed and warn that running
    /// elevated is discouraged.
    ProceedElevatedWarning,
    /// Developer Mode is disabled and the process is not elevated — the
    /// caller must drive the one-time UAC elevation flow before applying.
    RequireElevation,
}

/// Decide whether a plan can be applied as-is, or whether the Developer
/// Mode elevation flow must run first.
///
/// The decision is pure over (`plan`, `probe`): it never touches the
/// filesystem or the registry directly, so it is fully unit-testable
/// against a fake [`DevModeProbe`] on any platform.
///
/// The rules (REQ-007):
///
/// - A plan with no symbolic link operation always [`Proceed`]s — there is
///   nothing that needs Developer Mode.
/// - [`DevModeStatus::NotWindows`] always [`Proceed`]s — the check is
///   Windows-only.
/// - [`DevModeStatus::Enabled`] [`Proceed`]s — links create cleanly.
/// - [`DevModeStatus::Unsupported`] [`Proceed`]s — there is no Developer Mode
///   flag to toggle on this build, so the gate cannot help; the apply runs and
///   any per-link failure surfaces from the executor.
/// - [`DevModeStatus::Disabled`] with an elevated process yields
///   [`ProceedElevatedWarning`]; otherwise [`RequireElevation`].
///
/// [`Proceed`]: GateDecision::Proceed
/// [`ProceedElevatedWarning`]: GateDecision::ProceedElevatedWarning
/// [`RequireElevation`]: GateDecision::RequireElevation
#[must_use = "the gate decision selects the apply path"]
pub fn decide_symlink_gate(plan: &ResolvedPlan, probe: &impl DevModeProbe) -> GateDecision {
    if !plan_has_symlink_op(plan) {
        return GateDecision::Proceed;
    }
    match probe.dev_mode_status() {
        DevModeStatus::Enabled | DevModeStatus::Unsupported | DevModeStatus::NotWindows => {
            GateDecision::Proceed
        }
        DevModeStatus::Disabled => {
            if probe.is_elevated() {
                GateDecision::ProceedElevatedWarning
            } else {
                GateDecision::RequireElevation
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::engine::ResolvedOperation;
    use crate::journal::Plan;
    use crate::state_dir::HostOs;
    use crate::variables::Builtins;
    use crate::variables::Resolver;
    use camino::Utf8PathBuf;

    /// A fake probe so the gate decision is testable with no real
    /// registry or process token (the T-009 logic exercises this seam).
    struct FakeProbe {
        status: DevModeStatus,
        elevated: bool,
    }

    impl DevModeProbe for FakeProbe {
        fn dev_mode_status(&self) -> DevModeStatus {
            self.status
        }
        fn is_elevated(&self) -> bool {
            self.elevated
        }
    }

    fn op(mode: FileMode) -> ResolvedOperation {
        ResolvedOperation {
            mode,
            source: Utf8PathBuf::from("/repo/src"),
            targets: vec![Utf8PathBuf::from("/home/user/target")],
        }
    }

    /// Build a minimal in-memory `ResolvedPlan` carrying only the
    /// `operations` the predicate reads; the rest are inert placeholders.
    fn plan(ops: Vec<ResolvedOperation>) -> ResolvedPlan {
        ResolvedPlan {
            repo_root: Utf8PathBuf::from("/repo"),
            profile: String::new(),
            plan: Plan::new(Vec::new()),
            operations: ops,
            hooks: Vec::new(),
            state_dir: Utf8PathBuf::from("/state"),
            host_os: HostOs::current(),
            timestamp: "fixed".to_owned(),
            resolver: Resolver::new(Builtins::current()),
        }
    }

    #[test]
    fn non_windows_status_is_not_windows_and_unelevated() {
        // Task scenario 1: on a non-Windows host the reads return
        // NotWindows / false and no registry access happens (the
        // #[cfg(not(windows))] arms never call into `registry`).
        #[cfg(not(windows))]
        {
            assert_eq!(dev_mode_status(), DevModeStatus::NotWindows);
            assert!(!is_elevated());
            assert!(!windows_build_supports_dev_mode());
        }
    }

    #[test]
    fn unc_prefix_check_distinguishes_unc_from_posix() {
        // Task scenario 2.
        assert!(is_unc_path(Utf8Path::new(r"\\fileserver\share\dotfiles")));
        assert!(!is_unc_path(Utf8Path::new("/home/user/dot")));
        // A single leading backslash is a drive-relative path, not UNC.
        assert!(!is_unc_path(Utf8Path::new(r"\Users\dot")));
    }

    #[test]
    fn plan_predicate_detects_symlink_modes_only() {
        assert!(plan_has_symlink_op(&plan(vec![op(FileMode::Symlink)])));
        assert!(plan_has_symlink_op(&plan(vec![op(FileMode::SymlinkDir)])));
        assert!(plan_has_symlink_op(&plan(vec![
            op(FileMode::Copy),
            op(FileMode::SymlinkDir),
        ])));
        assert!(!plan_has_symlink_op(&plan(vec![
            op(FileMode::Copy),
            op(FileMode::CopyTree),
            op(FileMode::TemplateRender),
        ])));
        assert!(!plan_has_symlink_op(&plan(vec![])));
    }

    #[test]
    fn gate_disabled_not_elevated_requires_elevation() {
        // Task scenario 3, first half: Disabled + not-elevated + one
        // symlink op ⇒ elevation required.
        let probe = FakeProbe {
            status: DevModeStatus::Disabled,
            elevated: false,
        };
        let decision = decide_symlink_gate(&plan(vec![op(FileMode::Symlink)]), &probe);
        assert_eq!(decision, GateDecision::RequireElevation);
    }

    #[test]
    fn gate_enabled_proceeds() {
        // Task scenario 3, second half: same plan, Enabled ⇒ proceed.
        let probe = FakeProbe {
            status: DevModeStatus::Enabled,
            elevated: false,
        };
        let decision = decide_symlink_gate(&plan(vec![op(FileMode::Symlink)]), &probe);
        assert_eq!(decision, GateDecision::Proceed);
    }

    #[test]
    fn gate_disabled_but_elevated_proceeds_with_warning() {
        let probe = FakeProbe {
            status: DevModeStatus::Disabled,
            elevated: true,
        };
        let decision = decide_symlink_gate(&plan(vec![op(FileMode::SymlinkDir)]), &probe);
        assert_eq!(decision, GateDecision::ProceedElevatedWarning);
    }

    #[test]
    fn gate_no_symlink_op_proceeds_regardless_of_status() {
        // A copy-only plan never prompts even when Developer Mode is off
        // and the process is unelevated (REQ-007 done-when).
        let probe = FakeProbe {
            status: DevModeStatus::Disabled,
            elevated: false,
        };
        let decision = decide_symlink_gate(&plan(vec![op(FileMode::Copy)]), &probe);
        assert_eq!(decision, GateDecision::Proceed);
    }

    #[test]
    fn gate_unsupported_build_proceeds() {
        // No flag to toggle on a pre-1703 build: proceed and let the
        // executor surface any per-link failure.
        let probe = FakeProbe {
            status: DevModeStatus::Unsupported,
            elevated: false,
        };
        let decision = decide_symlink_gate(&plan(vec![op(FileMode::Symlink)]), &probe);
        assert_eq!(decision, GateDecision::Proceed);
    }
}
