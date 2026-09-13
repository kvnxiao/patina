//! `patina remove <path>` command logic.
//!
//! `patina remove <path>` unmanages a target. It replaces the target on disk
//! with a regular file containing the last-applied content, so an application
//! reading the target still finds valid content. It then removes the target's
//! `[[file]]` entry from
//! its module's `patina.toml` and re-journals the new managed set. `patina
//! status` therefore treats the path as deliberately unmanaged and leaves it
//! out of the report, rather than reporting an ORPHANED leftover. With
//! `--purge` the target is deleted from disk entirely instead of replaced.
//!
//! A tree-mode leaf is refused (exit 1). A `symlink-tree` or `copy`
//! `[[directory]]` entry declares one directory and materializes a leaf per
//! source file, so no `[[file]]` edit can drop a single leaf; the manifest
//! writer edits only `[[file]]` arrays. Drop the `[[directory]]` entry, or
//! exclude the leaf with an `ignore` pattern.
//!
//! Every refusal, and the manifest edit `remove` will make, is settled
//! before the prompt, so a refused `remove` leaves the target and every
//! manifest exactly as it found them. Settling the edit means planning
//! first. Planning a remote-backed entry against a cold cache fetches its
//! pinned checkout, so a declined `remove` can still have filled
//! `<state>/remotes/`, the same way a declined `apply` does. Writing the
//! edited manifest is the one step that follows the target replacement: a
//! failure there leaves the target already replaced while its entry still
//! stands.
//!
//! `remove` holds one exclusive advisory lock for the whole command and
//! re-journals under [`LockPolicy::Held`](patina_core::LockPolicy) through
//! the shared helpers in [`crate::cmd::managed`].
//!
//! ## Reconstructing the last-applied content
//!
//! The committed apply record maps each target to its canonical journaled
//! source path. For a symlink or copy target, the last-applied content is the
//! bytes of that source, read from the repository. For a template-rendered
//! target the journal records only a blake3 hash of the rendered bytes; such a
//! target has a journaled source ending in `.tmpl`. The content is therefore
//! reconstructed by re-rendering the source through `MiniJinja`, against the
//! variable context resolved at remove time. Re-rendering is deliberate: the
//! replacement matches the template source as it stands now, not the bytes the
//! last apply wrote.
//!
//! Planning, journaling, manifest editing, repo discovery, and template
//! rendering live in `patina_core`; this module is presentation and control
//! flow.

use crate::cli::RemoveArgs;
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
use anyhow::anyhow;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::ApplyRequest;
use patina_core::ConfigWriteError;
use patina_core::EngineError;
use patina_core::ExpectedTarget;
use patina_core::FileMode;
use patina_core::ModuleContext;
use patina_core::ResolvedPlan;
use patina_core::Resolver;
use patina_core::TargetOwner;
use patina_core::TemplateEngine;
use patina_core::anchor_input;
use patina_core::contract_home;
use patina_core::current_timestamp;
use patina_core::manage_key;
use patina_core::plan_apply;
use patina_core::read_latest_commit;
use patina_core::remove_file_entry;

/// Run `patina remove`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout through
/// the engine-error chain) when: the state directory or repository cannot be
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
    let target = anchor_input(&args.path, &home).map_err(EngineError::from)?;
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

    // Plan against the still-current managed set, before the entry is
    // removed, so the resolver has the variable context a template target
    // needs for its last-applied re-render and the declaring module is still
    // resolvable.
    let timestamp = current_timestamp();
    let resolved =
        plan_apply(&ApplyRequest::default(), &timestamp).context("failed to compute the plan")?;

    // The target path is read from the journal: the canonical path of the
    // materialized object, not the user's spelling of it.
    let target_path = Utf8PathBuf::from(expected.target());
    let owner = resolved.owner_of(&target_path);

    if let Some(owner) = owner
        && matches!(owner.mode, FileMode::SymlinkTree | FileMode::CopyTree)
    {
        return Ok(report_tree_leaf(args, &owner.module.manifest(), reporter));
    }

    // Settle the manifest edit before touching the target, so a target whose
    // entry cannot be dropped stays exactly as `remove` found it. Try the
    // portable form, the user's argument, and the journaled path to match
    // manifests written by both current and older `add` versions.
    let portable = contract_home(&target, &home);
    let source = Utf8PathBuf::from(expected.source());
    let edit = plan_manifest_edit(
        candidate_manifests(&resolved, &source, owner),
        &[portable.as_str(), args.path.as_str(), target_path.as_str()],
    )?;

    if !confirm(args, tty, reader, reporter) {
        return Ok(ExitCode::UserDeclined.code());
    }

    let content = if args.purge {
        None
    } else {
        let vars = owner.map_or(&resolved.resolver, |owner| owner.module.resolver());
        Some(reconstruct_content(expected, vars)?)
    };

    replace_target(&target_path, content.as_deref())?;
    fs_err::write(edit.manifest.as_std_path(), &edit.edited)
        .with_context(|| format!("failed to write {}", edit.manifest))?;

    // The re-plan runs after the manifest edit, so the fresh <ts>.COMMIT
    // omits the removed target and `patina status` stops listing it.
    rejournal(guard).await?;

    report_success(args, &target_path, reporter);
    Ok(ExitCode::Success.code())
}

