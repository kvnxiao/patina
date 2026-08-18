//! `patina init` command logic.
//!
//! `patina init [path]` scaffolds a root `patina.toml` at the target
//! directory. The target is the positional argument, or the current working
//! directory when omitted. `init` then persists that directory's absolute
//! canonical path to the per-machine state directory's `default_repo` file,
//! and prints a next-step hint pointing at `patina add`. A `patina.toml`
//! already at the target is a refusal (exit 1). Neither that file nor the state
//! directory is touched.
//!
//! `init` is a mutating command: it acquires the engine's exclusive advisory
//! lock at `<state>/lock` before any filesystem mutation. The manifest write
//! lives in `patina_core::config` ([`scaffold_root_manifest`]) and the
//! persisted pointer in `patina_core::discovery`
//! ([`write_persisted_default`]); this module is presentation and control
//! flow.
//!
//! ## Determinism
//!
//! Both the success and the already-initialized failure paths produce
//! byte-stable stdout: the success JSON includes only the created path and the
//! persisted pointer, and the failure error includes only the existing file
//! path. Neither includes the manifest's `created_at` timestamp, so two runs
//! against the same target produce identical stdout.

use crate::cli::InitArgs;
use crate::cmd::MANIFEST_FILENAME;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::paint;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::EngineError;
use patina_core::LockKind;
use patina_core::acquire_lock;
use patina_core::canonicalize_path;
use patina_core::exclusive_timeout;
use patina_core::resolve_state_dir;
use patina_core::scaffold_root_manifest;
use patina_core::write_persisted_default;

/// Run `patina init`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error when:
///
/// - the exclusive lock cannot be acquired within the timeout (exit 4, through
///   the engine-error chain);
/// - state-directory resolution, the target-directory creation, the manifest
///   write, canonicalization, or the persisted-pointer write fails (exit 1).
///
/// An existing `patina.toml` at the target is not an error. The target path
/// reports the refusal and returns exit code 1.
#[expect(
    clippy::unused_async,
    reason = "the subcommand dispatch in main.rs awaits every command uniformly; init's work is synchronous filesystem and lock I/O but keeps the async signature for parity."
)]
pub async fn run(args: &InitArgs, reporter: &mut impl Reporter) -> Result<i32> {
    let target = resolve_target_path(args.path.as_deref())?;
    let manifest_path = target.join(MANIFEST_FILENAME);

    // Refuse before the lock and before any mutation. The existing manifest
    // stays byte-identical, and the state directory is untouched.
    if manifest_path.exists() {
        return Ok(refuse_existing(&manifest_path, args.json, reporter));
    }

    // Map the lock error through `EngineError` so a contention timeout maps to
    // exit 4 in `ExitCode::from_error_chain`.
    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;

    // Re-check under the lock: a concurrent `init` may have created the
    // manifest during the wait.
    if manifest_path.exists() {
        return Ok(refuse_existing(&manifest_path, args.json, reporter));
    }

    fs_err::create_dir_all(target.as_std_path())
        .with_context(|| format!("failed to create target directory {target}"))?;

    let manifest = scaffold_root_manifest(&rfc3339_now());
    fs_err::write(manifest_path.as_std_path(), manifest)
        .with_context(|| format!("failed to write {manifest_path}"))?;

    // The persisted pointer must be the canonical absolute repo path so a
    // later bare `patina apply` resolves the same directory regardless of
    // how `init` was invoked.
    let canonical = canonicalize_path(&target).map_err(EngineError::from)?;
    write_persisted_default(&state, &canonical).map_err(EngineError::from)?;

    if args.json {
        reporter.json(&json_envelope(&canonical, &state));
    } else {
        let styles = reporter.styles();
        reporter.line(&format!(
            "Initialized root patina.toml at {}",
            paint(styles.path, manifest_path.as_str())
        ));
        reporter.line(&paint(styles.hint, &next_step_hint(&canonical)));
    }
    Ok(ExitCode::Success.code())
}

/// Report the already-initialized refusal and return exit code 1. Under
/// `--json` the typed error document is written to stdout. Otherwise the
/// message is written to stderr as a warning.
fn refuse_existing(manifest_path: &Utf8Path, json: bool, reporter: &mut impl Reporter) -> i32 {
    let message = format!("{manifest_path} already exists");
    if json {
        reporter.json(&error_envelope(manifest_path, &message));
    } else {
        reporter.warn(&message);
    }
    ExitCode::Generic.code()
}

