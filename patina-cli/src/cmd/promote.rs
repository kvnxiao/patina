//! `patina promote <target>` command logic.
//!
//! `patina promote <target>` reconciles a copy-mode target that the user
//! edited outside Patina. It copies the target's current bytes back into the
//! repository source the target was materialized from, then re-applies. The
//! fresh `<ts>.COMMIT` therefore records the new content's hash as the
//! expected hash, and `patina status` classifies the target CLEAN again.
//!
//! These target shapes are refused (exit 1):
//!
//! - **Symbolic-link targets** ([`ExpectedTarget::Symlink`]). The bytes a user
//!   sees through the link are already the repository's, so there is nothing to
//!   copy back.
//! - **Template-rendered targets** (the journaled source ends in `.tmpl`).
//!   Templating is non-invertible: the rendered bytes cannot be turned back
//!   into a template, so promotion cannot recover the source.
//! - **Remote-backed targets** (the journaled source lies under
//!   `<state>/remotes/`). A pinned checkout is immutable third-party content:
//!   writing into it would modify every entry reading that path until the pin
//!   moves. `patina remote check` compares revisions, so it would never report
//!   the edit.
//!
//! A `copy-tree` target promotes only the leaf file given on the command line,
//! not the whole tree: the journal records one [`ExpectedTarget`] per
//! materialized leaf, so the lookup resolves the single leaf and only its
//! source is rewritten.
//!
//! Like `remove`, `promote` holds one exclusive advisory lock for the whole
//! command and re-journals under
//! [`LockPolicy::Held`](patina_core::LockPolicy) through the shared helpers in
//! [`crate::cmd::managed`].
//!
//! Planning, journaling, and repo discovery live in `patina_core`; this
//! module is presentation and control flow.

use crate::cli::PromoteArgs;
use crate::cmd::add::resolve_home;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::cmd::managed::TEMPLATE_SUFFIX;
use crate::cmd::managed::acquire_state_and_lock;
use crate::cmd::managed::rejournal;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::paint;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::EngineError;
use patina_core::ExpectedTarget;
use patina_core::anchor_input;
use patina_core::canonicalize_path;
use patina_core::manage_key;
use patina_core::read_latest_commit;
use patina_core::remote::cache::remotes_root;

/// Run `patina promote`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout through
/// the engine-error chain) when the state directory cannot be resolved, the
/// lock cannot be acquired, the target's bytes cannot be read, the repository
/// source cannot be written, or the re-apply fails. An unmanaged target and
/// each refused shape (symbolic-link, template-rendered, remote-backed) return
/// their exit code through the `Ok` value instead.
pub(crate) async fn run(
    args: &PromoteArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let home = resolve_home()?;
    let target = anchor_input(&args.target, &home).map_err(EngineError::from)?;
    let target_key = manage_key(&target);

    let (state, guard) = acquire_state_and_lock()?;

    let journal_dir = state.join("journal");
    let record = read_latest_commit(&journal_dir).map_err(EngineError::from)?;
    let expected = record.as_ref().and_then(|record| {
        record
            .targets
            .iter()
            .find(|expected| manage_key(Utf8Path::new(expected.target())) == target_key)
    });
    let Some(expected) = expected else {
        return Ok(report_unmanaged(args, reporter));
    };

    if let Some(code) = refuse_unpromotable(args, expected, &state, reporter) {
        return Ok(code);
    }

    if !confirm(args, tty, reader, reporter) {
        return Ok(ExitCode::UserDeclined.code());
    }

    let target_path = Utf8PathBuf::from(expected.target());
    let source_path = Utf8PathBuf::from(expected.source());
    let bytes = fs_err::read(target_path.as_std_path())
        .with_context(|| format!("failed to read the target {target_path}"))?;
    fs_err::write(source_path.as_std_path(), &bytes)
        .with_context(|| format!("failed to write the repository source {source_path}"))?;

    rejournal(guard).await?;

    report_success(args, &target_path, &source_path, reporter);
    Ok(ExitCode::Success.code())
}

