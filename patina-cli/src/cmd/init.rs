//! `patina init` command logic (REQ-001).
//!
//! `patina init [path]` scaffolds a root `patina.toml` at the target
//! directory (the positional argument, or the current working directory
//! when omitted), persists the absolute canonical path of that directory
//! to the per-machine state directory's `default_repo` file, and prints a
//! next-step hint pointing at `patina add`. If a `patina.toml` already
//! exists at the target, the command refuses with a typed error and exits
//! 1 without touching the existing file or the state directory.
//!
//! `init` is a mutating command (REQ-009): it acquires the engine's
//! exclusive advisory lock at `<state>/lock` before any filesystem
//! mutation. The manifest-write engine semantics live in
//! `patina_core::config` ([`scaffold_root_manifest`]) and the persisted
//! pointer in `patina_core::discovery`
//! ([`write_persisted_default`]); this module is presentation and control
//! flow only, all output routed through the [`Reporter`].
//!
//! ## Determinism (REQ-010)
//!
//! Both the success and the already-initialized failure paths produce
//! byte-stable stdout: the success JSON names only the created path and the
//! persisted pointer, and the failure error names only the existing file
//! path. Neither carries the manifest's `created_at` timestamp, so two runs
//! against the same target produce identical stdout (CHK-017).

use crate::cli::InitArgs;
use crate::cmd::MANIFEST_FILENAME;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
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
/// Returns an error when the target `patina.toml` already exists (exit 1),
/// when the exclusive lock cannot be acquired within the timeout (exit 4
/// via the engine-error chain), or when state-directory resolution, the
/// manifest write, canonicalization, or the persisted-pointer write fails
/// at the engine level (exit 1).
#[expect(
    clippy::unused_async,
    reason = "the subcommand dispatch in main.rs awaits every command uniformly; init's work is synchronous filesystem and lock I/O but keeps the async signature for parity."
)]
pub async fn run(args: &InitArgs, reporter: &mut impl Reporter) -> Result<i32> {
    let target = resolve_target_path(args.path.as_deref())?;
    let manifest_path = target.join(MANIFEST_FILENAME);

    // Refuse before acquiring the lock or mutating anything: the existing
    // manifest is left byte-identical and the state directory untouched.
    if manifest_path.exists() {
        return Ok(refuse_existing(&manifest_path, args.json, reporter));
    }

    // REQ-009: take the exclusive advisory lock before any mutation. Map
    // the lock error through `EngineError` so a contention timeout reaches
    // the exit-code-4 mapping in `ExitCode::from_error_chain`.
    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;

    // Re-check after acquiring the lock: a concurrent `init` may have
    // created the manifest while we waited on the lock.
    if manifest_path.exists() {
        return Ok(refuse_existing(&manifest_path, args.json, reporter));
    }

    // Create the target directory now that we hold the lock, so no filesystem
    // mutation precedes lock acquisition (REQ-009). Creating a directory that
    // already exists is idempotent (the CWD case is always a no-op).
    fs_err::create_dir_all(target.as_std_path())
        .with_context(|| format!("failed to create target directory {target}"))?;

    let manifest = scaffold_root_manifest(&rfc3339_now());
    fs_err::write(manifest_path.as_std_path(), manifest)
        .with_context(|| format!("failed to write {manifest_path}"))?;

    // The persisted pointer must be the canonical absolute repo path so a
    // later bare `patina apply` resolves the same directory regardless of
    // how `init` was invoked (REQ-001).
    let canonical = canonicalize_path(&target).map_err(EngineError::from)?;
    write_persisted_default(&state, &canonical).map_err(EngineError::from)?;

    if args.json {
        reporter.json(&json_envelope(&canonical, &state));
    } else {
        reporter.line(&format!("Initialized root patina.toml at {manifest_path}"));
        reporter.line(&next_step_hint(&canonical));
    }
    Ok(ExitCode::Success.code())
}

/// Handle the already-initialized refusal (REQ-001) and return exit code 1.
/// The existing manifest is left untouched and the state directory is never
/// written. Under `--json` a deterministic error document naming the
/// existing path goes to stdout, so the failing `--json` stdout is itself
/// byte-stable across reruns per REQ-010 and CHK-017; otherwise the message
/// goes to stderr as a warning.
fn refuse_existing(manifest_path: &Utf8Path, json: bool, reporter: &mut impl Reporter) -> i32 {
    let message = format!("{manifest_path} already exists");
    if json {
        reporter.json(&error_envelope(manifest_path, &message));
    } else {
        reporter.warn(&message);
    }
    ExitCode::Generic.code()
}

/// Resolve the target directory path, performing no filesystem mutation.
///
/// A positional path is returned verbatim; when omitted the current working
/// directory is used. The directory is created by the caller under the
/// exclusive lock (REQ-009), not here, so that no mutation precedes the lock.
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
/// human success path (REQ-001 done-when).
fn next_step_hint(target: &Utf8Path) -> String {
    format!("Next: run `patina add {target}` to register an existing dotfile.")
}

/// Build the `--json` success envelope: the canonical repo path and the
/// persisted-pointer path. Both fields are deterministic for a given
/// target, so two successful runs produce byte-identical stdout (REQ-010).
fn json_envelope(canonical: &Utf8Path, state: &Utf8Path) -> String {
    let envelope = serde_json::json!({
        "initialized": canonical.as_str(),
        "default_repo": patina_core::default_repo_pointer_path(state).as_str(),
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build the `--json` already-exists error envelope: the typed error tag, the
/// existing manifest path, and the human message. Deterministic for a given
/// path, so the failing `--json` stdout is byte-stable across reruns (REQ-010,
/// CHK-017). Mirrors [`json_envelope`] for the success path.
fn error_envelope(manifest_path: &Utf8Path, message: &str) -> String {
    let envelope = serde_json::json!({
        "error": "already_exists",
        "path": manifest_path.as_str(),
        "message": message,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// The manifest's `created_at` RFC 3339 timestamp. This is the only place
/// `init` emits a wall-clock value, and it lands in the configuration file
/// (not stdout), so stdout determinism (REQ-010) is preserved.
fn rfc3339_now() -> String {
    jiff::Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_step_hint_names_target_and_add() {
        let hint = next_step_hint(Utf8Path::new("/tmp/dot"));
        assert_eq!(
            hint,
            "Next: run `patina add /tmp/dot` to register an existing dotfile."
        );
    }

    #[test]
    fn json_envelope_is_deterministic_and_names_both_paths() {
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
        assert_eq!(
            doc.get("default_repo").and_then(serde_json::Value::as_str),
            Some("/state/patina/default_repo")
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
