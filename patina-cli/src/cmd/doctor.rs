//! `patina doctor` read-only environment inspection.
//!
//! `patina doctor` inspects the per-machine state directory, the resolved
//! repository path, the running OS, and the declared file modes in the
//! repository, then emits the v1.0 finding set. Cloud-sync detection is out
//! of scope.
//!
//! The read-only path (no `--fix`) acquires only the shared advisory lock,
//! with the read-only escape hatch: a [`SHARED_TIMEOUT`] expiry warns and
//! proceeds rather than blocking the user.
//!
//! The `--fix` path is mutating. It acquires the exclusive lock, then prompts
//! per fixable finding (Developer Mode missing on Windows, a missing
//! `default_repo` pointer) and remediates on accept. Every remediation emits
//! a structured `tracing` event naming the finding code, the remediation, and
//! the outcome.
//!
//! Exit codes: 0 when only warning/info findings were raised; 1 on an
//! error-level finding.
//!
//! Output: human findings to stderr, `--json` emits a single
//! deterministic document on stdout (no timestamps / PIDs / random ids), so
//! two runs against unchanged state are byte-identical.

use crate::cli::DoctorArgs;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::Styles;
use crate::output::style::paint;
use crate::output::table::align;
use crate::output::table::row;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::DEV_MODE_REGISTRY_PATH;
use patina_core::DevModeStatus;
use patina_core::EngineError;
use patina_core::FileMode;
use patina_core::LockError;
use patina_core::LockKind;
use patina_core::OrphanReason;
use patina_core::SHARED_TIMEOUT;
use patina_core::acquire_lock;
use patina_core::dev_mode_status;
use patina_core::discover_modules;
use patina_core::exclusive_timeout;
use patina_core::is_unc_path;
use patina_core::journal_orphans;
use patina_core::parse_module_config;
use patina_core::parse_root_config;
use patina_core::persisted_default_present;
use patina_core::resolve_repository_root;
use patina_core::resolve_state_dir;
use patina_core::validate_repo_root;
use patina_core::windows_build_supports_dev_mode;
use patina_core::write_persisted_default;

/// A single doctor finding. Carries a stable [`FindingCode`], a
/// [`Level`], a human message, and the path the finding concerns when one
/// applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The stable code identifying the kind of finding.
    pub code: FindingCode,
    /// The severity level.
    pub level: Level,
    /// The human-readable message (stderr in human mode, `message` field in
    /// the JSON document).
    pub message: String,
    /// The path the finding concerns, when one applies (e.g. the resolved
    /// repository path for the UNC finding); `None` for findings with no
    /// associated path.
    pub path: Option<Utf8PathBuf>,
}

/// The stable code identifying a doctor finding. The string label
/// ([`FindingCode::label`]) is part of the JSON contract and the
/// human output, so it is defined once on the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingCode {
    /// On Windows, the resolved repository path is a UNC path.
    WinUnc,
    /// On Windows, the repository declares a symlink `[[file]]` and Developer
    /// Mode is disabled.
    WinDevMode,
    /// On Windows, the running OS build predates Windows 10 1703.
    WinOsOld,
    /// No `default_repo` pointer exists in the state directory.
    NoDefaultRepo,
    /// The repository declares a remote-backed module but no `git` binary
    /// resolves on `PATH`.
    NoGit,
    /// A prior apply materialized targets that the current `ignore` lists now
    /// exclude, so the next apply will reap them.
    IgnoredDeployed,
}

impl FindingCode {
    /// The stable string label for this code, used in both the JSON document
    /// and the human output.
    #[must_use = "the label is part of the JSON and human output contract"]
    pub fn label(self) -> &'static str {
        match self {
            FindingCode::WinUnc => "DOC-WIN-UNC",
            FindingCode::WinDevMode => "DOC-WIN-DEVMODE",
            FindingCode::WinOsOld => "DOC-WIN-OSOLD",
            FindingCode::NoDefaultRepo => "DOC-NO-DEFAULT-REPO",
            FindingCode::NoGit => "DOC-NO-GIT",
            FindingCode::IgnoredDeployed => "DOC-IGNORED-DEPLOYED",
        }
    }
}

/// A finding's severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Advisory note; never affects the exit code.
    Info,
    /// A warning; the command still exits 0.
    Warning,
    /// An error; the command exits 1.
    Error,
}

