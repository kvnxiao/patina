//! `patina remove <path>` command logic.
//!
//! Preserve a target as a regular file, or delete it with `--purge`, and
//! remove its manifest declaration. Journal and back up both paths before
//! writing either, then publish a checkpoint that omits the target.
//!
//! `remove` does not write another target or run a hook. The new commit is
//! derived from the latest record. Rollback stops at this checkpoint and
//! keeps its managed set current. An uncommitted removal recovers both paths
//! before a retry, including when its manifest declaration is already gone.
//! An ordinary write failure invokes the same recovery.
//!
//! A tree-mode leaf is refused (exit 1). A `symlink-tree` or `copy`
//! `[[directory]]` entry declares one directory and materializes a leaf per
//! source file, so no `[[file]]` edit can drop a single leaf; the manifest
//! writer edits only `[[file]]` arrays. Drop the `[[directory]]` entry, or
//! exclude the leaf with an `ignore` pattern.
//!
//! Before prompting, `remove` selects the manifest edit and leaves refused
//! targets and manifests unchanged. Selecting the edit requires planning,
//! which can fill `<state>/remotes/` for a remote-backed entry before the user
//! declines.
//!
//! `remove` holds one exclusive advisory lock for the whole command through
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
//! flow. Planning here finds the owning manifest and module variables; `remove`
//! does not execute the plan.

use crate::cli::RemoveArgs;
use crate::cmd::add::resolve_home;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::cmd::managed::Recorded;
use crate::cmd::managed::TEMPLATE_SUFFIX;
use crate::cmd::managed::acquire_state_and_lock;
use crate::cmd::managed::recover_held;
use crate::cmd::managed::refused;
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
use patina_core::remove_file_entry;

/// Run `patina remove`. Returns the process exit code.
///
/// A path that is not currently managed and a tree-mode leaf are refused with
/// exit 1, and a declined prompt returns exit 5 (`UserDeclined`). Each returns
/// its exit code through the `Ok` value, after warning when an interrupted
/// apply is pending.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout through
/// the engine-error chain) when: neither `HOME` nor `USERPROFILE` is set; the
/// path cannot be anchored; the state directory cannot be resolved or the lock
/// cannot be acquired; the committed apply record cannot be read; the plan
/// cannot be computed; no candidate manifest declares a `[[file]]` entry for
/// the target, or one cannot be read or edited; the journal directory cannot be
/// read while refusing; recovering an interrupted apply fails; the journaled
/// source cannot be read or re-rendered; the commit cannot be written; the
/// target replacement fails; or the manifest write fails.
pub(crate) fn run(
    args: &RemoveArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    let home = resolve_home()?;
    let target = anchor_input(&args.path, &home).map_err(EngineError::from)?;
    let target_key = manage_key(&target);

    let (state, _guard) = acquire_state_and_lock()?;

    let Some(recorded) = Recorded::find(&state, &target_key)? else {
        let code = report_unmanaged(args, reporter);
        return refused(&state, reporter, code);
    };
    let expected = &recorded.expected;

    let timestamp = current_timestamp();
    let mut resolved =
        plan_apply(&ApplyRequest::default(), &timestamp).context("failed to compute the plan")?;

    let target_path = Utf8PathBuf::from(expected.target());
    let owner = resolved.owner_of(&target_path);

    if let Some(owner) = owner
        && matches!(owner.mode, FileMode::SymlinkTree | FileMode::CopyTree)
    {
        let code = report_tree_leaf(args, &owner.module.manifest(), reporter);
        return refused(&state, reporter, code);
    }

    let portable = contract_home(&target, &home);
    let source = Utf8PathBuf::from(expected.source());
    let preflight = plan_manifest_edit(
        candidate_manifests(&resolved, &source, owner),
        &[portable.as_str(), args.path.as_str(), target_path.as_str()],
    );
    if let Err(error) = preflight
        && !patina_core::journal::pending_removal(&state, &target_path)
            .map_err(EngineError::from)?
    {
        return Err(error);
    }

    if !confirm(args, tty, reader, reporter) {
        return refused(&state, reporter, ExitCode::UserDeclined.code());
    }
    let recovery = recover_held(&state, reporter)?;
    if !recovery.recovered_timestamps().is_empty() {
        resolved = plan_apply(&ApplyRequest::default(), current_timestamp())
            .context("failed to compute the recovered plan")?;
    }
    let owner = resolved.owner_of(&target_path);
    let edit = plan_manifest_edit(
        candidate_manifests(&resolved, &source, owner),
        &[portable.as_str(), args.path.as_str(), target_path.as_str()],
    )?;

    let content = if args.purge {
        None
    } else {
        let vars = owner.map_or(&resolved.resolver, |owner| owner.module.resolver());
        Some(reconstruct_content(expected, vars)?)
    };

    let removal = patina_core::journal::Removal {
        state: &state,
        target: &target_path,
        content: content.as_deref(),
        manifest: &edit.manifest,
        edited_manifest: edit.edited.as_bytes(),
        remaining: recorded.without_target(),
    };
    if let Err(error) = removal.execute() {
        recover_held(&state, reporter).context("failed to recover the removal")?;
        return Err(error.into());
    }

    report_success(args, &target_path, reporter);
    Ok(ExitCode::Success.code())
}

