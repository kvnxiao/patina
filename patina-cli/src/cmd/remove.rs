//! `patina remove <path>` command logic (REQ-003).
//!
//! `patina remove <path>` unmanages a target. It removes the target's
//! `[[file]]` entry from its module's `patina.toml`, replaces the target on
//! disk with a regular file holding the last-applied content (so the user's
//! system stays functional), and re-journals the new managed set so
//! `patina status` treats the path as deliberately unmanaged (absent from
//! the report) rather than as an ORPHANED leftover. With `--purge` the
//! target is deleted from disk entirely instead of replaced.
//!
//! This is the first command to drive the engine re-apply under
//! [`LockPolicy::Held`](patina_core::LockPolicy): it holds ONE exclusive
//! advisory lock for the whole command (REQ-009) so the re-apply that writes
//! the fresh `<ts>.COMMIT` reuses the already-held guard instead of
//! self-contending against the command's own lock.
//!
//! ## Reconstructing the last-applied content
//!
//! The committed apply record (SPEC-0001 REQ-029) maps each target to its
//! canonical journaled source path. For a symlink or copy target the
//! last-applied content is the bytes of that source, read from the
//! repository. For a template-rendered target (the journaled source ends in
//! `.tmpl`) the journal records only a blake3 hash of the rendered bytes, so
//! the content is reconstructed by re-rendering the source through `MiniJinja`
//! against the variable context resolved at remove time — the deliberate
//! "reset to current source intent" semantics (DEC-005).
//!
//! Module-level engine semantics (planning, journaling, manifest editing,
//! repo discovery, template rendering) live in `patina_core`; this module is
//! presentation and control flow only, all output routed through the
//! [`Reporter`].

use crate::cli::RemoveArgs;
use crate::cmd::MANIFEST_FILENAME;
use crate::cmd::add::resolve_home;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::cmd::apply::current_timestamp;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::ApplyRequest;
use patina_core::EngineError;
use patina_core::ExpectedTarget;
use patina_core::LockKind;
use patina_core::LockPolicy;
use patina_core::ResolvedPlan;
use patina_core::TemplateEngine;
use patina_core::acquire_lock;
use patina_core::exclusive_timeout;
use patina_core::execute_plan;
use patina_core::expand_tilde;
use patina_core::manage_key;
use patina_core::plan_apply;
use patina_core::read_latest_commit;
use patina_core::remove_file_entry;
use patina_core::resolve_state_dir;

/// The `.tmpl` source suffix marking an implicit template-rendered target.
const TEMPLATE_SUFFIX: &str = ".tmpl";

/// Run `patina remove`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout via the
/// engine-error chain) when: the state directory or repository cannot be
/// resolved; the path is not currently managed; the journaled source cannot
/// be read or re-rendered; the target replacement fails; the manifest edit
/// fails; or the re-apply fails.
pub async fn run(
    args: &RemoveArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let home = resolve_home()?;
    let target = expand_tilde(&args.path, &home);
    let target_key = manage_key(&target);

    // REQ-009: take ONE exclusive advisory lock for the whole command. The
    // re-apply below reuses this guard via LockPolicy::Held rather than
    // acquiring a second time (which would self-contend).
    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;

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

    // Confirm before mutating (REQ-009: never mutate without consent).
    if !confirm(args, tty, reader, reporter) {
        return Ok(ExitCode::UserDeclined.code());
    }

    // Plan against the CURRENT managed set (before the entry is removed) so
    // the resolver carries the variable context a template target needs for
    // its last-applied re-render (DEC-005).
    let timestamp = current_timestamp();
    let resolved =
        plan_apply(&ApplyRequest::default(), &timestamp).context("failed to compute the plan")?;

    // Reconstruct the last-applied content (skipped for --purge, which
    // deletes the target outright).
    let content = if args.purge {
        None
    } else {
        Some(reconstruct_content(expected, &resolved)?)
    };

    // Replace the target on disk: this is remove-specific filesystem work
    // done BEFORE the re-apply. The target path is the journaled canonical
    // path of the materialized object.
    let target_path = Utf8PathBuf::from(expected.target());
    replace_target(&target_path, content.as_deref())?;

    // Remove the `[[file]]` entry from the owning module's manifest. The
    // owning module is the journaled source's parent directory.
    let source = Utf8PathBuf::from(expected.source());
    let manifest_path = owning_manifest(&source)?;
    remove_entry(&manifest_path, args.path.as_str(), &target_path)?;

    // Re-journal by re-applying under the held lock: the fresh plan now omits
    // the removed entry, so the new <ts>.COMMIT omits the target and
    // `patina status` no longer lists it. Re-plan AFTER the manifest edit so
    // the new plan reflects the removal; drive the apply with the lock we
    // already hold.
    let rejournal_ts = current_timestamp();
    let rejournaled =
        plan_apply(&ApplyRequest::default(), &rejournal_ts).context("failed to re-plan")?;
    execute_plan(
        &rejournaled,
        &ApplyRequest::default(),
        LockPolicy::Held(guard),
    )
    .await
    .context("re-apply failed")?;

    report_success(args, &target_path, reporter);
    Ok(ExitCode::Success.code())
}

