//! Hook execution with `must_succeed` semantics and the `--force-deploy`
//! override (REQ-006).
//!
//! The parser side (T-004) produces a [`HookEntry`] per `[[hook]]` table
//! array; this module runs those hooks at the right times in the apply
//! pipeline and reports each hook's outcome back to the orchestrator. The
//! responsibilities split into three steps, called in order by the apply
//! pipeline (T-016 / T-018):
//!
//! 1. **Resolve shells up front.** [`resolve_shells`] resolves every hook's
//!    shell *before any hook runs*. An explicit `shell` that does not resolve
//!    on `PATH` is a [`HookError::ShellNotFound`] surfaced here, so an
//!    unresolved shell aborts the apply before a single file operation or hook
//!    command executes. An omitted `shell` defaults to `bash` on macOS / Linux
//!    and `pwsh` on Windows.
//! 2. **Filter by `when`.** [`should_run`] evaluates a hook's optional `when`
//!    predicate through the shared T-008 [`Engine`] against the resolved
//!    variable context. A hook whose predicate is `false` is filtered out
//!    before execution; a hook with no predicate always runs.
//! 3. **Run and classify.** [`run_hook`] spawns the resolved shell with the
//!    hook command, awaits its exit status, and maps the status to a
//!    [`HookOutcome`] under the hook's `must_succeed` flag and the
//!    invocation-wide [`ForceDeploy`] override.
//!
//! # Outcome semantics
//!
//! The outcome is a *classification*, not an action: this module never
//! itself aborts the apply, rolls back, or writes to the user's terminal.
//! It returns a [`HookOutcome`] and lets the orchestrator (which owns the
//! journal, the rollback machinery, and the `output::Reporter` surface)
//! decide. The mapping the orchestrator applies:
//!
//! - [`HookOutcome::Succeeded`] — the hook exited zero; proceed.
//! - [`HookOutcome::Warned`] — the hook exited non-zero but degraded to a
//!   warning (either `must_succeed = false` or `--force-deploy` flipped it
//!   off). The orchestrator surfaces the warning and proceeds.
//! - [`HookOutcome::Failed`] — the hook exited non-zero with `must_succeed =
//!   true` and no `--force-deploy` override. For a `pre_apply` hook the
//!   orchestrator aborts before any file operation (CLI exit 2); for a
//!   `post_apply` hook it rolls back every file operation (CLI exit 3). The
//!   [`HookEvent`] on the originating [`HookEntry`] tells the orchestrator
//!   which.
//!
//! `--force-deploy` ([`ForceDeploy::Yes`]) overrides every hook in the
//! invocation to behave as `must_succeed = false`, so a non-zero exit can
//! only ever degrade to [`HookOutcome::Warned`], never
//! [`HookOutcome::Failed`].

use crate::config::HookEntry;
use crate::state_dir::HostOs;
use crate::template::Engine;
use crate::template::TemplateError;
use crate::variables::Resolver;
use camino::Utf8PathBuf;
use std::path::Path;
use tokio::process::Command;

/// Invocation-wide `--force-deploy` toggle.
///
/// When [`ForceDeploy::Yes`], every hook in the current `patina apply`
/// invocation is treated as `must_succeed = false` regardless of its
/// declared value, so a non-zero exit degrades to a warning rather than
/// aborting or rolling back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceDeploy {
    /// `--force-deploy` was not passed; honour each hook's declared
    /// `must_succeed`.
    No,
    /// `--force-deploy` was passed; override every hook to
    /// `must_succeed = false`.
    Yes,
}

impl ForceDeploy {
    /// Whether a hook's failure must abort, given its declared
    /// `must_succeed`. Under [`ForceDeploy::Yes`] this is always `false`.
    fn enforces(self, must_succeed: bool) -> bool {
        match self {
            ForceDeploy::Yes => false,
            ForceDeploy::No => must_succeed,
        }
    }
}

