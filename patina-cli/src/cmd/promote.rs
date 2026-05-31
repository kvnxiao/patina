//! `patina promote <target>` command logic (REQ-004).
//!
//! `patina promote <target>` reconciles a copy-mode target that the user
//! edited outside Patina: it copies the target's current bytes back into the
//! repository source the target was materialized from, then re-applies so the
//! fresh `<ts>.COMMIT` records the new content's hash as the expected hash and
//! `patina status` classifies the target CLEAN again.
//!
//! Two target shapes are refused (exit 1):
//!
//! - **Symbolic-link targets** ([`ExpectedTarget::Symlink`]). A symlink IS its
//!   source — the bytes the user sees through the link are the repository bytes
//!   — so there is nothing to copy back and promotion is meaningless.
//! - **Template-rendered targets** (the journaled source ends in `.tmpl`).
//!   Templating is non-invertible (DEC-006): the rendered bytes cannot be
//!   turned back into a template, so promotion cannot recover the source.
//!
//! A `copy-tree` target promotes the individual leaf file named, not the whole
//! tree: the journal carries one [`ExpectedTarget`] per materialized leaf, so
//! the lookup resolves the single leaf and only its source is rewritten.
//!
//! Like `remove`, `promote` holds ONE exclusive advisory lock for the whole
//! command (REQ-009) and re-journals under
//! [`LockPolicy::Held`](patina_core::LockPolicy) via the shared
//! [`crate::cmd::managed`] scaffolding, so the re-apply reuses the held guard
//! instead of self-contending.
//!
//! Module-level engine semantics (planning, journaling, repo discovery) live
//! in `patina_core`; this module is presentation and control flow only, all
//! output routed through the [`Reporter`].

use crate::cli::PromoteArgs;
use crate::cmd::add::resolve_home;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::cmd::managed::TEMPLATE_SUFFIX;
use crate::cmd::managed::acquire_state_and_lock;
use crate::cmd::managed::rejournal;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::EngineError;
use patina_core::ExpectedTarget;
use patina_core::expand_tilde;
use patina_core::manage_key;
use patina_core::read_latest_commit;

/// Run `patina promote`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout via the
/// engine-error chain) when: the state directory cannot be resolved; the lock
/// cannot be acquired; the target is not currently managed; the target is a
/// symlink or template-rendered (refused); the target's bytes cannot be read;
/// the repository source cannot be written; or the re-apply fails.
pub async fn run(
    args: &PromoteArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let home = resolve_home()?;
    let target = expand_tilde(&args.target, &home);
    let target_key = manage_key(&target);

    // REQ-009: take ONE exclusive advisory lock for the whole command. The
    // re-apply below reuses this guard via LockPolicy::Held.
    let (state, guard) = acquire_state_and_lock()?;

    // Locate the journaled expectation for this target in the latest commit.
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

    // Refuse the two non-promotable shapes (exit 1), before any prompt or
    // mutation, so a refused promote never touches the filesystem.
    if let Some(code) = refuse_unpromotable(args, expected, reporter) {
        return Ok(code);
    }

    // Confirm before mutating (REQ-009: never mutate without consent).
    if !confirm(args, tty, reader, reporter) {
        return Ok(ExitCode::UserDeclined.code());
    }

    // Copy the target's current bytes back into the repository source the
    // target was materialized from (REQ-029 records that canonical source).
    let target_path = Utf8PathBuf::from(expected.target());
    let source_path = Utf8PathBuf::from(expected.source());
    let bytes = fs_err::read(target_path.as_std_path())
        .with_context(|| format!("failed to read the target {target_path}"))?;
    fs_err::write(source_path.as_std_path(), &bytes)
        .with_context(|| format!("failed to write the repository source {source_path}"))?;

    // Re-journal by re-applying under the held lock: the fresh <ts>.COMMIT
    // records content_hash(new bytes) as the expected hash, so `status`
    // classifies the target CLEAN.
    rejournal(guard).await?;

    report_success(args, &target_path, &source_path, reporter);
    Ok(ExitCode::Success.code())
}