impl Level {
    /// The stable lowercase label for this level, used in the JSON document
    /// and the human output.
    #[must_use = "the label is part of the JSON and human output contract"]
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

/// The host-state inputs [`compute_findings`] reads, gathered by
/// [`gather_inputs`].
///
/// A test fills these fields directly, including the Windows-specific reads
/// (Developer Mode status, OS-build support), so the whole finding set is
/// unit-testable on any platform with no real registry in the loop.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent host-state fact gathered from a distinct source (the platform, the repository's declared modes, the OS-build query, the state-directory pointer), not a state machine that would be better modelled as an enum."
)]
pub struct Inputs {
    /// Whether the running host is Windows. Off Windows, no `DOC-WIN-*`
    /// finding fires.
    pub is_windows: bool,
    /// The Developer Mode registry status (from [`dev_mode_status`]).
    pub dev_mode: DevModeStatus,
    /// Whether the running OS build supports Developer Mode (Windows 10 1703+).
    pub build_supports_dev_mode: bool,
    /// The resolved repository path, when discovery succeeded. `None` when no
    /// repository could be resolved (the UNC finding then cannot apply).
    pub repo_root: Option<Utf8PathBuf>,
    /// Whether the resolved repository declares at least one `symlink` /
    /// `symlink-dir` `[[file]]` entry.
    pub repo_declares_symlink: bool,
    /// Whether the `default_repo` pointer exists in the state directory.
    pub default_repo_present: bool,
    /// Whether the resolved repository declares at least one remote-backed
    /// module. A repository with no remotes never shells out to git, so the
    /// git finding cannot fire.
    pub repo_declares_remote: bool,
    /// Whether a `git` binary resolves on `PATH`.
    pub git_available: bool,
    /// Targets a prior apply materialized that the current ignore lists now
    /// exclude, sorted by path. Empty when there is no prior commit, when
    /// nothing is ignored, or when the query failed.
    pub ignored_deployed: Vec<Utf8PathBuf>,
}

/// Run `patina doctor`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error (exit 1) when the per-machine state directory cannot be
/// resolved, or when a `--fix` remediation fails: the persisted-default
/// write, or the Windows helper running but leaving the flag off. On the
/// `--fix` path an exclusive-lock timeout maps to exit 4 via the engine-error
/// chain.
///
/// Repository-discovery and manifest-parse failures are never fatal, and a
/// shared-lock timeout is downgraded to a stderr warning.
pub fn run(
    args: &DoctorArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let state = resolve_state_dir().map_err(EngineError::from)?;

    if args.fix {
        run_fix(args, &state, tty, reader, reporter)
    } else {
        run_report(args, &state, reporter)
    }
}

/// The read-only diagnostic path (no `--fix`).
///
/// Acquires only the shared lock, with the read-only escape hatch: a timeout
/// warns and proceeds rather than blocking the user behind a concurrent
/// mutating apply.
fn run_report(args: &DoctorArgs, state: &Utf8Path, reporter: &mut impl Reporter) -> Result<i32> {
    let lock_path = state.join("lock");
    let _guard = match acquire_lock(&lock_path, LockKind::Shared, SHARED_TIMEOUT) {
        Ok(guard) => Some(guard),
        Err(LockError::Timeout { path, waited, .. }) => {
            reporter.warn(&format!(
                "could not acquire the shared lock on `{path}` within {waited:?}; \
                 proceeding with doctor without it"
            ));
            None
        }
        Err(other) => return Err(EngineError::Lock(other).into()),
    };

    let inputs = gather_inputs(state);
    let findings = compute_findings(&inputs);

    if args.json {
        reporter.json(&json_envelope(&findings));
    } else {
        render_human(&findings, reporter);
    }
    Ok(exit_code(&findings).code())
}

/// The interactive remediation path (`--fix`).
///
/// A non-TTY `--fix` without `--yes` cannot prompt, so it refuses up front
/// with exit 1 before taking any lock or mutating anything. With a
/// TTY (or `--yes`) it acquires the exclusive lock, recomputes the
/// findings under the lock, then walks each fixable finding: prompt (or
/// auto-accept under `--yes`) and remediate on accept. Non-fixable findings
/// still surface as warnings. Each remediation that runs emits a structured
/// `tracing` event.
fn run_fix(
    args: &DoctorArgs,
    state: &Utf8Path,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    if !args.yes && tty == Tty::NonInteractive {
        reporter.warn(
            "`patina doctor --fix` cannot prompt in a non-TTY shell; \
             pass --yes to accept every remediation automatically",
        );
        return Ok(ExitCode::Generic.code());
    }

    // A contention timeout reaches the exit-4 mapping through the
    // engine-error chain.
    let lock_path = state.join("lock");
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;

    // Recompute findings under the lock so the remediation acts on the state
    // no concurrent mutator can be racing.
    let inputs = gather_inputs(state);
    let findings = compute_findings(&inputs);

    for finding in &findings {
        match finding.code {
            FindingCode::NoDefaultRepo => {
                fix_default_repo(args, state, tty, reader, reporter)?;
            }
            FindingCode::WinDevMode => {
                fix_dev_mode(args, tty, reader, reporter)?;
            }
            // Non-fixable findings: surface the warning, name why Patina
            // cannot remedy them, and move on.
            FindingCode::WinUnc
            | FindingCode::WinOsOld
            | FindingCode::NoGit
            | FindingCode::IgnoredDeployed => {
                reporter.warn(&format!(
                    "[{}] {} is not auto-fixable: {}",
                    finding.level.label(),
                    finding.code.label(),
                    finding.message
                ));
            }
        }
    }

    if findings.is_empty() {
        reporter.line("doctor --fix: no findings; nothing to remediate.");
    }
    Ok(ExitCode::Success.code())
}

