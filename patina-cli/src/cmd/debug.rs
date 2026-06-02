//! `patina debug journal <path>` / `patina debug drift-cache <path>`
//! command logic (REQ-020, REQ-007).
//!
//! The `debug` group is a namespace for post-mortem tooling; `journal`
//! decodes a binary `<ts>.plan` file and `drift-cache` decodes a watcher
//! `drift.cache` file. Each renders human-readably to stdout, routed
//! through the [`Reporter`] like every other user-facing output path. The
//! renders themselves live in `patina_core` (the version-envelope decode
//! and the formatting are engine concerns); this module is control flow
//! and exit-code mapping only.
//!
//! ## Exit codes
//!
//! | Outcome                                   | Code |
//! |-------------------------------------------|------|
//! | File decoded and rendered                 | 0    |
//! | Missing / unreadable path, version mismatch, corrupt body | 1 |
//!
//! A version mismatch (a file written by a newer binary) and a missing
//! path are both generic failures under REQ-022's exit-code-1 bucket; the
//! reporter names the path and, for a mismatch, both versions.

use crate::cli::DebugCommand;
use crate::cli::DebugDriftCacheArgs;
use crate::cli::DebugJournalArgs;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use patina_core::load_drift_cache_file;
use patina_core::load_plan_file;
use patina_core::render_drift_cache;
use patina_core::render_plan;

/// Dispatch a `patina debug` subcommand, returning the process exit code.
///
/// A failed decode is surfaced to the user through the reporter and mapped
/// to exit code 1 rather than bubbled as an error: the `debug` group, like
/// the rest of the CLI, expresses terminal states as exit codes.
#[must_use = "the returned exit code is the process's terminal status"]
pub fn run(command: &DebugCommand, reporter: &mut impl Reporter) -> i32 {
    match command {
        DebugCommand::Journal(args) => run_journal(args, reporter),
        DebugCommand::DriftCache(args) => run_drift_cache(args, reporter),
    }
}

/// Decode and render the plan file named by `args.path`.
fn run_journal(args: &DebugJournalArgs, reporter: &mut impl Reporter) -> i32 {
    match load_plan_file(&args.path) {
        Ok((plan, timestamp)) => {
            let rendered = render_plan(&plan, &timestamp);
            reporter.diff(&rendered);
            ExitCode::Success.code()
        }
        Err(err) => {
            // The typed error's own `Display` is the single source of truth
            // for the human-readable line: `Read` carries its IO cause and
            // `Decode` carries its `JournalError` (a version mismatch names
            // both majors). Mirrors `rollback.rs`'s `err.to_string()` path.
            reporter.warn(&err.to_string());
            ExitCode::Generic.code()
        }
    }
}

/// Decode and render the drift cache named by `args.path`.
fn run_drift_cache(args: &DebugDriftCacheArgs, reporter: &mut impl Reporter) -> i32 {
    match load_drift_cache_file(&args.path) {
        Ok(cache) => {
            let rendered = render_drift_cache(&cache);
            reporter.diff(&rendered);
            ExitCode::Success.code()
        }
        Err(err) => {
            // The typed `DriftCacheError`'s own `Display` is the source of
            // truth for the failure reason (its `VersionMismatch` arm names
            // both the found and supported majors). Its `Filesystem` arm,
            // however, is a `#[from] std::io::Error` that does not carry the
            // path, so this control-flow layer — which holds `args.path` —
            // prefixes it to honour the contract that a debug failure names
            // the file it was pointed at.
            reporter.warn(&format!("{}: {err}", args.path));
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
    use patina_core::DRIFT_CACHE_MAJOR_VERSION;
    use patina_core::Disposition;
    use patina_core::DriftCache;
    use patina_core::DriftEntry;
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
    fn missing_path_exits_one_and_names_the_path() {
        let mut r = BufferReporter::new();
        let code = run_journal(&args("/no/such/plan.plan"), &mut r);
        assert_eq!(code, 1);
        assert!(r.err.contains("/no/such/plan.plan"), "{}", r.err);
        assert!(r.out.is_empty(), "nothing rendered on failure: {}", r.out);
    }

    #[test]
    fn version_mismatch_exits_one_and_names_both_versions() {
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
        assert!(r.err.contains("65535"), "names the plan major: {}", r.err);
        assert!(r.err.contains('1'), "names the supported major: {}", r.err);
        assert!(
            r.err.to_lowercase().contains("version"),
            "names the version dimension: {}",
            r.err
        );
    }

    fn drift_args(path: impl Into<Utf8PathBuf>) -> DebugDriftCacheArgs {
        DebugDriftCacheArgs { path: path.into() }
    }

    #[test]
    fn renders_a_valid_drift_cache_to_stdout_and_exits_zero() {
        // CHK-018: a populated drift cache renders with the version, the
        // bound journal timestamp, the target path, and both hashes; exit 0.
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join("drift.cache");
        let entry = DriftEntry::new("/home/u/.gitconfig", [0x11; 32], [0x22; 32], 1_700_000_000);
        let cache = DriftCache::new("20260528T120000Z", vec![entry]);
        fs_err::write(&path, cache.encode().expect("encode")).expect("write drift cache");

        let mut r = BufferReporter::new();
        let code = run_drift_cache(&drift_args(path), &mut r);
        assert_eq!(code, 0);
        assert!(r.out.contains("version:"), "names the version: {}", r.out);
        assert!(
            r.out.contains("20260528T120000Z"),
            "names the bound journal timestamp: {}",
            r.out
        );
        assert!(
            r.out.contains("/home/u/.gitconfig"),
            "names the target path: {}",
            r.out
        );
        // Both 32-byte hashes render as their lower-case hex repeats.
        assert!(
            r.out.contains(&"11".repeat(32)),
            "names the expected hash: {}",
            r.out
        );
        assert!(
            r.out.contains(&"22".repeat(32)),
            "names the actual hash: {}",
            r.out
        );
        assert!(r.err.is_empty(), "no warnings on success: {}", r.err);
    }

    #[test]
    fn missing_drift_cache_path_exits_one_and_names_the_path() {
        let mut r = BufferReporter::new();
        let code = run_drift_cache(&drift_args("/no/such/drift.cache"), &mut r);
        assert_eq!(code, 1);
        assert!(
            r.err.contains("/no/such/drift.cache"),
            "names the missing path: {}",
            r.err
        );
        assert!(r.out.is_empty(), "nothing rendered on failure: {}", r.out);
    }

    #[test]
    fn drift_cache_version_mismatch_exits_one_and_names_both_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join("drift.cache");
        let cache = DriftCache::new("20260528T120000Z", vec![]);
        let mut bytes = cache.encode().expect("encode");
        // Overwrite the envelope's major with u16::MAX so the running binary
        // (drift-cache major 1) refuses it, naming both versions.
        bytes
            .get_mut(..2)
            .expect("envelope")
            .copy_from_slice(&u16::MAX.to_le_bytes());
        fs_err::write(&path, bytes).expect("write drift cache");

        let mut r = BufferReporter::new();
        let code = run_drift_cache(&drift_args(path), &mut r);
        assert_eq!(code, 1);
        assert!(r.err.contains("65535"), "names the cache major: {}", r.err);
        assert!(
            r.err.contains(&DRIFT_CACHE_MAJOR_VERSION.to_string()),
            "names the supported major: {}",
            r.err
        );
        assert!(
            r.err.to_lowercase().contains("version"),
            "names the version dimension: {}",
            r.err
        );
    }
}