/// Failures from hook preparation and execution (REQ-006).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HookError {
    /// A hook declared an explicit `shell` that does not resolve to an
    /// executable on the current `PATH`. Surfaced during
    /// [`resolve_shells`], before any hook command runs, so the apply can
    /// abort before mutating the filesystem.
    #[error("hook shell `{shell}` was not found on PATH")]
    ShellNotFound {
        /// The unresolved shell binary named by the hook.
        shell: String,
    },

    /// Evaluating a hook's `when` predicate failed (an undefined variable
    /// under strict-undefined, or a syntax / evaluation error).
    #[error("hook `when` predicate failed: {source}")]
    When {
        /// The underlying template-evaluation error.
        #[source]
        source: TemplateError,
    },

    /// Spawning or awaiting the hook's shell process failed at the OS
    /// level (the binary resolved but could not be executed, or the child
    /// could not be waited on).
    #[error("hook command `{command}` failed to execute: {source}")]
    Spawn {
        /// The hook command that could not be run.
        command: String,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

/// A hook whose shell has been resolved to a concrete executable, paired
/// with the originating [`HookEntry`].
///
/// Produced by [`resolve_shells`] for every hook up front so an
/// unresolved explicit shell aborts the apply before any hook runs. The
/// borrow of the originating entry keeps `command`, `when`, `event`, and
/// `must_succeed` available to [`should_run`] and [`run_hook`] without
/// cloning.
#[derive(Debug, Clone)]
pub struct ResolvedHook<'a> {
    /// The originating parsed hook entry.
    pub entry: &'a HookEntry,
    /// The shell binary to invoke (a default name like `bash` / `pwsh`,
    /// or the explicit shell the entry declared once confirmed on PATH).
    shell: String,
}

/// How a single hook command terminated, after applying `must_succeed`
/// and the [`ForceDeploy`] override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    /// The hook exited zero.
    Succeeded,
    /// The hook exited non-zero but `must_succeed` was effectively
    /// `false` (declared so, or overridden by `--force-deploy`), so the
    /// failure degrades to a warning and the apply proceeds.
    Warned,
    /// The hook exited non-zero with `must_succeed` effectively `true`.
    /// The orchestrator aborts (`pre_apply`) or rolls back (`post_apply`)
    /// based on the originating [`HookEvent`].
    Failed,
}

/// Resolve the shell for every hook up front, before any hook runs.
///
/// An omitted `shell` defaults to the platform shell for `host_os`
/// (`bash` on macOS / Linux, `pwsh` on Windows). An explicit `shell` must
/// resolve to an executable on the current `PATH`.
///
/// # Errors
///
/// Returns [`HookError::ShellNotFound`] naming the first explicit shell
/// that does not resolve on `PATH`. Resolving all shells before running
/// any hook is the REQ-006 contract: an unresolved shell aborts the apply
/// before any file operation or hook command executes.
pub fn resolve_shells(
    hooks: &[HookEntry],
    host_os: HostOs,
) -> Result<Vec<ResolvedHook<'_>>, HookError> {
    hooks
        .iter()
        .map(|entry| {
            let shell = match &entry.shell {
                None => default_shell(host_os).to_owned(),
                Some(explicit) => {
                    if resolve_on_path(explicit).is_none() {
                        return Err(HookError::ShellNotFound {
                            shell: explicit.clone(),
                        });
                    }
                    explicit.clone()
                }
            };
            Ok(ResolvedHook { entry, shell })
        })
        .collect()
}

/// Whether a hook should run, given its optional `when` predicate.
///
/// A hook with no `when` always runs. A hook with a `when` runs iff the
/// predicate evaluates truthy against `resolver` through `engine`.
///
/// # Errors
///
/// Returns [`HookError::When`] when the predicate references an undefined
/// variable under strict-undefined semantics or fails to compile /
/// evaluate.
pub fn should_run(
    hook: &ResolvedHook<'_>,
    engine: &Engine,
    resolver: &Resolver,
) -> Result<bool, HookError> {
    match &hook.entry.when {
        None => Ok(true),
        Some(expr) => engine
            .eval_when(expr, resolver)
            .map_err(|source| HookError::When { source }),
    }
}