fn refuse_unpromotable(
    args: &PromoteArgs,
    expected: &ExpectedTarget,
    state: &Utf8Path,
    reporter: &mut impl Reporter,
) -> Option<i32> {
    if let ExpectedTarget::Content { source, .. } = expected
        && let Some(remote) = remote_backing(Utf8Path::new(source), state)
    {
        let message = format!(
            "{} is deployed from the remote `{remote}`: its source {source} is a \
             pinned checkout Patina treats as immutable, so the edited bytes \
             cannot be promoted into it. Change the upstream repository and run \
             `patina remote update {remote}`.",
            args.target
        );
        report_refusal(args, "remote_backed_target", &message, reporter);
        return Some(ExitCode::Generic.code());
    }
    match expected {
        ExpectedTarget::Symlink { .. } => {
            let message = format!(
                "{} is a symbolic-link target: a symlink shares its content with \
                 its source, so there is nothing to promote back into the repository.",
                args.target
            );
            report_refusal(args, "symlink_target", &message, reporter);
            Some(ExitCode::Generic.code())
        }
        ExpectedTarget::Content { source, .. } if source.ends_with(TEMPLATE_SUFFIX) => {
            let message = format!(
                "{} is rendered from the template source {source}: templating is \
                 non-invertible, so the rendered output cannot be promoted back \
                 into the template.",
                args.target
            );
            report_refusal(args, "template_target", &message, reporter);
            Some(ExitCode::Generic.code())
        }
        ExpectedTarget::Content { .. } => None,
        // `ExpectedTarget` is #[non_exhaustive]; a future materialization
        // shape is refused conservatively rather than silently promoted.
        _ => {
            let message = format!(
                "{} has an expected-state shape promote does not know how to \
                 reconcile.",
                args.target
            );
            report_refusal(args, "unpromotable_target", &message, reporter);
            Some(ExitCode::Generic.code())
        }
    }
}

/// Return the remote whose pinned checkout contains `source`.
///
/// Apply records a canonical source, but the cache root retains the state
/// directory's spelling. macOS can resolve `/var` to `/private/var`, Windows
/// can return an 8.3 short name, and a symlinked home directory can produce
/// another spelling. The lookup checks the original and canonical cache roots.
fn remote_backing(source: &Utf8Path, state: &Utf8Path) -> Option<String> {
    let root = remotes_root(state);
    let canonical = canonicalize_path(&root).ok();
    std::iter::once(root.as_path())
        .chain(canonical.as_deref())
        .find_map(|root| source.strip_prefix(root).ok())
        .and_then(|relative| relative.components().next())
        .map(|component| component.as_str().to_owned())
}

/// Report a refusal through the reporter: a JSON error envelope on stdout under
/// `--json`, otherwise a warning line on stderr.
fn report_refusal(args: &PromoteArgs, error: &str, message: &str, reporter: &mut impl Reporter) {
    if args.json {
        reporter.json(&error_envelope(error, args.target.as_str(), message));
    } else {
        reporter.warn(message);
    }
}