/// Remediate the `DOC-NO-DEFAULT-REPO` finding by writing the current working
/// directory's canonical absolute path as the persisted default.
///
/// The CWD is validated as a repository root (an existing directory holding a
/// `patina.toml` with `[patina].root = true`) via
/// [`patina_core::validate_repo_root`], the same predicate repository
/// discovery uses. A non-repository CWD, or a canonicalization failure, is a
/// hard error (exit 1).
fn fix_default_repo(
    args: &DoctorArgs,
    state: &Utf8Path,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<()> {
    if !confirm(
        args,
        tty,
        reader,
        reporter,
        "Record the current directory as the default repository?",
    ) {
        return Ok(());
    }

    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let cwd = Utf8PathBuf::from_path_buf(cwd)
        .map_err(|p| anyhow::anyhow!("current directory `{}` is not valid UTF-8", p.display()))?;
    let canonical = validate_repo_root(&cwd).map_err(|reason| {
        anyhow::anyhow!("current directory {cwd} is not a valid Patina repository: {reason}")
    })?;
    write_persisted_default(state, &canonical).map_err(EngineError::from)?;

    tracing::info!(
        finding = FindingCode::NoDefaultRepo.label(),
        remediation = "write_default_repo",
        outcome = "written",
        repo = %canonical,
        "doctor --fix wrote the persisted default repository pointer",
    );
    reporter.line(&format!("Recorded {canonical} as the default repository."));
    Ok(())
}

/// Remediate the `DOC-WIN-DEVMODE` finding by driving the one-time UAC
/// elevation flow and re-checking the registry afterward.
#[cfg(windows)]
fn fix_dev_mode(
    args: &DoctorArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<()> {
    if !confirm(
        args,
        tty,
        reader,
        reporter,
        "Enable Developer Mode via a one-time UAC elevation?",
    ) {
        return Ok(());
    }

    reporter.line("Requesting one-time elevation to enable Developer Mode…");
    match patina_core::launch_elevate_helper().context("failed to launch the elevation helper")? {
        patina_core::ElevationOutcome::EnabledNow => {
            tracing::info!(
                finding = FindingCode::WinDevMode.label(),
                remediation = "elevate_dev_mode",
                outcome = "enabled",
                "doctor --fix enabled Developer Mode via the UAC helper",
            );
            reporter.line("Developer Mode is now enabled.");
            Ok(())
        }
        patina_core::ElevationOutcome::Declined => {
            tracing::info!(
                finding = FindingCode::WinDevMode.label(),
                remediation = "elevate_dev_mode",
                outcome = "declined",
                "doctor --fix elevation declined; Developer Mode left disabled",
            );
            reporter.warn(
                "Developer Mode was not enabled (elevation declined); \
                 re-run `patina doctor --fix` to try again.",
            );
            Ok(())
        }
        patina_core::ElevationOutcome::RanButStillDisabled => Err(anyhow::anyhow!(
            "the elevation helper ran but Developer Mode is still disabled; \
             the registry value {DEV_MODE_REGISTRY_PATH} did not change to 1"
        )),
    }
}

/// Non-Windows stub: [`compute_findings`] gates every `DOC-WIN-*` finding to
/// `is_windows`, so this arm is unreachable in practice. It exists only so
/// the `--fix` match compiles without a `#[cfg]` at the call site.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature parity with the fallible #[cfg(windows)] variant"
)]
fn fix_dev_mode(
    _args: &DoctorArgs,
    _tty: Tty,
    _reader: &mut impl PromptReader,
    _reporter: &mut impl Reporter,
) -> Result<()> {
    Ok(())
}