/// Run a resolved hook and classify its exit status under `force_deploy`.
///
/// Spawns the resolved shell against the hook command (`<shell> -c
/// <command>` on Unix shells, `<shell> -Command <command>` on PowerShell)
/// and awaits the child. The exit status maps to a [`HookOutcome`]:
/// zero is [`HookOutcome::Succeeded`]; non-zero is [`HookOutcome::Failed`]
/// when `must_succeed` is effectively enforced and [`HookOutcome::Warned`]
/// otherwise.
///
/// # Errors
///
/// Returns [`HookError::Spawn`] when the child cannot be spawned or waited
/// on at the OS level. A non-zero *exit code* is not an error — it is a
/// successful classification into [`HookOutcome::Warned`] /
/// [`HookOutcome::Failed`] so the orchestrator can act on it.
pub async fn run_hook(
    hook: &ResolvedHook<'_>,
    force_deploy: ForceDeploy,
) -> Result<HookOutcome, HookError> {
    let command = &hook.entry.command;
    let status = build_command(&hook.shell, command)
        .status()
        .await
        .map_err(|source| HookError::Spawn {
            command: command.clone(),
            source,
        })?;

    if status.success() {
        return Ok(HookOutcome::Succeeded);
    }
    if force_deploy.enforces(hook.entry.must_succeed) {
        Ok(HookOutcome::Failed)
    } else {
        Ok(HookOutcome::Warned)
    }
}

/// Default shell binary name for `bash`-family Unix hosts (macOS, Linux).
const DEFAULT_UNIX_SHELL: &str = "bash";
/// Default shell binary name for Windows hosts (PowerShell 7+).
const DEFAULT_WINDOWS_SHELL: &str = "pwsh";

/// Platform default shell binary name for the resolved host.
fn default_shell(host_os: HostOs) -> &'static str {
    match host_os {
        HostOs::Windows => DEFAULT_WINDOWS_SHELL,
        HostOs::Linux | HostOs::MacOs => DEFAULT_UNIX_SHELL,
    }
}

/// Build the [`Command`] that runs `command` through `shell`.
///
/// PowerShell uses `-Command`; every other (POSIX) shell uses `-c`. The
/// distinction is made on the shell's file stem so an explicit
/// `/usr/local/bin/pwsh` or a bare `pwsh` both take the PowerShell flag.
fn build_command(shell: &str, command: &str) -> Command {
    let mut cmd = Command::new(shell);
    if is_powershell(shell) {
        cmd.arg("-Command");
    } else {
        cmd.arg("-c");
    }
    cmd.arg(command);
    cmd
}

/// Whether `shell` names a PowerShell binary (`pwsh` or `powershell`),
/// matching on the file stem so an absolute path resolves the same way.
fn is_powershell(shell: &str) -> bool {
    let stem = Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(shell);
    stem.eq_ignore_ascii_case("pwsh") || stem.eq_ignore_ascii_case("powershell")
}