/// Confirm the promotion before mutating. `--yes` proceeds unconditionally; a
/// TTY prompts; a non-TTY without `--yes` declines (no consent is possible).
fn confirm(
    args: &PromoteArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> bool {
    match (args.yes, tty) {
        (true, _) => true,
        (false, Tty::NonInteractive) => {
            reporter
                .warn("refusing to promote without confirmation: pass --yes in a non-TTY shell");
            false
        }
        (false, Tty::Interactive) => {
            reporter.confirm(&format!("Promote {}?", args.target));
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    }
}

/// Report the unmanaged-target refusal (exit 1) and return the exit code.
///
/// The message includes the target and every discovery source: `$PATINA_REPO`,
/// the walk-up from the current directory, and the persisted default. It
/// repeats the established discovery-error wording, so `remove` and `promote`
/// explain an unmanaged path the same way.
fn report_unmanaged(args: &PromoteArgs, reporter: &mut impl Reporter) -> i32 {
    let message = format!(
        "{} is not managed by patina (no journaled apply lists it). \
         patina resolves the repository from $PATINA_REPO, a walk-up from the \
         current directory, or the persisted default repo.",
        args.target
    );
    if args.json {
        reporter.json(&error_envelope(
            "not_managed",
            args.target.as_str(),
            &message,
        ));
    } else {
        reporter.warn(&message);
    }
    ExitCode::Generic.code()
}

/// Report a successful promotion through the reporter.
fn report_success(
    args: &PromoteArgs,
    target: &Utf8Path,
    source: &Utf8Path,
    reporter: &mut impl Reporter,
) {
    if args.json {
        reporter.json(&success_envelope(&args.target, target, source));
    } else {
        let path = paint(reporter.styles().path, args.target.as_str());
        reporter.line(&format!(
            "Promoted {path}: copied its current bytes into {source} and re-applied."
        ));
    }
}

/// Build the `--json` success envelope. Deterministic for a given input (no
/// timestamps / PIDs).
fn success_envelope(target: &Utf8Path, resolved_target: &Utf8Path, source: &Utf8Path) -> String {
    let envelope = serde_json::json!({
        "promoted": target.as_str(),
        "target": resolved_target.as_str(),
        "source": source.as_str(),
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build the `--json` typed-error envelope: `error`, `target`, `message`.
/// `promote` keys the subject `target` rather than `path`, after its own
/// positional argument.
fn error_envelope(error: &str, target: &str, message: &str) -> String {
    let envelope = serde_json::json!({
        "error": error,
        "target": target,
        "message": message,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use patina_core::Disposition;

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

    fn args() -> PromoteArgs {
        PromoteArgs {
            target: Utf8PathBuf::from("~/.gitconfig"),
            json: false,
            yes: false,
        }
    }

    fn preapproved_args() -> PromoteArgs {
        PromoteArgs {
            yes: true,
            ..args()
        }
    }

    fn symlink_target() -> ExpectedTarget {
        ExpectedTarget::Symlink {
            target: "/home/u/.zshrc".to_owned(),
            link_target: "/repo/zsh/zshrc".to_owned(),
            entry: 0,
            disposition: Disposition::Create,
        }
    }

    fn template_target() -> ExpectedTarget {
        ExpectedTarget::Content {
            target: "/home/u/.gitconfig".to_owned(),
            source: "/repo/git/gitconfig.tmpl".to_owned(),
            hash: [0u8; 32],
            entry: 0,
            disposition: Disposition::Create,
        }
    }

    fn copy_target() -> ExpectedTarget {
        ExpectedTarget::Content {
            target: "/home/u/.gitconfig".to_owned(),
            source: "/repo/git/gitconfig".to_owned(),
            hash: [0u8; 32],
            entry: 0,
            disposition: Disposition::Create,
        }
    }

    fn state() -> Utf8PathBuf {
        Utf8PathBuf::from("/state/patina")
    }

    fn remote_backed_target() -> ExpectedTarget {
        ExpectedTarget::Content {
            target: "/home/u/.claude/skills/tone.md".to_owned(),
            source: "/state/patina/remotes/humanizer/abc123/skills/tone.md".to_owned(),
            hash: [0u8; 32],
            entry: 0,
            disposition: Disposition::Create,
        }
    }

    #[test]
    fn refuse_unpromotable_refuses_remote_backed_targets() {
        let mut reporter = BufferReporter::new();
        let code = refuse_unpromotable(
            &preapproved_args(),
            &remote_backed_target(),
            &state(),
            &mut reporter,
        );
        assert_eq!(code, Some(ExitCode::Generic.code()));
        assert!(
            reporter.err.contains("humanizer") && reporter.err.contains("remote update"),
            "the refusal must name the remote and the command that moves its pin, got: {}",
            reporter.err
        );
    }

    #[test]
    fn no_refusal_carries_collapsed_line_continuations() {
        for expected in [
            symlink_target(),
            template_target(),
            copy_target(),
            remote_backed_target(),
        ] {
            let mut reporter = BufferReporter::new();
            refuse_unpromotable(&preapproved_args(), &expected, &state(), &mut reporter);
            assert!(
                !reporter.err.contains("  "),
                "refusal for {expected:?} carries a multi-space run: {}",
                reporter.err
            );
        }
    }

    #[test]
    fn remote_backing_names_the_remote_and_ignores_a_repository_source() {
        assert_eq!(
            remote_backing(
                Utf8Path::new("/state/patina/remotes/humanizer/abc123/SKILL.md"),
                &state()
            )
            .as_deref(),
            Some("humanizer")
        );
        assert!(
            remote_backing(Utf8Path::new("/repo/git/gitconfig"), &state()).is_none(),
            "a repository source is promotable"
        );
        assert!(
            remote_backing(Utf8Path::new("/state/patina/journal/x"), &state()).is_none(),
            "another state-directory subtree is not a remote checkout"
        );
    }

    #[test]
    fn refuse_unpromotable_refuses_symlink_targets() {
        let mut reporter = BufferReporter::new();
        let code = refuse_unpromotable(
            &preapproved_args(),
            &symlink_target(),
            &state(),
            &mut reporter,
        );
        assert_eq!(code, Some(ExitCode::Generic.code()));
        assert!(
            reporter.err.contains("symbolic-link") && reporter.err.contains("source"),
            "the refusal must explain symlink targets share content with their source, got: {}",
            reporter.err
        );
    }

    #[test]
    fn refuse_unpromotable_refuses_template_targets() {
        let mut reporter = BufferReporter::new();
        let code = refuse_unpromotable(
            &preapproved_args(),
            &template_target(),
            &state(),
            &mut reporter,
        );
        assert_eq!(code, Some(ExitCode::Generic.code()));
        assert!(
            reporter.err.contains("gitconfig.tmpl") && reporter.err.contains("template"),
            "the refusal must include the .tmpl source and the word template, got: {}",
            reporter.err
        );
    }

    #[test]
    fn refuse_unpromotable_allows_copy_targets() {
        let mut reporter = BufferReporter::new();
        let code =
            refuse_unpromotable(&preapproved_args(), &copy_target(), &state(), &mut reporter);
        assert!(
            code.is_none(),
            "a copy-mode content target must be promotable"
        );
        assert!(
            reporter.err.is_empty(),
            "no refusal must be reported for a copy target, got: {}",
            reporter.err
        );
    }

    #[test]
    fn confirm_yes_proceeds_without_reading() {
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            &preapproved_args(),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter
        ));
    }

    #[test]
    fn confirm_non_tty_without_yes_declines() {
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let proceed = confirm(&args(), Tty::NonInteractive, &mut reader, &mut reporter);
        assert!(!proceed, "a non-TTY shell without --yes must decline");
        assert!(
            reporter.err.contains("--yes"),
            "the refusal must include --yes, got: {}",
            reporter.err
        );
    }

    #[test]
    fn confirm_tty_reads_the_answer() {
        let mut reader = ScriptedReader::new(&["y\n"]);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            &args(),
            Tty::Interactive,
            &mut reader,
            &mut reporter
        ));

        let mut reader = ScriptedReader::new(&["n\n"]);
        let mut reporter = BufferReporter::new();
        assert!(!confirm(
            &args(),
            Tty::Interactive,
            &mut reader,
            &mut reporter
        ));
    }

    #[test]
    fn success_envelope_is_deterministic() {
        let target = Utf8Path::new("~/.gitconfig");
        let resolved = Utf8Path::new("/home/u/.gitconfig");
        let source = Utf8Path::new("/repo/git/gitconfig");
        let first = success_envelope(target, resolved, source);
        let second = success_envelope(target, resolved, source);
        assert_eq!(first, second, "same inputs yield byte-identical JSON");
        let doc: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert_eq!(
            doc.get("promoted").and_then(serde_json::Value::as_str),
            Some("~/.gitconfig")
        );
        assert_eq!(
            doc.get("source").and_then(serde_json::Value::as_str),
            Some("/repo/git/gitconfig")
        );
    }
}
