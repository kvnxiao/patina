//! `patina doctor` read-only environment inspection (REQ-005, REQ-010).
//!
//! `patina doctor` inspects the per-machine state directory, the resolved
//! repository path, the running OS, and the declared file modes in the
//! repository, then emits an exhaustively-specified set of findings. The
//! finding set is the complete v1.0 set; adding to it requires a SPEC
//! amendment (DEC-004 keeps cloud-sync detection out of scope).
//!
//! The read-only path (no `--fix`) acquires only the SHARED advisory lock
//! (REQ-009) with the read-only escape hatch: a
//! [`SHARED_TIMEOUT`] expiry warns and proceeds
//! rather than blocking the user.
//!
//! The `--fix` path (REQ-006) is mutating: it acquires the EXCLUSIVE lock,
//! then walks the fixable findings — Developer Mode missing on Windows and a
//! missing `default_repo` pointer — prompting per finding (or auto-accepting
//! under `--yes`) and remediating on accept. Non-fixable findings (UNC paths,
//! OS-too-old) are still surfaced with their warning. A non-TTY `--fix`
//! without `--yes` cannot prompt, so it refuses with exit 1 naming the missing
//! flag. Each remediation that runs emits a structured `tracing` event
//! recording the finding code, the chosen remediation, and the outcome.
//!
//! Exit codes follow REQ-005: 0 when only warning/info findings were raised;
//! 1 only on an error-level finding. The v1.0 finding set has no error-level
//! finding, so the exit-1 path is reserved for future additions.
//!
//! Output follows REQ-010: human findings to stderr, `--json` emits a single
//! deterministic document on stdout (no timestamps / PIDs / random ids), so
//! two runs against unchanged state are byte-identical (CHK-018). The findings
//! computation ([`compute_findings`]) is pure over its inputs so the whole
//! finding set is unit-testable on the macOS/Linux CI, with the
//! Windows-specific reads gated behind the [`Inputs`] struct the caller fills.

use crate::cli::DoctorArgs;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
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
use patina_core::SHARED_TIMEOUT;
use patina_core::acquire_lock;
use patina_core::canonicalize_path;
use patina_core::dev_mode_status;
use patina_core::discover_modules;
use patina_core::exclusive_timeout;
use patina_core::is_unc_path;
use patina_core::parse_module_config;
use patina_core::persisted_default_present;
use patina_core::resolve_repository_root;
use patina_core::resolve_state_dir;
use patina_core::windows_build_supports_dev_mode;
use patina_core::write_persisted_default;

/// A single doctor finding (REQ-005). Carries a stable [`FindingCode`], a
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
/// ([`FindingCode::label`]) is part of the JSON contract (REQ-010) and the
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
        }
    }
}

/// A finding's severity. `Info` is advisory, `Warning` does not fail the
/// command, `Error` would (no v1.0 finding is `Error`; the variant reserves
/// the exit-1 path for future additions per REQ-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Advisory note; never affects the exit code.
    Info,
    /// A warning; the command still exits 0.
    Warning,
    /// An error; the command exits 1. No v1.0 finding uses this.
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

/// The host-state inputs the finding computation reads, gathered by [`run`]
/// before the pure [`compute_findings`] decides the finding set.
///
/// Abstracting the reads behind this struct lets the whole finding set be
/// unit-tested on any platform: the Windows-specific reads (Developer Mode
/// status, OS-build support) are plain fields the test fills directly, with
/// no real registry in the loop.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent host-state fact gathered from a distinct source (the platform, the repository's declared modes, the OS-build query, the state-directory pointer), not a state machine that would be better modelled as an enum. They are read once in compute_findings and never combined into a single mode."
)]
pub struct Inputs {
    /// Whether the running host is Windows. Off Windows the three `DOC-WIN-*`
    /// findings never fire regardless of the other fields.
    pub is_windows: bool,
    /// The Developer Mode registry status (from
    /// [`dev_mode_status`]).
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
}

/// Run `patina doctor`. Returns the process exit code.
///
/// Without `--fix` this is the read-only diagnostic path: acquire the SHARED
/// lock (with the read-only escape hatch) and report findings. With `--fix`
/// (REQ-006) it is the mutating remediation path: acquire the EXCLUSIVE lock,
/// then prompt-and-remediate each fixable finding.
///
/// # Errors
///
/// Returns an error (exit 1) when the per-machine state directory cannot be
/// resolved. On the read-only path, repository-discovery and manifest-parse
/// failures are not fatal: doctor is a diagnostic, so it reports what it can
/// and treats an unresolvable repository as "no repository-scoped findings"
/// rather than aborting; a shared-lock timeout is downgraded to a stderr
/// warning (REQ-009 read-only escape hatch). On the `--fix` path an
/// exclusive-lock timeout maps to exit 4 via the engine-error chain, and a
/// remediation failure (the persisted-default write, or the Windows helper
/// running but leaving the flag off) is a hard error (exit 1).
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
/// Acquires only the SHARED lock, with the read-only escape hatch — a timeout
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

