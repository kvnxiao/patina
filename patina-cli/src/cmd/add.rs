//! `patina add <path>` command logic (REQ-002).
//!
//! `patina add <path>` brings an existing dotfile under management. It
//! resolves the repository root, stages the target file's bytes into a
//! module subdirectory (`<repo>/<module>/<source>`), appends a `[[file]]`
//! entry to that module's `patina.toml` (creating it if absent), and leaves
//! the original target as a regular file containing the original bytes. The
//! command does NOT drive the engine apply path — materialization (turning
//! the target into a symlink / copy / rendered template) is deferred to a
//! later `patina apply` (move-on-add, resolved open-question (a): the bytes
//! move into the repo, while the target stays a plain file so apply
//! converges it).
//!
//! `add` is a mutating command (REQ-009): it acquires the engine's
//! exclusive advisory lock at `<state>/lock` before any filesystem
//! mutation, so two concurrent `add` invocations against the same state
//! directory serialize.
//!
//! ## Mode and module resolution
//!
//! The module name comes from `--module`, else an interactive prompt;
//! in a non-TTY shell without `--module` the command exits 1 with a typed
//! error naming the missing flag. The mode comes from exactly one of the
//! `--symlink` / `--copy` / `--template` flags (clap enforces at-most-one,
//! exit 2 on two), else an interactive prompt; in a non-TTY shell without a
//! mode flag the command exits 1.
//!
//! Module-level engine semantics (manifest editing, repo discovery,
//! tilde expansion, canonicalization) live in `patina_core`; this module
//! is presentation and control flow only, all output routed through the
//! [`Reporter`].

use crate::cli::AddArgs;
use crate::cmd::MANIFEST_FILENAME;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::EngineError;
use patina_core::FileMode;
use patina_core::LockKind;
use patina_core::acquire_lock;
use patina_core::append_file_entry;
use patina_core::canonicalize_path;
use patina_core::discover_modules;
use patina_core::exclusive_timeout;
use patina_core::expand_tilde;
use patina_core::parse_module_config;
use patina_core::resolve_repository_root;
use patina_core::resolve_state_dir;

/// The materialization mode the user selected for the new entry.
///
/// `add` exposes only these three of the engine's five [`FileMode`]
/// variants; `symlink-dir` / `copy-tree` are a v1.0 non-goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddMode {
    /// `--symlink`: filed as `mode = "symlink"`.
    Symlink,
    /// `--copy`: filed as `mode = "copy"`.
    Copy,
    /// `--template`: filed as an implicit-template `.tmpl` source (no
    /// `mode` key; the engine derives [`FileMode::TemplateRender`] from the
    /// `.tmpl` suffix).
    Template,
}

impl AddMode {
    /// The single lowercase label shown in prompts and the JSON envelope.
    fn label(self) -> &'static str {
        match self {
            AddMode::Symlink => "symlink",
            AddMode::Copy => "copy",
            AddMode::Template => "template",
        }
    }
}