/// Reconstruct the last-applied content for `expected` from its journaled
/// source.
///
/// - Symlink / copy targets: the source bytes read from the repository.
/// - Template targets (`.tmpl` source): re-rendered through `MiniJinja` against
///   the variable context the plan resolved (DEC-005).
fn reconstruct_content(expected: &ExpectedTarget, resolved: &ResolvedPlan) -> Result<Vec<u8>> {
    let source = Utf8PathBuf::from(expected.source());
    if source.as_str().ends_with(TEMPLATE_SUFFIX) {
        let body = fs_err::read_to_string(source.as_std_path())
            .with_context(|| format!("failed to read template source {source}"))?;
        let rendered = TemplateEngine::new()
            .render(&body, &resolved.resolver)
            .map_err(EngineError::from)
            .with_context(|| format!("failed to re-render template source {source}"))?;
        Ok(rendered.into_bytes())
    } else {
        fs_err::read(source.as_std_path())
            .with_context(|| format!("failed to read source {source}"))
    }
}

/// Replace the target on disk. With `content`, remove the existing
/// symlink/file and write a regular file holding the reconstructed bytes;
/// without it (`--purge`), delete the target entirely.
///
/// The existing target is removed first so a symlink is replaced by a real
/// file (writing through a symlink would clobber the repository source).
fn replace_target(target: &Utf8Path, content: Option<&[u8]>) -> Result<()> {
    remove_if_present(target)?;
    if let Some(bytes) = content {
        if let Some(parent) = target.parent() {
            fs_err::create_dir_all(parent.as_std_path())
                .with_context(|| format!("failed to create parent directory of {target}"))?;
        }
        fs_err::write(target.as_std_path(), bytes)
            .with_context(|| format!("failed to write the replacement file at {target}"))?;
    }
    Ok(())
}

/// Remove the file or symlink at `path` if it exists, treating an absent
/// target as success. Uses `symlink_metadata` so a symlink is removed as the
/// link (not followed to its destination).
fn remove_if_present(path: &Utf8Path) -> Result<()> {
    match fs_err::symlink_metadata(path.as_std_path()) {
        Ok(_) => fs_err::remove_file(path.as_std_path())
            .with_context(|| format!("failed to remove the existing target at {path}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(anyhow::Error::new(error)
                .context(format!("failed to inspect the target at {path}")))
        }
    }
}

/// Derive the owning module's manifest path from a journaled source path:
/// `<repo>/<module>/<source>` → `<repo>/<module>/patina.toml`.
fn owning_manifest(source: &Utf8Path) -> Result<Utf8PathBuf> {
    let module_dir = source
        .parent()
        .ok_or_else(|| anyhow!("the journaled source `{source}` has no parent module directory"))?;
    Ok(module_dir.join(MANIFEST_FILENAME))
}

/// Remove the `[[file]]` entry for `entry_target` from the module manifest at
/// `manifest_path`, writing the edited text back. The manifest stores the
/// target as the user wrote it (e.g. `~/.zshrc`); the writer also accepts the
/// canonical form, so both spellings are attempted before giving up.
fn remove_entry(
    manifest_path: &Utf8Path,
    entry_target: &str,
    canonical_target: &Utf8Path,
) -> Result<()> {
    let text = fs_err::read_to_string(manifest_path.as_std_path())
        .with_context(|| format!("failed to read {manifest_path}"))?;
    // The entry's `target` key matches the manifest spelling; fall back to
    // the canonical absolute path in case the manifest stored that form.
    let edited = match remove_file_entry(&text, entry_target) {
        Ok(edited) => edited,
        Err(_) => remove_file_entry(&text, canonical_target.as_str())
            .map_err(EngineError::from)
            .with_context(|| {
                format!("failed to remove the entry for {entry_target} from {manifest_path}")
            })?,
    };
    fs_err::write(manifest_path.as_std_path(), edited)
        .with_context(|| format!("failed to write {manifest_path}"))?;
    Ok(())
}

