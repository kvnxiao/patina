//! `patina debug journal <path>` command logic.
//!
//! The `debug` group is a namespace for post-mortem tooling; `journal`
//! decodes a binary `<ts>.COMMIT` record when the path ends in `.COMMIT`, and
//! a `<ts>.plan` file otherwise. Both the version-envelope decode and the
//! formatting are engine concerns and live in `patina_core`; this module is
//! control flow and exit-code mapping.
//!
//! ## Exit codes
//!
//! | Outcome                                   | Code |
//! |-------------------------------------------|------|
//! | File decoded and rendered                 | 0    |
//! | Missing / unreadable path, version mismatch, corrupt body | 1 |
//!
//! On decode failure, the reporter prints the path. A version-mismatch failure
//! also includes both major versions: the file's, written by a newer binary,
//! and the one this binary supports.

use crate::cli::DebugCommand;
use crate::cli::DebugJournalArgs;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use patina_core::chain_message;
use patina_core::journal::COMMIT_SUFFIX;
use patina_core::load_commit_file;
use patina_core::load_plan_file;
use patina_core::render_plan;
use patina_core::render_record;

/// Dispatch a `patina debug` subcommand, returning the process exit code.
///
/// A failed decode is printed through the reporter and maps to exit code 1:
/// the `debug` group expresses its terminal states as exit codes, like the
/// rest of the CLI.
#[must_use]
pub(crate) fn run(command: &DebugCommand, reporter: &mut impl Reporter) -> i32 {
    match command {
        DebugCommand::Journal(args) => run_journal(args, reporter),
    }
}

/// Decode and render the commit record or plan file at `args.path`.
fn run_journal(args: &DebugJournalArgs, reporter: &mut impl Reporter) -> i32 {
    let rendered = if args.path.as_str().ends_with(COMMIT_SUFFIX) {
        load_commit_file(&args.path).map(|(record, timestamp)| render_record(&record, &timestamp))
    } else {
        load_plan_file(&args.path).map(|(plan, timestamp)| render_plan(&plan, &timestamp))
    };
    match rendered {
        Ok(rendered) => {
            reporter.out_block(&rendered);
            ExitCode::Success.code()
        }
        Err(err) => {
            // `PlanRenderError`'s `Read` and `Decode` variants each include
            // the path. `Decode`'s source is a `JournalError`, whose
            // version-mismatch arm includes both major versions.
            reporter.warn(&chain_message(&err));
            ExitCode::Generic.code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use patina_core::Disposition;
    use patina_core::Plan;
    use patina_core::PlannedOperation;

    fn args(path: impl Into<Utf8PathBuf>) -> DebugJournalArgs {
        DebugJournalArgs { path: path.into() }
    }

    #[test]
    fn renders_a_valid_plan_to_stdout_and_exits_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join("20260528T120000Z.plan");
        let plan = Plan::new(vec![PlannedOperation::symlink(
            "zsh/zshrc",
            "/home/u/.zshrc",
            Disposition::Create,
        )]);
        fs_err::write(&path, plan.encode().expect("encode")).expect("write plan");

        let mut r = BufferReporter::new();
        let code = run_journal(&args(path), &mut r);
        assert_eq!(code, 0);
        assert!(r.out.contains("symlink"), "{}", r.out);
        assert!(r.out.contains("/home/u/.zshrc"), "{}", r.out);
        assert!(r.err.is_empty(), "no warnings on success: {}", r.err);
    }

    #[test]
    fn renders_a_commit_record_with_its_reaped_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join(format!("20260528T120000Z{COMMIT_SUFFIX}"));
        let record = patina_core::ApplyRecord::new(
            patina_core::LastApply {
                at: "2026-05-28T12:00:00Z".to_owned(),
                user: "u".to_owned(),
                host: "h".to_owned(),
            },
            Vec::new(),
            vec!["/home/u/.old".to_owned()],
        );
        fs_err::write(&path, record.encode().expect("encode")).expect("write commit");

        let mut r = BufferReporter::new();
        let code = run_journal(&args(path), &mut r);
        assert_eq!(code, 0, "{}", r.err);
        assert!(r.out.contains("reaped:\n    /home/u/.old\n"), "{}", r.out);
    }

    #[test]
    fn missing_path_exits_one_and_includes_the_path() {
        let mut r = BufferReporter::new();
        let code = run_journal(&args("/no/such/plan.plan"), &mut r);
        assert_eq!(code, 1);
        assert!(r.err.contains("/no/such/plan.plan"), "{}", r.err);
        assert!(r.out.is_empty(), "nothing rendered on failure: {}", r.out);
    }

    #[test]
    fn version_mismatch_exits_one_and_includes_both_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join("20260528T120000Z.plan");
        let plan = Plan::new(vec![PlannedOperation::copy(
            "a",
            "/home/u/.a",
            Disposition::Create,
        )]);
        let mut bytes = plan.encode().expect("encode");
        // Overwrite the envelope's major with u16::MAX so the running
        // binary (major 1) refuses it.
        bytes
            .get_mut(..2)
            .expect("envelope")
            .copy_from_slice(&u16::MAX.to_le_bytes());
        fs_err::write(&path, bytes).expect("write plan");

        let mut r = BufferReporter::new();
        let code = run_journal(&args(path), &mut r);
        assert_eq!(code, 1);
        assert!(
            r.err.contains("65535"),
            "stderr must include the plan major: {}",
            r.err
        );
        assert!(
            r.err.contains('1'),
            "stderr must include the supported major: {}",
            r.err
        );
        assert!(
            r.err.to_lowercase().contains("version"),
            "stderr must include the word version: {}",
            r.err
        );
    }
}