/// The interactive remediation path (`--fix`, REQ-006).
///
/// A non-TTY `--fix` without `--yes` cannot prompt, so it refuses up front
/// with exit 1 (REQ-006) before taking any lock or mutating anything. With a
/// TTY (or `--yes`) it acquires the EXCLUSIVE lock (REQ-009), recomputes the
/// findings under the lock, then walks each fixable finding: prompt (or
/// auto-accept under `--yes`) and remediate on accept. Non-fixable findings
/// still surface as warnings. Each remediation that runs emits a structured
/// `tracing` event (REQ-006).
fn run_fix(
    args: &DoctorArgs,
    state: &Utf8Path,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    // Non-TTY without --yes: no per-finding consent is possible, so refuse
    // before acquiring the lock or mutating anything (REQ-006).
    if !args.yes && tty == Tty::NonInteractive {
        reporter.warn(
            "`patina doctor --fix` cannot prompt in a non-TTY shell; \
             pass --yes to accept every remediation automatically",
        );
        return Ok(ExitCode::Generic.code());
    }

    // REQ-009: take the EXCLUSIVE lock before any mutation — distinct from the
    // read-only path's shared lock. A contention timeout reaches the exit-4
    // mapping through the engine-error chain.
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
            // cannot remedy them, and move on (REQ-006).
            FindingCode::WinUnc | FindingCode::WinOsOld => {
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
/// directory's canonical absolute path as the persisted default (REQ-006). The
/// CWD must be a valid Patina repository; canonicalization failure is a hard
/// error (exit 1).
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
    let canonical = canonicalize_path(&cwd).map_err(EngineError::from)?;
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
/// elevation flow (REQ-007) and re-checking the registry afterward (REQ-006).
/// Off Windows this finding never fires, so the body is Windows-only; the
/// non-Windows stub keeps the single call site compiling on every platform.
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

/// Non-Windows stub: the `DOC-WIN-DEVMODE` finding never fires off Windows
/// (the three `DOC-WIN-*` findings are gated to `is_windows` in
/// [`compute_findings`]), so this arm is unreachable in practice. It exists
/// only so the `--fix` match compiles without a `#[cfg]` at the call site.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature parity with the fallible #[cfg(windows)] variant so the single call site in run_fix compiles on every platform"
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
            reporter.prompt(&format!("{question} [y/N] "));
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    }
}

/// Gather the host-state [`Inputs`] the finding computation reads.
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
    Inputs {
        is_windows: cfg!(windows),
        dev_mode: dev_mode_status(),
        build_supports_dev_mode: windows_build_supports_dev_mode(),
        repo_root,
        repo_declares_symlink,
        default_repo_present: persisted_default_present(state),
    }
}

/// Whether `repo_root`'s modules declare any `symlink` / `symlink-dir`
/// `[[file]]` entry. A module whose manifest fails to parse is skipped (it is
/// not a symlink declaration we can confirm); a discovery failure yields
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
/// The order is stable (UNC, Developer Mode, OS-too-old, then the
/// state-directory note) so the rendered output is deterministic (REQ-010).
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
        findings.push(Finding {
            code: FindingCode::NoDefaultRepo,
            level: Level::Info,
            message: "no default repository is recorded in the state directory; \
                      run `patina init` to set one."
                .to_owned(),
            path: None,
        });
    }

    findings
}

/// The exit code REQ-005 assigns: 1 if any error-level finding was raised,
/// otherwise 0. The v1.0 finding set has no error-level finding, so this is 0
/// in practice; the error branch reserves the exit-1 path for future additions.
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
/// byte-identical (CHK-018, REQ-010).
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

/// Render the findings to stderr as one warning line each (REQ-010 routes all
/// findings to stderr regardless of format). A clean environment prints a
/// single "no findings" line so the user gets explicit confirmation.
fn render_human(findings: &[Finding], reporter: &mut impl Reporter) {
    if findings.is_empty() {
        reporter.line("doctor: no findings; the environment looks healthy.");
        return;
    }
    for finding in findings {
        reporter.warn(&format!(
            "[{}] {}: {}",
            finding.level.label(),
            finding.code.label(),
            finding.message
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;

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
        // The NonInteractive arm declines; run_fix refuses before we get here,
        // so this conservative default never auto-remediates.
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
        // REQ-006: a non-TTY --fix without --yes cannot prompt, so it refuses
        // with exit 1 naming the missing --yes flag — before any lock or
        // mutation. The state path is never touched because we return first.
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
        }
    }

    fn codes(findings: &[Finding]) -> Vec<FindingCode> {
        findings.iter().map(|f| f.code).collect()
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
        assert!(
            note.message.contains("patina init"),
            "the note must suggest `patina init`, got: {}",
            note.message
        );
        // An info-only finding still exits 0.
        assert_eq!(exit_code(&findings), ExitCode::Success);
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
        // names Developer Mode and the registry path (CHK-010).
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
        // All four findings present at once: order is UNC, DevMode, OSOld,
        // NoDefaultRepo — fixed so the rendered output is deterministic.
        let inputs = Inputs {
            is_windows: true,
            dev_mode: DevModeStatus::Disabled,
            build_supports_dev_mode: false,
            repo_root: Some(Utf8PathBuf::from(r"\\server\share\dotfiles")),
            repo_declares_symlink: true,
            default_repo_present: false,
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