/// Confirm the removal before mutating. `--yes` proceeds unconditionally; a
/// TTY prompts; a non-TTY without `--yes` declines (no consent is possible).
fn confirm(
    args: &RemoveArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> bool {
    match (args.yes, tty) {
        (true, _) => true,
        (false, Tty::NonInteractive) => {
            reporter.warn("refusing to remove without confirmation: pass --yes in a non-TTY shell");
            false
        }
        (false, Tty::Interactive) => {
            reporter.prompt(&format!("Remove {}? [y/N] ", args.path));
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    }
}

/// Report the unmanaged-path refusal (exit 1) and return the exit code. The
/// message names the path and the three discovery sources, matching the
/// established discovery-error wording.
fn report_unmanaged(args: &RemoveArgs, reporter: &mut impl Reporter) -> i32 {
    let message = format!(
        "{} is not managed by patina (no journaled apply lists it). \
         patina resolves the repository from $PATINA_REPO, a walk-up from the \
         current directory, or the persisted default repo.",
        args.path
    );
    if args.json {
        reporter.json(&error_envelope("not_managed", args.path.as_str(), &message));
    } else {
        reporter.warn(&message);
    }
    ExitCode::Generic.code()
}

/// Report a successful removal through the reporter.
fn report_success(args: &RemoveArgs, target: &Utf8Path, reporter: &mut impl Reporter) {
    if args.json {
        reporter.json(&success_envelope(&args.path, target, args.purge));
    } else if args.purge {
        reporter.line(&format!("Removed {} and deleted it from disk.", args.path));
    } else {
        reporter.line(&format!(
            "Removed {}; replaced it with a regular file holding the last-applied content.",
            args.path
        ));
    }
}

/// Build the `--json` success envelope. Deterministic for a given input (no
/// timestamps / PIDs), so it satisfies REQ-010.
fn success_envelope(target: &Utf8Path, resolved_target: &Utf8Path, purged: bool) -> String {
    let envelope = serde_json::json!({
        "removed": target.as_str(),
        "target": resolved_target.as_str(),
        "purged": purged,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build a `--json` typed-error envelope mirroring `add`'s shape.
fn error_envelope(error: &str, path: &str, message: &str) -> String {
    let envelope = serde_json::json!({
        "error": error,
        "path": path,
        "message": message,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::reporter::BufferReporter;
    use tempfile::TempDir;

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

    fn args(purge: bool, json: bool, yes: bool) -> RemoveArgs {
        RemoveArgs {
            path: Utf8PathBuf::from("~/.zshrc"),
            purge,
            json,
            yes,
        }
    }

    #[test]
    fn confirm_yes_proceeds_without_reading() {
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            &args(false, false, true),
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
            &args(false, false, false),
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
            &args(false, false, false),
            Tty::Interactive,
            &mut reader,
            &mut reporter
        ));

        let mut reader = ScriptedReader::new(&["n\n"]);
        let mut reporter = BufferReporter::new();
        assert!(!confirm(
            &args(false, false, false),
            Tty::Interactive,
            &mut reader,
            &mut reporter
        ));
    }

    #[test]
    fn owning_manifest_is_the_source_module_dir() {
        let manifest = owning_manifest(Utf8Path::new("/repo/zsh/zshrc")).expect("manifest");
        assert_eq!(manifest, Utf8PathBuf::from("/repo/zsh/patina.toml"));
    }

    #[test]
    fn remove_if_present_tolerates_absent_target() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let absent = dir.join("not-here");
        remove_if_present(&absent).expect("absent target is a no-op");
    }

    #[test]
    fn remove_if_present_removes_a_regular_file() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let file = dir.join("present");
        fs_err::write(file.as_std_path(), b"x").expect("seed file");
        remove_if_present(&file).expect("remove present file");
        assert!(!file.exists(), "the file must be gone");
    }

    #[test]
    fn replace_target_writes_a_regular_file() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let target = dir.join("out");
        replace_target(&target, Some(b"shell-config")).expect("replace");
        assert_eq!(
            fs_err::read(target.as_std_path()).expect("read replacement"),
            b"shell-config"
        );
    }

    #[test]
    fn replace_target_purge_deletes() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let target = dir.join("out");
        fs_err::write(target.as_std_path(), b"x").expect("seed file");
        replace_target(&target, None).expect("purge");
        assert!(!target.exists(), "purge must delete the target");
    }

    #[test]
    fn remove_entry_deletes_the_matching_file_entry() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let manifest = dir.join("patina.toml");
        fs_err::write(
            manifest.as_std_path(),
            "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n\n\
             # keep me\n[[file]]\nsource = \"vimrc\"\ntarget = \"~/.vimrc\"\nmode = \"copy\"\n",
        )
        .expect("seed manifest");

        remove_entry(&manifest, "~/.zshrc", Utf8Path::new("/home/u/.zshrc")).expect("remove entry");

        let body = fs_err::read_to_string(manifest.as_std_path()).expect("read manifest");
        assert!(
            !body.contains("~/.zshrc"),
            "the removed entry's target must be gone, got: {body}"
        );
        assert!(
            body.contains("~/.vimrc"),
            "the sibling entry must survive, got: {body}"
        );
        assert!(
            body.contains("# keep me"),
            "the sibling's comment must survive, got: {body}"
        );
    }

    #[test]
    fn success_envelope_is_deterministic() {
        let target = Utf8Path::new("~/.zshrc");
        let resolved = Utf8Path::new("/home/u/.zshrc");
        let first = success_envelope(target, resolved, false);
        let second = success_envelope(target, resolved, false);
        assert_eq!(first, second, "same inputs yield byte-identical JSON");
        let doc: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert_eq!(
            doc.get("removed").and_then(serde_json::Value::as_str),
            Some("~/.zshrc")
        );
        assert_eq!(
            doc.get("purged").and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }
}