/// Refuse the two non-promotable target shapes. Returns `Some(exit code)` when
/// the target is a symlink or template-rendered (the caller returns it); `None`
/// when the target is a promotable copy-mode `Content` target.
fn refuse_unpromotable(
    args: &PromoteArgs,
    expected: &ExpectedTarget,
    reporter: &mut impl Reporter,
) -> Option<i32> {
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

/// Report a refusal through the reporter: a JSON error envelope under `--json`,
/// otherwise a warning line. The message goes to stderr either way.
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
            reporter.prompt(&format!("Promote {}? [y/N] ", args.target));
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    }
}

/// Report the unmanaged-target refusal (exit 1) and return the exit code. The
/// message names the target and the three discovery sources, matching the
/// established discovery-error wording.
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
        reporter.line(&format!(
            "Promoted {}: copied its current bytes into {source} and re-applied.",
            args.target
        ));
    }
}

/// Build the `--json` success envelope. Deterministic for a given input (no
/// timestamps / PIDs), so it satisfies REQ-010.
fn success_envelope(target: &Utf8Path, resolved_target: &Utf8Path, source: &Utf8Path) -> String {
    let envelope = serde_json::json!({
        "promoted": target.as_str(),
        "target": resolved_target.as_str(),
        "source": source.as_str(),
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build a `--json` typed-error envelope mirroring `remove`'s shape.
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

    fn args(json: bool, yes: bool) -> PromoteArgs {
        PromoteArgs {
            target: Utf8PathBuf::from("~/.gitconfig"),
            json,
            yes,
        }
    }

    fn symlink_target() -> ExpectedTarget {
        ExpectedTarget::Symlink {
            target: "/home/u/.zshrc".to_owned(),
            link_target: "/repo/zsh/zshrc".to_owned(),
            entry: 0,
        }
    }

    fn template_target() -> ExpectedTarget {
        ExpectedTarget::Content {
            target: "/home/u/.gitconfig".to_owned(),
            source: "/repo/git/gitconfig.tmpl".to_owned(),
            hash: [0u8; 32],
            entry: 0,
        }
    }

    fn copy_target() -> ExpectedTarget {
        ExpectedTarget::Content {
            target: "/home/u/.gitconfig".to_owned(),
            source: "/repo/git/gitconfig".to_owned(),
            hash: [0u8; 32],
            entry: 0,
        }
    }

    #[test]
    fn refuse_unpromotable_refuses_symlink_targets() {
        let mut reporter = BufferReporter::new();
        let code = refuse_unpromotable(&args(false, true), &symlink_target(), &mut reporter);
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
        let code = refuse_unpromotable(&args(false, true), &template_target(), &mut reporter);
        assert_eq!(code, Some(ExitCode::Generic.code()));
        assert!(
            reporter.err.contains("gitconfig.tmpl") && reporter.err.contains("template"),
            "the refusal must name the .tmpl source and the word template, got: {}",
            reporter.err
        );
    }

    #[test]
    fn refuse_unpromotable_allows_copy_targets() {
        let mut reporter = BufferReporter::new();
        let code = refuse_unpromotable(&args(false, true), &copy_target(), &mut reporter);
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
            &args(false, true),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter
        ));
    }

    #[test]
    fn confirm_non_tty_without_yes_declines() {
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let proceed = confirm(
            &args(false, false),
            Tty::NonInteractive,
            &mut reader,
            &mut reporter,
        );
        assert!(!proceed, "a non-TTY shell without --yes must decline");
        assert!(
            reporter.err.contains("--yes"),
            "the refusal must name --yes, got: {}",
            reporter.err
        );
    }

    #[test]
    fn confirm_tty_reads_the_answer() {
        let mut reader = ScriptedReader::new(&["y\n"]);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            &args(false, false),
            Tty::Interactive,
            &mut reader,
            &mut reporter
        ));

        let mut reader = ScriptedReader::new(&["n\n"]);
        let mut reporter = BufferReporter::new();
        assert!(!confirm(
            &args(false, false),
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