/// Reconstruct the last-applied content for `expected` from its journaled
/// source.
///
/// - Symlink / copy targets: the source bytes read from the repository.
/// - Template targets (`.tmpl` source): re-rendered through `MiniJinja` against
///   `vars`, the resolver scoped to the declaring module.
fn reconstruct_content(expected: &ExpectedTarget, vars: &Resolver) -> Result<Vec<u8>> {
    let source = Utf8PathBuf::from(expected.source());
    if source.as_str().ends_with(TEMPLATE_SUFFIX) {
        let body = fs_err::read_to_string(source.as_std_path())
            .context("failed to read template source")?;
        let rendered = TemplateEngine::new()
            .render(&body, vars)
            .map_err(EngineError::from)
            .with_context(|| format!("failed to re-render template source {source}"))?;
        Ok(rendered.into_bytes())
    } else {
        fs_err::read(source.as_std_path()).context("failed to read source")
    }
}

#[derive(Debug)]
struct ManifestEdit {
    manifest: Utf8PathBuf,
    edited: String,
}

/// Return candidate manifests in ownership order.
///
/// A repository source selects its containing module even if a `when`
/// predicate has since changed. A remote source falls back to the module that
/// the current plan associates with the target.
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

/// Find and remove the target's `[[file]]` entry in memory.
///
/// # Errors
///
/// Returns an error naming every spelling tried when no candidate contains a
/// matching `[[file]]` entry. Missing candidate manifests are skipped. Other
/// read and parse errors stop the command.
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
        "no patina.toml declares a [[file]] entry for {}; remove a [[directory]] \
         entry by editing its manifest directly",
        spellings.join(" or ")
    ))
}

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

    fn args() -> RemoveArgs {
        RemoveArgs {
            path: Utf8PathBuf::from("~/.zshrc"),
            purge: false,
            json: false,
            yes: false,
        }
    }

    #[test]
    fn confirm_yes_proceeds_without_reading() {
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        assert!(confirm(
            &RemoveArgs {
                yes: true,
                ..args()
            },
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
    fn the_edit_drops_the_matching_file_entry_and_preserves_its_siblings() {
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

    #[test]
    fn reconstructing_from_an_unreadable_source_names_its_path_once() {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(td.path()).expect("utf8 tempdir path");
        let resolver = Resolver::new(patina_core::Builtins::current());
        for name in ["missing", "missing.tmpl"] {
            let source = dir.join(name);
            let expected = ExpectedTarget::Content {
                target: "/home/user/.rc".to_owned(),
                source: source.to_string(),
                hash: [0; 32],
                entry: 0,
                disposition: patina_core::Disposition::Create,
            };

            let err = reconstruct_content(&expected, &resolver)
                .expect_err("a missing source cannot be read");

            let rendered = format!("{err:#}");
            assert_eq!(rendered.matches(source.as_str()).count(), 1, "{rendered}");
        }
    }
}
