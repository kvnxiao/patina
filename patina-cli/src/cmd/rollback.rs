//! `patina rollback` command logic.
//!
//! Reverses the most recent committed apply to its pre-apply filesystem
//! state. The engine semantics (lock, journal scan, per-entry atomic
//! inverse replay, rolled-back sentinel) live in `patina_core::rollback`;
//! this module owns the TTY-prompt / `--yes` / `--json` decision tree and
//! maps the engine outcome onto the process exit code, all output routed
//! through the [`Reporter`].
//!
//! ## Exit codes
//!
//! | Outcome                                         | Code |
//! |-------------------------------------------------|------|
//! | Rolled back, previewed, or user-confirmed       | 0    |
//! | No prior apply / per-entry atomic abort         | 1    |
//! | User declined the prompt                        | 5    |
//!
//! A `NoPriorApply` or `RollbackPartial` is a typed engine error that
//! exits 1 with the message on stderr; every other engine error
//! (lock timeout, IO) propagates as an `anyhow` error from `run`.

use crate::cli::RollbackArgs;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Result;
use patina_core::EngineError;
use patina_core::RollbackError;
use patina_core::RollbackOptions;

/// Run `patina rollback`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error when the engine-level rollback fails for a reason other
/// than the two typed user-facing outcomes (`NoPriorApply`,
/// `RollbackPartial`), which are surfaced as a stderr warning and exit
/// code 1 rather than an `Err`. A declined prompt maps to exit code 5.
pub async fn run(
    args: &RollbackArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let should_proceed = match (args.yes, tty) {
        (true, _) => true,
        (false, Tty::NonInteractive) => {
            // Non-TTY without --yes: preview only, exit 0 (mirrors apply).
            if args.json {
                reporter.json(&json_envelope("previewed"));
            } else {
                reporter.line("Would roll back the most recent apply. Re-run with --yes to apply.");
            }
            return Ok(ExitCode::Success.code());
        }
        (false, Tty::Interactive) => {
            reporter.prompt("Roll back the most recent apply? [y/N] ");
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    };

    if !should_proceed {
        return Ok(ExitCode::UserDeclined.code());
    }

    match patina_core::rollback(RollbackOptions::default()).await {
        Ok(()) => {
            if args.json {
                reporter.json(&json_envelope("rolled_back"));
            } else {
                reporter.line("Rolled back the most recent apply.");
            }
            Ok(ExitCode::Success.code())
        }
        // The two typed user-facing outcomes exit 1 with the message on
        // stderr rather than bubbling up as an `anyhow` error.
        Err(EngineError::Rollback(
            err @ (RollbackError::NoPriorApply | RollbackError::RollbackPartial { .. }),
        )) => {
            reporter.warn(&err.to_string());
            Ok(ExitCode::Generic.code())
        }
        Err(other) => Err(other.into()),
    }
}

/// Build the `--json` envelope: a single `result` field naming the outcome.
fn json_envelope(result: &str) -> String {
    let envelope = serde_json::json!({ "result": result });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;

    struct ScriptedReader {
        answer: Option<String>,
    }

    impl PromptReader for ScriptedReader {
        fn read_line(&mut self) -> Option<String> {
            self.answer.take()
        }
    }

    #[tokio::test]
    async fn non_tty_without_yes_previews_and_exits_zero() {
        let args = RollbackArgs::default();
        let mut reader = ScriptedReader { answer: None };
        let mut reporter = BufferReporter::new();
        let code = run(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .await
            .expect("preview path never errors");
        assert_eq!(code, 0);
        assert!(reporter.out.contains("Would roll back"));
    }

    #[tokio::test]
    async fn declined_prompt_exits_five() {
        let args = RollbackArgs::default();
        let mut reader = ScriptedReader {
            answer: Some("n\n".to_owned()),
        };
        let mut reporter = BufferReporter::new();
        let code = run(&args, Tty::Interactive, &mut reader, &mut reporter)
            .await
            .expect("declined prompt is not an error");
        assert_eq!(code, 5);
    }

    #[test]
    fn json_envelope_carries_result() {
        let doc: serde_json::Value =
            serde_json::from_str(&json_envelope("rolled_back")).expect("valid JSON");
        assert_eq!(
            doc.get("result").and_then(serde_json::Value::as_str),
            Some("rolled_back")
        );
    }
}