/// Run `patina add`. Returns the process exit code.
///
/// # Errors
///
/// Returns an error (exit 1, or exit 4 on a lock-acquisition timeout via
/// the engine-error chain) when: the repository root cannot be resolved;
/// the module is missing in a non-TTY shell; the mode is missing in a
/// non-TTY shell; the target path is already managed; the target file does
/// not exist or cannot be moved; or the manifest read/write fails.
#[expect(
    clippy::unused_async,
    reason = "the subcommand dispatch in main.rs awaits every command uniformly; add's work is synchronous filesystem and lock I/O but keeps the async signature for parity."
)]
pub async fn run(
    args: &AddArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<i32> {
    // Resolve the mode and module up front (before any lock or mutation),
    // failing fast on the non-TTY-missing-input paths (REQ-002).
    let Some(mode) = resolve_mode(args, tty, reader, reporter)? else {
        return Ok(ExitCode::Generic.code());
    };
    let Some(module) = resolve_module(args, tty, reader, reporter)? else {
        return Ok(ExitCode::Generic.code());
    };

    let repo_root = resolve_repository_root().map_err(EngineError::from)?;
    let home = resolve_home()?;
    let target = expand_tilde(&args.path, &home);

    // REQ-009: take the exclusive advisory lock before any mutation.
    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;

    // Refuse a path that is already managed (REQ-002): scan every module's
    // manifest for a `[[file]]` whose target resolves to the same path.
    if let Some(existing_module) = find_managed(&repo_root, &target, &home)? {
        let message = format!(
            "{} is already managed by module `{existing_module}`",
            args.path
        );
        if args.json {
            reporter.json(&error_envelope(
                "already_managed",
                args.path.as_str(),
                &message,
            ));
        } else {
            reporter.warn(&message);
        }
        return Ok(ExitCode::Generic.code());
    }

    let file_name = target
        .file_name()
        .ok_or_else(|| anyhow!("the path `{}` has no file name", args.path))?;
    // The repository source name strips the leading dot of a dotfile so the
    // repo holds `zsh/zshrc`, not `zsh/.zshrc` (REQ-002 done-when:
    // `~/.zshrc` → `<repo>/zsh/zshrc`).
    let basename = repo_source_name(file_name);
    // A `--template` source records the `.tmpl` suffix so the engine derives
    // the implicit template mode; the on-disk moved file keeps that suffix.
    let source = match mode {
        AddMode::Template => format!("{basename}.tmpl"),
        AddMode::Symlink | AddMode::Copy => basename.clone(),
    };

    let module_dir = repo_root.join(&module);
    fs_err::create_dir_all(module_dir.as_std_path())
        .with_context(|| format!("failed to create module directory {module_dir}"))?;
    let dest = module_dir.join(&source);

    stage_into_repo(&target, &dest)?;

    // Append the entry to the module manifest, creating it if absent. The
    // target is stored as the user wrote it (e.g. `~/.zshrc`) so the
    // manifest stays portable across machines.
    let manifest_path = module_dir.join(MANIFEST_FILENAME);
    let existing_text = read_manifest_text(&manifest_path)?;
    let new_text = append_file_entry(&existing_text, &source, args.path.as_str(), file_mode(mode))
        .map_err(EngineError::from)?;
    fs_err::write(manifest_path.as_std_path(), new_text)
        .with_context(|| format!("failed to write {manifest_path}"))?;

    if args.json {
        reporter.json(&success_envelope(&args.path, &dest, &module, mode));
    } else {
        reporter.line(&format!(
            "Added {} to module `{module}` as {} (run `patina apply` to materialize).",
            args.path,
            mode.label()
        ));
    }
    Ok(ExitCode::Success.code())
}

/// Scan every module manifest for a `[[file]]` whose target resolves to the
/// same absolute path as `target`. Returns the owning module's name on a match.
///
/// Targets are compared by tilde-expanded form (the manifest may store a
/// `~`-relative target while the input is absolute, or vice versa), without
/// touching the filesystem.
fn find_managed(
    repo_root: &Utf8Path,
    target: &Utf8Path,
    home: &Utf8Path,
) -> Result<Option<String>> {
    let modules = discover_modules(repo_root).map_err(EngineError::from)?;
    for module in modules {
        let manifest = module.path.join(MANIFEST_FILENAME);
        let config = parse_module_config(&manifest).map_err(EngineError::from)?;
        for entry in &config.files {
            for entry_target in &entry.targets {
                if expand_tilde(entry_target, home) == target {
                    return Ok(Some(module.name));
                }
            }
        }
    }
    Ok(None)
}

/// Stage the target's bytes into the repository at `to`, leaving the
/// original `from` in place as a regular file.
///
/// `add` brings a dotfile under management without materializing it: the
/// repository gets the source bytes, and the target stays a plain file so a
/// subsequent `patina apply` converges it into the declared mode (REQ-002,
/// CHK-003 — `~/.zshrc` is still a regular file with the original bytes,
/// apply has not run). The bytes are copied (not renamed away) precisely so
/// the original target survives.
fn stage_into_repo(from: &Utf8Path, to: &Utf8Path) -> Result<()> {
    fs_err::copy(from.as_std_path(), to.as_std_path())
        .with_context(|| format!("failed to copy {from} into the repository at {to}"))?;
    Ok(())
}

/// Read the existing module manifest text, treating a missing file as the
/// empty document so a fresh module's manifest is created on first `add`.
fn read_manifest_text(manifest_path: &Utf8Path) -> Result<String> {
    if manifest_path.exists() {
        fs_err::read_to_string(manifest_path.as_std_path())
            .with_context(|| format!("failed to read {manifest_path}"))
    } else {
        Ok(String::new())
    }
}

/// Resolve the selected mode, prompting in a TTY when no flag is set.
/// Returns `Ok(None)` for the non-TTY-without-mode refusal (exit 1).
fn resolve_mode(
    args: &AddArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<Option<AddMode>> {
    // clap's mode group guarantees at most one flag is set.
    if args.symlink {
        return Ok(Some(AddMode::Symlink));
    }
    if args.copy {
        return Ok(Some(AddMode::Copy));
    }
    if args.template {
        return Ok(Some(AddMode::Template));
    }
    match tty {
        Tty::NonInteractive => {
            reporter
                .warn("a mode flag is required in a non-TTY shell: pass one of --symlink, --copy, or --template");
            Ok(None)
        }
        Tty::Interactive => {
            reporter.prompt("Mode? [symlink/copy/template] ");
            let answer = reader.read_line().unwrap_or_default();
            match answer.trim() {
                "symlink" => Ok(Some(AddMode::Symlink)),
                "copy" => Ok(Some(AddMode::Copy)),
                "template" => Ok(Some(AddMode::Template)),
                other => Err(anyhow!(
                    "unrecognized mode `{other}`; expected symlink, copy, or template"
                )),
            }
        }
    }
}

/// Resolve the module name, prompting in a TTY when `--module` is absent.
/// Returns `Ok(None)` for the non-TTY-without-module refusal (exit 1).
fn resolve_module(
    args: &AddArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<Option<String>> {
    if let Some(module) = &args.module {
        return Ok(Some(module.clone()));
    }
    match tty {
        Tty::NonInteractive => {
            reporter.warn("--module is required in a non-TTY shell");
            Ok(None)
        }
        Tty::Interactive => {
            reporter.prompt("Module name? ");
            let answer = reader.read_line().unwrap_or_default();
            let name = answer.trim();
            if name.is_empty() {
                Err(anyhow!("a module name is required"))
            } else {
                Ok(Some(name.to_owned()))
            }
        }
    }
}

/// Derive the repository source name from a target's file name, stripping a
/// single leading dot so a dotfile lands as `zsh/zshrc` rather than
/// `zsh/.zshrc` (REQ-002 done-when). A name that is exactly `.` (or empty
/// after stripping) is returned verbatim so the result is never empty.
fn repo_source_name(file_name: &str) -> String {
    match file_name.strip_prefix('.') {
        Some(rest) if !rest.is_empty() => rest.to_owned(),
        _ => file_name.to_owned(),
    }
}

/// Map the CLI mode onto the engine [`FileMode`] the writer accepts.
fn file_mode(mode: AddMode) -> FileMode {
    match mode {
        AddMode::Symlink => FileMode::Symlink,
        AddMode::Copy => FileMode::Copy,
        AddMode::Template => FileMode::TemplateRender,
    }
}

/// Resolve the user's home directory for tilde expansion, reading `$HOME`
/// then `$USERPROFILE` (the Windows fallback).
pub(crate) fn resolve_home() -> Result<Utf8PathBuf> {
    for name in ["HOME", "USERPROFILE"] {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            return Ok(Utf8PathBuf::from(value));
        }
    }
    Err(anyhow!(
        "cannot expand `~`: neither HOME nor USERPROFILE is set"
    ))
}

/// Build the `--json` success envelope. Deterministic for a given input
/// (no timestamps / PIDs), so it satisfies REQ-010.
fn success_envelope(target: &Utf8Path, dest: &Utf8Path, module: &str, mode: AddMode) -> String {
    // `canonicalize_path` is best-effort here purely for display stability;
    // the stored manifest target is the verbatim user input.
    let canonical_dest = canonicalize_path(dest).unwrap_or_else(|_| dest.to_path_buf());
    let envelope = serde_json::json!({
        "added": target.as_str(),
        "module": module,
        "mode": mode.label(),
        "source": canonical_dest.as_str(),
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_owned())
}

/// Build a `--json` typed-error envelope mirroring `init`'s shape.
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

    fn args_with(symlink: bool, copy: bool, template: bool, module: Option<&str>) -> AddArgs {
        AddArgs {
            path: Utf8PathBuf::from("~/.zshrc"),
            module: module.map(str::to_owned),
            symlink,
            copy,
            template,
            json: false,
            yes: false,
        }
    }

    #[test]
    fn repo_source_name_strips_one_leading_dot() {
        assert_eq!(repo_source_name(".zshrc"), "zshrc");
        assert_eq!(repo_source_name("config"), "config");
        // A name that is only a dot, or `..`, is preserved (stripping would
        // leave an empty or surprising name).
        assert_eq!(repo_source_name("."), ".");
        assert_eq!(repo_source_name(".."), ".");
    }

    #[test]
    fn file_mode_maps_each_variant() {
        assert_eq!(file_mode(AddMode::Symlink), FileMode::Symlink);
        assert_eq!(file_mode(AddMode::Copy), FileMode::Copy);
        assert_eq!(file_mode(AddMode::Template), FileMode::TemplateRender);
    }

    #[test]
    fn resolve_mode_reads_the_set_flag() {
        let args = args_with(true, false, false, None);
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let mode = resolve_mode(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve")
            .expect("a mode flag was set");
        assert_eq!(mode, AddMode::Symlink);
    }

    #[test]
    fn resolve_mode_non_tty_without_flag_refuses() {
        let args = args_with(false, false, false, Some("zsh"));
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let mode = resolve_mode(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve must not error on the refusal path");
        assert!(
            mode.is_none(),
            "a non-TTY shell without a mode flag refuses"
        );
        assert!(
            reporter.err.contains("--symlink"),
            "the refusal must name the mode flags, got: {}",
            reporter.err
        );
    }

    #[test]
    fn resolve_mode_prompts_in_a_tty() {
        let args = args_with(false, false, false, Some("zsh"));
        let mut reader = ScriptedReader::new(&["copy\n"]);
        let mut reporter = BufferReporter::new();
        let mode = resolve_mode(&args, Tty::Interactive, &mut reader, &mut reporter)
            .expect("resolve")
            .expect("the prompt yielded a mode");
        assert_eq!(mode, AddMode::Copy);
    }

    #[test]
    fn resolve_module_non_tty_without_flag_refuses() {
        let args = args_with(true, false, false, None);
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let module = resolve_module(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve must not error on the refusal path");
        assert!(module.is_none(), "a non-TTY shell without --module refuses");
        assert!(
            reporter.err.contains("--module"),
            "the refusal must name the missing --module flag, got: {}",
            reporter.err
        );
    }

    #[test]
    fn resolve_module_uses_the_flag_value() {
        let args = args_with(true, false, false, Some("zsh"));
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let module = resolve_module(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve")
            .expect("the flag value");
        assert_eq!(module, "zsh");
    }

    #[test]
    fn success_envelope_is_deterministic_and_names_fields() {
        let target = Utf8Path::new("~/.zshrc");
        let dest = Utf8Path::new("/repo/zsh/zshrc");
        let first = success_envelope(target, dest, "zsh", AddMode::Symlink);
        let second = success_envelope(target, dest, "zsh", AddMode::Symlink);
        assert_eq!(first, second, "same inputs yield byte-identical JSON");
        let doc: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert_eq!(
            doc.get("added").and_then(serde_json::Value::as_str),
            Some("~/.zshrc")
        );
        assert_eq!(
            doc.get("module").and_then(serde_json::Value::as_str),
            Some("zsh")
        );
        assert_eq!(
            doc.get("mode").and_then(serde_json::Value::as_str),
            Some("symlink")
        );
    }
}