/// Decide whether a fixable finding's remediation should run: `--yes` accepts
/// unconditionally; a TTY prompts and reads the answer; a non-TTY without
/// `--yes` never reaches here (`run_fix` refuses up front), so the
/// `NonInteractive` arm conservatively declines.
fn confirm(
    args: &DoctorArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
    question: &str,
) -> bool {
    match (args.yes, tty) {
        (true, _) => true,
        (false, Tty::NonInteractive) => false,
        (false, Tty::Interactive) => {
            reporter.confirm(question);
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    }
}

/// Gather the host-state [`Inputs`].
///
/// Repository discovery is best-effort: a failure to resolve the repository
/// (no `patina.toml`, no persisted default) yields `repo_root = None` and
/// `repo_declares_symlink = false`, so doctor still reports the
/// state-directory findings rather than aborting.
fn gather_inputs(state: &Utf8Path) -> Inputs {
    let repo_root = resolve_repository_root().ok();
    let repo_declares_symlink = repo_root
        .as_deref()
        .is_some_and(repository_declares_symlink);
    let repo_declares_remote = repo_root.as_deref().is_some_and(repository_declares_remote);
    Inputs {
        is_windows: cfg!(windows),
        dev_mode: dev_mode_status(),
        build_supports_dev_mode: windows_build_supports_dev_mode(),
        repo_root,
        repo_declares_symlink,
        default_repo_present: persisted_default_present(state),
        repo_declares_remote,
        git_available: patina_core::git_available(),
        ignored_deployed: deployed_but_now_ignored(state),
    }
}

/// Targets the last committed apply materialized that the current ignore lists
/// now exclude.
///
/// Read-only and best-effort. A repository that cannot be planned, a state
/// directory with no commit, or a manifest that fails to parse all yield an
/// empty list rather than an error. The next `apply` reaps the same set behind
/// its diff-and-prompt, so this finding only warns.
fn deployed_but_now_ignored(state: &Utf8Path) -> Vec<Utf8PathBuf> {
    // The reap set filtered by reason, not a second query over
    // `managed.ignored`: the reap also skips a target already gone from disk, a
    // directory, and one a second entry still governs.
    let Ok(orphans) = journal_orphans(&state.join("journal")) else {
        return Vec::new();
    };
    let mut found: Vec<Utf8PathBuf> = orphans
        .into_iter()
        .filter(|orphan| matches!(orphan.reason, OrphanReason::Ignored))
        .map(|orphan| orphan.target)
        .collect();
    found.dedup();
    found
}

/// Whether `repo_root`'s manifest declares any `[[remote]]`. A root manifest
/// that fails to parse yields `false`.
fn repository_declares_remote(repo_root: &Utf8Path) -> bool {
    let manifest = repo_root.join(crate::cmd::MANIFEST_FILENAME);
    parse_root_config(&manifest).is_ok_and(|config| !config.remotes.is_empty())
}

/// Whether `repo_root`'s modules declare any `symlink` / `symlink-dir`
/// `[[file]]` entry. A module whose manifest fails to parse is skipped, since
/// nothing in it confirms a symlink declaration; a discovery failure yields
/// `false`.
fn repository_declares_symlink(repo_root: &Utf8Path) -> bool {
    let Ok(modules) = discover_modules(repo_root) else {
        return false;
    };
    modules.iter().any(|module| {
        let manifest = module.path.join(crate::cmd::MANIFEST_FILENAME);
        parse_module_config(&manifest).is_ok_and(|config| {
            config
                .files
                .iter()
                .any(|entry| matches!(entry.mode, FileMode::Symlink | FileMode::SymlinkDir))
        })
    })
}

/// Compute the finding set from [`Inputs`]. Pure over its argument: no
/// filesystem, registry, or environment access, so the whole v1.0 finding set
/// is unit-testable on any platform.
///
/// The push order below is fixed, so the rendered output is deterministic.
#[must_use = "the computed findings drive the output and exit code"]
pub fn compute_findings(inputs: &Inputs) -> Vec<Finding> {
    let mut findings = Vec::new();

    if inputs.is_windows {
        if let Some(repo_root) = inputs.repo_root.as_deref()
            && is_unc_path(repo_root)
        {
            findings.push(Finding {
                code: FindingCode::WinUnc,
                level: Level::Warning,
                message: format!(
                    "the resolved repository path {repo_root} is a UNC path; \
                     UNC paths cannot host symbolic links, so symlink targets \
                     will fail to materialize."
                ),
                path: Some(repo_root.to_path_buf()),
            });
        }

        if inputs.repo_declares_symlink && inputs.dev_mode == DevModeStatus::Disabled {
            findings.push(Finding {
                code: FindingCode::WinDevMode,
                level: Level::Warning,
                message: format!(
                    "the repository declares symbolic-link entries but Developer \
                     Mode is not enabled; enable it so patina can create symbolic \
                     links without elevation. Registry flag: {DEV_MODE_REGISTRY_PATH}"
                ),
                path: None,
            });
        }

        if !inputs.build_supports_dev_mode {
            findings.push(Finding {
                code: FindingCode::WinOsOld,
                level: Level::Warning,
                message: "the running Windows build predates Windows 10 1703, the \
                          first build to support Developer Mode symbolic-link \
                          creation."
                    .to_owned(),
                path: None,
            });
        }
    }

    if !inputs.default_repo_present {
        // The advice must be actionable for the state it fires in: when a
        // repository already resolves (env var or walk-up), `patina init`
        // refuses on the existing manifest, so point at `doctor --fix`, which
        // records the pointer for an existing repository. The message also
        // says why the pointer matters: this invocation found the repository
        // through its own working directory or PATINA_REPO, and invocations
        // with neither (the background watch service in particular) fall back
        // to the recorded default.
        let message = match inputs.repo_root.as_deref() {
            Some(repo_root) => format!(
                "no default repository is recorded in the state directory; \
                 {repo_root} was resolved from this invocation's working \
                 directory or PATINA_REPO, so `patina` run without either \
                 (including the background watch service) will not find it. \
                 Run `patina doctor --fix` from {repo_root} to record it."
            ),
            None => "no default repository is recorded in the state directory; \
                     run `patina init` to set one."
                .to_owned(),
        };
        findings.push(Finding {
            code: FindingCode::NoDefaultRepo,
            level: Level::Info,
            message,
            path: None,
        });
    }

    if inputs.repo_declares_remote && !inputs.git_available {
        findings.push(Finding {
            code: FindingCode::NoGit,
            level: Level::Warning,
            message: "the repository declares remote-backed modules but no `git` binary \
                      resolves on PATH; patina fetches remote sources by shelling out to \
                      git, so `apply` cannot materialize a pin that is not already cached."
                .to_owned(),
            path: None,
        });
    }

    if let Some(first) = inputs.ignored_deployed.first() {
        let count = inputs.ignored_deployed.len();
        let noun = if count == 1 { "target" } else { "targets" };
        findings.push(Finding {
            code: FindingCode::IgnoredDeployed,
            level: Level::Warning,
            message: format!(
                "a prior apply materialized {count} {noun} that an `ignore` list now \
                 excludes; the next `patina apply` reaps them and names `ignored` \
                 as the reason."
            ),
            path: Some(first.clone()),
        });
    }

    findings
}

/// 1 when any finding is error-level, otherwise 0. No v1.0 finding is
/// error-level, so the error branch never fires today; it reserves the exit-1
/// path for a future addition.
fn exit_code(findings: &[Finding]) -> ExitCode {
    if findings.iter().any(|f| f.level == Level::Error) {
        ExitCode::Generic
    } else {
        ExitCode::Success
    }
}

/// Build the `--json` envelope: a single object with a `findings` array of
/// `{code, level, message, path?}` objects. Deterministic for a given input
/// (no timestamps / PIDs), so two runs against unchanged state are
/// byte-identical.
fn json_envelope(findings: &[Finding]) -> String {
    let array: Vec<serde_json::Value> = findings
        .iter()
        .map(|finding| {
            let mut object = serde_json::Map::new();
            object.insert("code".to_owned(), finding.code.label().into());
            object.insert("level".to_owned(), finding.level.label().into());
            object.insert("message".to_owned(), finding.message.clone().into());
            if let Some(path) = &finding.path {
                object.insert("path".to_owned(), path.as_str().into());
            }
            serde_json::Value::Object(object)
        })
        .collect();
    let envelope = serde_json::json!({ "findings": array });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Render the findings to stderr as one aligned row each (all findings go to
/// stderr regardless of format). A clean environment prints a single "no
/// findings" line so the user gets explicit confirmation.
///
/// The block goes out through [`Reporter::err_block`]. One
/// [`Reporter::warn`] per line would paint every level the same yellow,
/// because `warn` forces a single style over a whole line. The bracketed level
/// stays in the first cell, so a stripped report still tells an advisory note
/// from an error.
fn render_human(findings: &[Finding], reporter: &mut impl Reporter) {
    if findings.is_empty() {
        reporter.line("doctor: no findings; the environment looks healthy.");
        return;
    }
    let styles = reporter.styles();
    // Level and code share the level's color, so severity reads off the whole
    // left edge.
    let table: String = findings
        .iter()
        .map(|finding| {
            let style = level_style(finding.level, &styles);
            row(&[
                paint(style, &format!("[{}]", finding.level.label())).as_str(),
                paint(style, finding.code.label()).as_str(),
                finding.message.as_str(),
            ])
        })
        .collect();
    reporter.err_block(&align(&table));
}

/// The palette role for a finding's level.
fn level_style(level: Level, styles: &Styles) -> anstyle::Style {
    match level {
        Level::Info => styles.finding.info,
        Level::Warning => styles.finding.warning,
        Level::Error => styles.finding.error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use crate::output::reporter::assert_color_is_additive;

    /// A scripted prompt reader yielding a fixed sequence of lines.
    struct ScriptedReader {
        lines: std::collections::VecDeque<String>,
    }

    impl ScriptedReader {
        fn new(lines: &[&str]) -> Self {
            Self {
                lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            }
        }
    }

    impl PromptReader for ScriptedReader {
        fn read_line(&mut self) -> Option<String> {
            self.lines.pop_front()
        }
    }

    fn fix_args(yes: bool) -> DoctorArgs {
        DoctorArgs {
            fix: true,
            json: false,
            yes,
        }
    }

    #[test]
    fn confirm_yes_proceeds_without_reading() {
        // --yes accepts unconditionally and never consults the reader.
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            &fix_args(true),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
            "Proceed?",
        ));
    }

    #[test]
    fn confirm_non_tty_without_yes_declines() {
        // The NonInteractive arm declines. `run_fix` refuses before this
        // point, so the conservative default never auto-remediates.
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        assert!(!confirm(
            &fix_args(false),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
            "Proceed?",
        ));
    }

    #[test]
    fn confirm_tty_reads_the_answer() {
        let mut reporter = BufferReporter::new();
        let mut yes_reader = ScriptedReader::new(&["y\n"]);
        assert!(confirm(
            &fix_args(false),
            Tty::Interactive,
            &mut yes_reader,
            &mut reporter,
            "Proceed?",
        ));

        let mut no_reader = ScriptedReader::new(&["n\n"]);
        assert!(!confirm(
            &fix_args(false),
            Tty::Interactive,
            &mut no_reader,
            &mut reporter,
            "Proceed?",
        ));
    }

    #[test]
    fn fix_in_non_tty_without_yes_refuses_exit_one() {
        // A non-TTY --fix without --yes cannot prompt, so it refuses with
        // exit 1 naming the missing --yes flag, before any lock or mutation.
        // The refusal returns first, so the state path is never touched.
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let code = run_fix(
            &fix_args(false),
            Utf8Path::new("/nonexistent/state"),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        )
        .expect("the non-TTY refusal is a clean exit, not an error");
        assert_eq!(code, ExitCode::Generic.code());
        assert!(
            reporter.err.contains("--yes"),
            "the refusal must name --yes, got: {}",
            reporter.err
        );
    }

    fn base_inputs() -> Inputs {
        Inputs {
            is_windows: false,
            dev_mode: DevModeStatus::NotWindows,
            build_supports_dev_mode: false,
            repo_root: Some(Utf8PathBuf::from("/home/u/dotfiles")),
            repo_declares_symlink: false,
            default_repo_present: true,
            repo_declares_remote: false,
            git_available: true,
            ignored_deployed: Vec::new(),
        }
    }

    fn codes(findings: &[Finding]) -> Vec<FindingCode> {
        findings.iter().map(|f| f.code).collect()
    }

    #[test]
    fn missing_git_fires_only_when_a_remote_is_declared() {
        let no_remotes = Inputs {
            repo_declares_remote: false,
            git_available: false,
            ..base_inputs()
        };
        assert!(
            !codes(&compute_findings(&no_remotes)).contains(&FindingCode::NoGit),
            "a repository with no remotes must not be warned about git"
        );

        let with_remotes = Inputs {
            repo_declares_remote: true,
            git_available: false,
            ..base_inputs()
        };
        assert_eq!(
            codes(&compute_findings(&with_remotes)),
            vec![FindingCode::NoGit]
        );
    }

    #[test]
    fn a_declared_remote_with_git_present_raises_nothing() {
        let inputs = Inputs {
            repo_declares_remote: true,
            git_available: true,
            ..base_inputs()
        };
        assert!(compute_findings(&inputs).is_empty());
    }

    #[test]
    fn clean_non_windows_env_yields_no_findings() {
        let findings = compute_findings(&base_inputs());
        assert!(
            findings.is_empty(),
            "a clean non-Windows env with a default repo should have no findings, got: {findings:?}"
        );
        assert_eq!(exit_code(&findings), ExitCode::Success);
    }

    #[test]
    fn missing_default_repo_is_info_not_warning() {
        let inputs = Inputs {
            default_repo_present: false,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        assert_eq!(codes(&findings), vec![FindingCode::NoDefaultRepo]);
        let note = findings.first().expect("one finding");
        assert_eq!(note.level, Level::Info);
        // A repository resolved (base_inputs has repo_root set), so `patina
        // init` would refuse on the existing manifest; the advice must point
        // at `doctor --fix`, name the resolved root, and say what breaks
        // without the pointer (the cwd-less background watch service).
        assert!(
            note.message.contains("patina doctor --fix")
                && note.message.contains("/home/u/dotfiles")
                && note.message.contains("watch service"),
            "with a resolved repository the note must suggest `patina doctor --fix`, \
             name the root, and name the watch-service consequence, got: {}",
            note.message
        );
        assert!(
            !note.message.contains("patina init"),
            "with a resolved repository the note must not suggest `patina init` \
             (it refuses on an existing manifest), got: {}",
            note.message
        );
        // An info-only finding still exits 0.
        assert_eq!(exit_code(&findings), ExitCode::Success);
    }

    #[test]
    fn missing_default_repo_without_a_repo_suggests_init() {
        // No repository resolves at all: there is nothing for `doctor --fix`
        // to record, so the advice is `patina init`.
        let inputs = Inputs {
            repo_root: None,
            default_repo_present: false,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        assert_eq!(codes(&findings), vec![FindingCode::NoDefaultRepo]);
        let note = findings.first().expect("one finding");
        assert_eq!(note.level, Level::Info);
        assert!(
            note.message.contains("patina init"),
            "without a resolved repository the note must suggest `patina init`, got: {}",
            note.message
        );
    }

    #[test]
    fn windows_findings_never_fire_off_windows() {
        // Even with every Windows trigger condition met, an off-Windows host
        // raises none of the DOC-WIN-* findings.
        let inputs = Inputs {
            is_windows: false,
            dev_mode: DevModeStatus::Disabled,
            build_supports_dev_mode: false,
            repo_root: Some(Utf8PathBuf::from(r"\\server\share\dotfiles")),
            repo_declares_symlink: true,
            default_repo_present: true,
            repo_declares_remote: false,
            git_available: true,
            ignored_deployed: Vec::new(),
        };
        let findings = compute_findings(&inputs);
        assert!(
            findings.is_empty(),
            "DOC-WIN-* findings must be gated to Windows, got: {findings:?}"
        );
    }

    #[test]
    fn windows_unc_repo_warns_naming_the_path() {
        let repo = Utf8PathBuf::from(r"\\fileserver\share\dotfiles");
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Enabled,
            build_supports_dev_mode: true,
            repo_root: Some(repo.clone()),
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        let unc = findings
            .iter()
            .find(|f| f.code == FindingCode::WinUnc)
            .expect("UNC finding present");
        assert_eq!(unc.level, Level::Warning);
        assert!(
            unc.message.contains("UNC") && unc.message.contains(repo.as_str()),
            "the UNC warning must name UNC and the path, got: {}",
            unc.message
        );
        assert_eq!(unc.path.as_deref(), Some(repo.as_path()));
    }

    #[test]
    fn windows_devmode_finding_requires_symlink_and_disabled() {
        // Symlink declared + Developer Mode disabled ⇒ the warning fires and
        // names Developer Mode and the registry path.
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Disabled,
            build_supports_dev_mode: true,
            repo_declares_symlink: true,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        let devmode = findings
            .iter()
            .find(|f| f.code == FindingCode::WinDevMode)
            .expect("Developer Mode finding present");
        assert_eq!(devmode.level, Level::Warning);
        assert!(
            devmode.message.contains("Developer Mode")
                && devmode.message.contains(DEV_MODE_REGISTRY_PATH),
            "the warning must name Developer Mode and the registry path, got: {}",
            devmode.message
        );
    }

    #[test]
    fn windows_devmode_finding_absent_when_no_symlink_declared() {
        // Developer Mode disabled but no symlink declared ⇒ no finding (a
        // copy-only repo never needs Developer Mode).
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Disabled,
            build_supports_dev_mode: true,
            repo_declares_symlink: false,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        assert!(
            !findings.iter().any(|f| f.code == FindingCode::WinDevMode),
            "no Developer Mode finding without a symlink declaration, got: {findings:?}"
        );
    }

    #[test]
    fn windows_devmode_finding_absent_when_enabled() {
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Enabled,
            build_supports_dev_mode: true,
            repo_declares_symlink: true,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        assert!(
            !findings.iter().any(|f| f.code == FindingCode::WinDevMode),
            "Developer Mode enabled clears the finding, got: {findings:?}"
        );
    }

    #[test]
    fn windows_old_build_warns() {
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Unsupported,
            build_supports_dev_mode: false,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        let osold = findings
            .iter()
            .find(|f| f.code == FindingCode::WinOsOld)
            .expect("OS-too-old finding present");
        assert_eq!(osold.level, Level::Warning);
        assert!(
            osold.message.contains("1703"),
            "the warning must name the 1703 build floor, got: {}",
            osold.message
        );
    }

    #[test]
    fn finding_order_is_stable() {
        // Inputs that trigger the Windows findings and the state-directory
        // note at once. The order is fixed, so the render is deterministic.
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Disabled,
            build_supports_dev_mode: false,
            repo_root: Some(Utf8PathBuf::from(r"\\server\share\dotfiles")),
            repo_declares_symlink: true,
            default_repo_present: false,
            repo_declares_remote: false,
            git_available: true,
            ignored_deployed: Vec::new(),
        };
        let findings = compute_findings(&inputs);
        assert_eq!(
            codes(&findings),
            vec![
                FindingCode::WinUnc,
                FindingCode::WinDevMode,
                FindingCode::WinOsOld,
                FindingCode::NoDefaultRepo,
            ]
        );
    }

    #[test]
    fn json_envelope_is_deterministic_and_well_shaped() {
        let inputs = Inputs {
            default_repo_present: false,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        let first = json_envelope(&findings);
        let second = json_envelope(&findings);
        assert_eq!(first, second, "same findings yield byte-identical JSON");

        let doc: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        let array = doc
            .get("findings")
            .and_then(serde_json::Value::as_array)
            .expect("findings array");
        assert_eq!(array.len(), 1);
        let entry = array.first().expect("one entry");
        assert_eq!(
            entry.get("code").and_then(serde_json::Value::as_str),
            Some("DOC-NO-DEFAULT-REPO")
        );
        assert_eq!(
            entry.get("level").and_then(serde_json::Value::as_str),
            Some("info")
        );
        // A finding with no associated path omits the `path` key entirely.
        assert!(
            entry.get("path").is_none(),
            "no path key for a pathless finding"
        );
    }

    #[test]
    fn json_envelope_includes_path_when_present() {
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Enabled,
            build_supports_dev_mode: true,
            repo_root: Some(Utf8PathBuf::from(r"\\server\share\dotfiles")),
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        let doc: serde_json::Value =
            serde_json::from_str(&json_envelope(&findings)).expect("valid JSON");
        let entry = doc.pointer("/findings/0").expect("one finding at index 0");
        assert_eq!(
            entry.get("path").and_then(serde_json::Value::as_str),
            Some(r"\\server\share\dotfiles")
        );
    }

    #[test]
    fn human_render_routes_findings_to_stderr() {
        let inputs = Inputs {
            default_repo_present: false,
            ..base_inputs()
        };
        let findings = compute_findings(&inputs);
        let mut reporter = BufferReporter::new();
        render_human(&findings, &mut reporter);
        assert!(
            reporter.err.contains("DOC-NO-DEFAULT-REPO"),
            "findings must render to stderr, got err: {}",
            reporter.err
        );
        assert!(
            reporter.out.is_empty(),
            "no finding prose belongs on stdout in human mode, got out: {}",
            reporter.out
        );
    }

    /// A reader has to tell an advisory note from an error at a glance, and
    /// color alone cannot carry that. The bracketed word must survive a strip.
    #[test]
    fn each_level_keeps_its_bracketed_word_and_paints_apart() {
        let findings = [Level::Info, Level::Warning, Level::Error].map(|level| Finding {
            code: FindingCode::NoGit,
            level,
            message: "a message".to_owned(),
            path: None,
        });

        assert_color_is_additive(|reporter| render_human(&findings, reporter));

        let mut plain = BufferReporter::new();
        render_human(&findings, &mut plain);
        let mut colored = BufferReporter::colored();
        render_human(&findings, &mut colored);

        for level in ["[info]", "[warning]", "[error]"] {
            assert!(
                plain.err.contains(level),
                "the level must stay in the text: {}",
                plain.err
            );
        }
        for level in [Level::Info, Level::Warning, Level::Error] {
            let painted = paint(
                level_style(level, &Styles::colored()),
                &format!("[{}]", level.label()),
            );
            assert!(
                colored.err.contains(&painted),
                "the {} level must wear its own role: {}",
                level.label(),
                colored.err.escape_debug()
            );
        }
    }

    #[test]
    fn human_render_reports_clean_env() {
        let mut reporter = BufferReporter::new();
        render_human(&[], &mut reporter);
        assert!(
            reporter.out.contains("no findings"),
            "a clean env must confirm no findings, got: {}",
            reporter.out
        );
    }
}