/// Reconstruct the last-applied content for `expected` from its journaled
/// source.
///
/// - Symlink / copy targets: the source bytes read from the repository.
/// - Template targets (`.tmpl` source): re-rendered through `MiniJinja` against
///   `vars`, the resolver the declaring module scopes.
fn reconstruct_content(expected: &ExpectedTarget, vars: &Resolver) -> Result<Vec<u8>> {
    let source = Utf8PathBuf::from(expected.source());
    if source.as_str().ends_with(TEMPLATE_SUFFIX) {
        let body = fs_err::read_to_string(source.as_std_path())
            .with_context(|| format!("failed to read template source {source}"))?;
        let rendered = TemplateEngine::new()
            .render(&body, vars)
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
/// file (writing through a symlink would overwrite the repository source).
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

/// The manifest edit `remove` will apply, computed before any mutation.
#[derive(Debug)]
struct ManifestEdit {
    /// The manifest that declared the entry.
    manifest: Utf8PathBuf,
    /// That manifest's text with the entry dropped.
    edited: String,
}

/// The manifests that may declare the journaled entry, most authoritative
/// first.
///
/// The journaled `source` decides it. A repository source lies under the
/// module directory that declared it, whatever depth the `source` key spells
/// and whichever entries are active on this host, so a `when` predicate that
/// has flipped since the apply cannot redirect the edit at another module's
/// declaration of the same target. Only a remote-backed source, which lives
/// under the state directory rather than the repository, falls through to the
/// module the current plan materializes the target from.
fn candidate_manifests(
    resolved: &ResolvedPlan,
    source: &Utf8Path,
    owner: Option<TargetOwner<'_>>,
) -> Vec<Utf8PathBuf> {
    let declaring = resolved
        .modules
        .iter()
        .find(|module| source.starts_with(module.directory()))
        .map(ModuleContext::manifest);
    let owning = owner.map(|owner| owner.module.manifest());
    let mut candidates: Vec<Utf8PathBuf> = declaring.into_iter().chain(owning).collect();
    candidates.dedup();
    candidates
}

/// Find the manifest declaring the target and compute its text without that
/// entry, trying each candidate manifest against each spelling of the target.
///
/// # Errors
///
/// Returns an error naming every spelling tried when no candidate declares a
/// `[[file]]` entry for the target. A `[[directory]]` entry lands here too,
/// because the manifest writer edits only `[[file]]` arrays. A manifest that
/// cannot be read, or whose TOML does not parse, fails the command rather than
/// being skipped: the target is declared somewhere, and skipping would report
/// the wrong reason for not finding it.
fn plan_manifest_edit(
    candidates: impl IntoIterator<Item = Utf8PathBuf>,
    spellings: &[&str],
) -> Result<ManifestEdit> {
    for manifest in candidates {
        let text = match fs_err::read_to_string(manifest.as_std_path()) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!("failed to read {manifest}")));
            }
        };
        for spelling in spellings {
            match remove_file_entry(&text, spelling) {
                Ok(edited) => return Ok(ManifestEdit { manifest, edited }),
                Err(ConfigWriteError::EntryNotFound { .. }) => {}
                Err(error) => {
                    return Err(EngineError::from(error))
                        .with_context(|| format!("failed to edit {manifest}"));
                }
            }
        }
    }
    Err(anyhow!(
        "no patina.toml declares a [[file]] entry for {}; a [[directory]] entry is \
         unmanaged by editing its manifest directly",
        spellings.join(" or ")
    ))
}