/// Resolve `binary` to a concrete executable path on the current `PATH`,
/// returning `None` when it is not found.
///
/// A `binary` that already contains a path separator is checked directly
/// rather than walked across `PATH`. On Windows the `PATHEXT` extensions
/// (`.EXE`, `.CMD`, …) are appended to each candidate so a bare `pwsh`
/// resolves to `pwsh.exe`.
#[must_use = "the resolved path is the result of the PATH lookup; use it"]
pub fn resolve_on_path(binary: &str) -> Option<Utf8PathBuf> {
    if binary.contains('/') || binary.contains('\\') {
        let candidate = Utf8PathBuf::from(binary);
        return is_executable_file(&candidate).then_some(candidate);
    }

    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let dir = Utf8PathBuf::from_path_buf(dir).ok()?;
        for candidate in path_candidates(&dir, binary) {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Candidate file names for `binary` inside `dir`. On Windows this fans
/// out across the `PATHEXT` extensions (plus the bare name); elsewhere it
/// is just `dir/binary`.
#[cfg(windows)]
fn path_candidates(dir: &camino::Utf8Path, binary: &str) -> Vec<Utf8PathBuf> {
    let mut out = vec![dir.join(binary)];
    let already_has_ext = Path::new(binary).extension().is_some();
    if !already_has_ext {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            out.push(dir.join(format!("{binary}{ext}")));
        }
    }
    out
}

/// Candidate file names for `binary` inside `dir` on Unix: a single
/// `dir/binary`.
#[cfg(not(windows))]
fn path_candidates(dir: &camino::Utf8Path, binary: &str) -> Vec<Utf8PathBuf> {
    vec![dir.join(binary)]
}

/// Whether `path` is an existing regular file (the PATH-resolution
/// existence check). The OS enforces the execute bit at spawn time; a
/// regular file on `PATH` is the resolution contract this check gates.
fn is_executable_file(path: &camino::Utf8Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookEvent;
    use crate::variables::Builtins;

    fn hook(event: HookEvent, command: &str) -> HookEntry {
        HookEntry {
            event,
            command: command.to_owned(),
            shell: None,
            when: None,
            must_succeed: true,
        }
    }

    fn resolver() -> Resolver {
        Resolver::new(Builtins::for_tests())
    }

    fn host_os() -> HostOs {
        HostOs::current()
    }

    /// A shell guaranteed present on the test host for execution tests:
    /// the platform default. If the default is missing the test host is
    /// misconfigured for running hooks at all.
    fn host_default_shell() -> &'static str {
        default_shell(HostOs::current())
    }

    #[test]
    fn omitted_shell_defaults_per_platform() {
        let hooks = vec![hook(HookEvent::PreApply, "echo hi")];
        let resolved = resolve_shells(&hooks, HostOs::Linux).expect("resolve");
        assert_eq!(resolved.first().expect("one resolved hook").shell, "bash");
        let resolved = resolve_shells(&hooks, HostOs::MacOs).expect("resolve");
        assert_eq!(resolved.first().expect("one resolved hook").shell, "bash");
        let resolved = resolve_shells(&hooks, HostOs::Windows).expect("resolve");
        assert_eq!(resolved.first().expect("one resolved hook").shell, "pwsh");
    }

    #[test]
    fn explicit_unresolved_shell_errors_before_running() {
        let mut entry = hook(HookEvent::PreApply, "echo hi");
        entry.shell = Some("nonexistent-shell-xyz".to_owned());
        let hooks = vec![entry];
        let err = resolve_shells(&hooks, host_os()).expect_err("unresolved shell must error");
        assert!(
            matches!(&err, HookError::ShellNotFound { shell } if shell == "nonexistent-shell-xyz"),
            "expected ShellNotFound naming the binary, got {err:?}"
        );
    }

    #[test]
    fn unresolved_shell_aborts_whole_set_before_any_resolution() {
        // The first hook resolves fine; the second names a missing shell.
        // `resolve_shells` must surface the error rather than return a
        // partial set, so the orchestrator aborts before running any hook.
        let mut bad = hook(HookEvent::PreApply, "echo hi");
        bad.shell = Some("definitely-not-a-real-shell-9000".to_owned());
        let hooks = vec![hook(HookEvent::PreApply, "echo ok"), bad];
        let err = resolve_shells(&hooks, host_os()).expect_err("must error on the bad shell");
        assert!(matches!(err, HookError::ShellNotFound { .. }));
    }

    #[test]
    fn should_run_true_when_no_predicate() {
        let hooks = vec![hook(HookEvent::PreApply, "echo hi")];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        assert!(
            should_run(
                resolved.first().expect("one resolved hook"),
                &Engine::new(),
                &resolver()
            )
            .expect("eval")
        );
    }

    #[test]
    fn should_run_filters_on_false_predicate() {
        // `patina.os` is the test host's OS; assert against a value it
        // cannot equal so the predicate is deterministically false.
        let r = resolver();
        let os = r.get("patina.os").expect("os resolves");
        let other = if os == "macos" { "linux" } else { "macos" };
        let mut entry = hook(HookEvent::PreApply, "echo hi");
        entry.when = Some(format!("patina.os == '{other}'"));
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        assert!(
            !should_run(
                resolved.first().expect("one resolved hook"),
                &Engine::new(),
                &r
            )
            .expect("eval")
        );
    }

    #[test]
    fn should_run_true_when_predicate_matches_builtin() {
        let r = resolver();
        let os = r.get("patina.os").expect("os resolves");
        let mut entry = hook(HookEvent::PreApply, "echo hi");
        entry.when = Some(format!("patina.os == '{os}'"));
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        assert!(
            should_run(
                resolved.first().expect("one resolved hook"),
                &Engine::new(),
                &r
            )
            .expect("eval")
        );
    }

    #[test]
    fn should_run_surfaces_undefined_variable_error() {
        let r = resolver();
        let os = r.get("patina.os").expect("os resolves");
        // Force the predicate to reach the undefined operand on any host.
        let mut entry = hook(HookEvent::PreApply, "echo hi");
        entry.when = Some(format!("patina.os == '{os}' and missing_hook_var"));
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        let err = should_run(
            resolved.first().expect("one resolved hook"),
            &Engine::new(),
            &r,
        )
        .expect_err("undefined must error");
        assert!(matches!(err, HookError::When { .. }));
    }

    #[tokio::test]
    async fn zero_exit_succeeds() {
        let mut entry = hook(HookEvent::PreApply, "exit 0");
        entry.shell = Some(host_default_shell().to_owned());
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        let outcome = run_hook(
            resolved.first().expect("one resolved hook"),
            ForceDeploy::No,
        )
        .await
        .expect("run");
        assert_eq!(outcome, HookOutcome::Succeeded);
    }

    #[tokio::test]
    async fn nonzero_exit_with_must_succeed_fails() {
        let mut entry = hook(HookEvent::PreApply, "exit 1");
        entry.shell = Some(host_default_shell().to_owned());
        entry.must_succeed = true;
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        let outcome = run_hook(
            resolved.first().expect("one resolved hook"),
            ForceDeploy::No,
        )
        .await
        .expect("run");
        assert_eq!(outcome, HookOutcome::Failed);
    }

    #[tokio::test]
    async fn nonzero_exit_without_must_succeed_warns() {
        let mut entry = hook(HookEvent::PreApply, "exit 1");
        entry.shell = Some(host_default_shell().to_owned());
        entry.must_succeed = false;
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        let outcome = run_hook(
            resolved.first().expect("one resolved hook"),
            ForceDeploy::No,
        )
        .await
        .expect("run");
        assert_eq!(outcome, HookOutcome::Warned);
    }

    #[tokio::test]
    async fn force_deploy_downgrades_must_succeed_failure_to_warning() {
        let mut entry = hook(HookEvent::PostApply, "exit 1");
        entry.shell = Some(host_default_shell().to_owned());
        entry.must_succeed = true;
        let hooks = vec![entry];
        let resolved = resolve_shells(&hooks, host_os()).expect("resolve");
        let outcome = run_hook(
            resolved.first().expect("one resolved hook"),
            ForceDeploy::Yes,
        )
        .await
        .expect("run");
        assert_eq!(outcome, HookOutcome::Warned);
    }

    #[test]
    fn force_deploy_enforces_logic() {
        assert!(ForceDeploy::No.enforces(true));
        assert!(!ForceDeploy::No.enforces(false));
        assert!(!ForceDeploy::Yes.enforces(true));
        assert!(!ForceDeploy::Yes.enforces(false));
    }

    #[test]
    fn powershell_detection_matches_stem_and_path() {
        assert!(is_powershell("pwsh"));
        assert!(is_powershell("PowerShell"));
        assert!(is_powershell("/usr/local/bin/pwsh"));
        assert!(!is_powershell("bash"));
        assert!(!is_powershell("/bin/sh"));
    }

    #[test]
    fn resolve_on_path_finds_the_default_shell() {
        // The platform default shell must resolve on PATH on any host that
        // can run hooks at all; this exercises the PATH walk end-to-end.
        assert!(
            resolve_on_path(host_default_shell()).is_some(),
            "default shell {} should resolve on PATH",
            host_default_shell()
        );
    }

    #[test]
    fn resolve_on_path_rejects_missing_binary() {
        assert!(resolve_on_path("definitely-not-on-path-zzz-9999").is_none());
    }
}