/// Resolve the target directory path. A positional path is used verbatim. With
/// no positional path, the current working directory is used.
///
/// The caller creates the directory under the exclusive lock, so no
/// filesystem mutation precedes the lock.
fn resolve_target_path(path: Option<&Utf8Path>) -> Result<Utf8PathBuf> {
    if let Some(path) = path {
        Ok(path.to_owned())
    } else {
        let cwd = std::env::current_dir().context("failed to read the current directory")?;
        Utf8PathBuf::from_path_buf(cwd)
            .map_err(|p| anyhow!("current directory `{}` is not valid UTF-8", p.display()))
    }
}

/// The single-line next-step hint printed as the final stdout line on the
/// human success path.
fn next_step_hint(target: &Utf8Path) -> String {
    format!("Next: run `patina add {target}` to register an existing dotfile.")
}

/// Build the `--json` success envelope: the canonical repo path and the
/// persisted-pointer path. Both fields are deterministic for a given
/// target, so two successful runs produce byte-identical stdout.
fn json_envelope(canonical: &Utf8Path, state: &Utf8Path) -> String {
    let envelope = serde_json::json!({
        "initialized": canonical.as_str(),
        "default_repo": patina_core::default_repo_pointer_path(state).as_str(),
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build the `--json` already-exists error envelope: `error`, `path`,
/// `message`. `add` and `remove` emit the same error keys. Deterministic for a
/// given path, so the failing `--json` stdout is byte-stable across reruns.
fn error_envelope(manifest_path: &Utf8Path, message: &str) -> String {
    let envelope = serde_json::json!({
        "error": "already_exists",
        "path": manifest_path.as_str(),
        "message": message,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// The manifest's `created_at` RFC 3339 timestamp. The only wall-clock value
/// `init` emits, and it is written to the configuration file rather than to
/// stdout.
fn rfc3339_now() -> String {
    jiff::Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_step_hint_includes_target_and_add() {
        let hint = next_step_hint(Utf8Path::new("/tmp/dot"));
        assert_eq!(
            hint,
            "Next: run `patina add /tmp/dot` to register an existing dotfile."
        );
    }

    #[test]
    fn json_envelope_is_deterministic_and_includes_both_paths() {
        let canonical = Utf8Path::new("/repo/dot");
        let state = Utf8Path::new("/state/patina");
        let first = json_envelope(canonical, state);
        let second = json_envelope(canonical, state);
        assert_eq!(first, second, "same inputs must yield byte-identical JSON");

        let doc: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert_eq!(
            doc.get("initialized").and_then(serde_json::Value::as_str),
            Some("/repo/dot")
        );
        // `default_repo_pointer_path` joins with the OS separator (`\` on
        // Windows, `/` elsewhere), so a hardcoded forward-slash literal would
        // fail on Windows. The expectation comes from the same public API the
        // envelope uses.
        assert_eq!(
            doc.get("default_repo").and_then(serde_json::Value::as_str),
            Some(patina_core::default_repo_pointer_path(state).as_str())
        );
    }

    #[test]
    fn refuse_existing_json_emits_typed_error_to_stdout() {
        use crate::output::reporter::BufferReporter;
        let mut r = BufferReporter::new();
        let path = Utf8Path::new("/repo/patina.toml");
        let code = refuse_existing(path, true, &mut r);
        assert_eq!(code, ExitCode::Generic.code());
        assert!(r.err.is_empty(), "the --json refusal must not write stderr");
        let doc: serde_json::Value = serde_json::from_str(r.out.trim()).expect("one JSON doc");
        assert_eq!(
            doc.get("error").and_then(serde_json::Value::as_str),
            Some("already_exists")
        );
        assert_eq!(
            doc.get("path").and_then(serde_json::Value::as_str),
            Some("/repo/patina.toml")
        );
    }

    #[test]
    fn refuse_existing_human_warns_to_stderr() {
        use crate::output::reporter::BufferReporter;
        let mut r = BufferReporter::new();
        let path = Utf8Path::new("/repo/patina.toml");
        let code = refuse_existing(path, false, &mut r);
        assert_eq!(code, ExitCode::Generic.code());
        assert!(r.out.is_empty(), "the human refusal must not write stdout");
        assert!(r.err.contains("already exists"));
        assert!(r.err.contains("/repo/patina.toml"));
    }

    #[test]
    fn rfc3339_now_parses_as_a_timestamp() {
        // The manifest's created_at must be a parseable RFC 3339 string so
        // the scaffolded file round-trips through the TOML datetime parser.
        let now = rfc3339_now();
        now.parse::<jiff::Timestamp>()
            .expect("rfc3339_now must produce a parseable timestamp");
    }
}