/// Report the tree-leaf refusal (exit 1) and return the exit code.
fn report_tree_leaf(args: &RemoveArgs, manifest: &Utf8Path, reporter: &mut impl Reporter) -> i32 {
    let message = format!(
        "{} is one leaf of a tree-mode [[directory]] entry declared in {manifest}. \
         The entry declares the directory, not the leaf, so no single leaf can be \
         unmanaged on its own. Drop the [[directory]] entry, or exclude the leaf \
         with an ignore pattern.",
        args.path
    );
    if args.json {
        reporter.json(&error_envelope("tree_leaf", args.path.as_str(), &message));
    } else {
        reporter.warn(&message);
    }
    ExitCode::Generic.code()
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
            reporter.confirm(&format!("Remove {}?", args.path));
            let answer = reader.read_line().unwrap_or_default();
            matches!(answer.trim(), "y" | "Y")
        }
    }
}

/// Report the unmanaged-path refusal (exit 1) and return the exit code.
///
/// The message includes the path and every discovery source: `$PATINA_REPO`,
/// the walk-up from the current directory, and the persisted default. It
/// repeats the established discovery-error wording, so `remove` and `promote`
/// explain an unmanaged path the same way.
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
        return;
    }
    let path = paint(reporter.styles().path, args.path.as_str());
    if args.purge {
        reporter.line(&format!("Removed {path} and deleted it from disk."));
    } else {
        reporter.line(&format!(
            "Removed {path}; replaced it with a regular file holding the last-applied content."
        ));
    }
}

/// Build the `--json` success envelope. Deterministic for a given input (no
/// timestamps / PIDs).
fn success_envelope(target: &Utf8Path, resolved_target: &Utf8Path, purged: bool) -> String {
    let envelope = serde_json::json!({
        "removed": target.as_str(),
        "target": resolved_target.as_str(),
        "purged": purged,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build the `--json` typed-error envelope: `error`, `path`, `message`. `init`
/// and `add` emit the same error keys.
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
            "the refusal must include --yes, got: {}",
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
    fn the_edit_drops_the_matching_file_entry_and_keeps_its_siblings() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let manifest = dir.join("patina.toml");
        fs_err::write(
            manifest.as_std_path(),
            "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"symlink\"\n\n\
             # keep me\n[[file]]\nsource = \"vimrc\"\ntarget = \"~/.vimrc\"\nmode = \"copy\"\n",
        )
        .expect("seed manifest");

        let edit = plan_manifest_edit([manifest.clone()], &["~/.zshrc", "/home/u/.zshrc"])
            .expect("remove entry");

        assert_eq!(edit.manifest, manifest);
        let body = edit.edited;
        assert!(
            !body.contains("~/.zshrc"),
            "the removed entry's target must be gone, got: {body}"
        );
        assert!(
            body.contains("~/.vimrc"),
            "the sibling entry must be preserved, got: {body}"
        );
        assert!(
            body.contains("# keep me"),
            "the sibling's comment must be preserved, got: {body}"
        );
    }

    #[test]
    fn the_edit_falls_through_to_a_later_spelling() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let manifest = dir.join("patina.toml");
        fs_err::write(
            manifest.as_std_path(),
            "[[file]]
source = \"zshrc\"
target = \".zshrc\"
mode = \"symlink\"
",
        )
        .expect("seed manifest");

        let edit = plan_manifest_edit(
            [manifest.clone()],
            &["~/.zshrc", ".zshrc", "/home/u/.zshrc"],
        )
        .expect("a later spelling matches");

        let body = edit.edited;
        assert!(
            !body.contains("[[file]]"),
            "the entry matched by the second spelling must be gone, got: {body}"
        );
    }

    #[test]
    fn a_declaration_free_manifest_reports_every_spelling_tried() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let manifest = dir.join("patina.toml");
        fs_err::write(
            manifest.as_std_path(),
            "[[file]]
source = \"vimrc\"
target = \"~/.vimrc\"
mode = \"copy\"
",
        )
        .expect("seed manifest");

        let error = plan_manifest_edit([manifest.clone()], &["~/.zshrc", "/home/u/.zshrc"])
            .expect_err("no spelling matches");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("~/.zshrc") && rendered.contains("/home/u/.zshrc"),
            "the error must name both spellings, got: {rendered}"
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
