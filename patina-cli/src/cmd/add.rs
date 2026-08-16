//! `patina add <path>` command logic.
//!
//! `patina add <path>` brings an existing dotfile under management. It
//! resolves the repository root, then stages the target's bytes into a module
//! subdirectory (`<repo>/<module>/<source>`). It appends the table-array entry
//! matching the source kind to that module's `patina.toml`, and creates the
//! manifest when none exists. A file source gets a `[[file]]` entry, a
//! directory source a `[[directory]]` entry. `add` copies the bytes into the
//! repository and leaves the target where it is; a later `patina apply`
//! materializes it as a symlink, a copy, or a rendered template.
//!
//! `add` is a mutating command: it acquires the engine's
//! exclusive advisory lock at `<state>/lock` before any filesystem
//! mutation, so two concurrent `add` invocations against the same state
//! directory serialize.
//!
//! ## Mode and module resolution
//!
//! The module name comes from `--module`, else an interactive prompt. In a
//! non-TTY shell without `--module`, the command warns, names the missing
//! flag, and exits 1. The mode comes from exactly one of the `--symlink` /
//! `--copy` / `--template` / `--symlink-tree` flags (clap enforces
//! at-most-one, exit 2 when more than one is given), else an interactive
//! prompt. In a non-TTY shell without a mode flag, the command exits 1 the
//! same way.
//!
//! The mode flags are kind-checked against the source's on-disk kind:
//! `--symlink` and `--copy` are valid for either kind, while
//! `--template` is file-only and `--symlink-tree` is directory-only. An
//! incompatible flag/kind pair is rejected with a typed error naming the
//! offending flag and the source kind, before any mutation. A directory
//! source therefore never emits a `[[file]]` entry and vice versa.
//!
//! Manifest editing, repo discovery, tilde expansion, and canonicalization
//! all live in `patina_core`; this module is presentation and control flow.

use crate::cli::AddArgs;
use crate::cmd::MANIFEST_FILENAME;
use crate::cmd::apply::PromptReader;
use crate::cmd::apply::Tty;
use crate::exit_code::ExitCode;
use crate::output::reporter::Reporter;
use crate::output::style::paint;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::EngineError;
use patina_core::FileMode;
use patina_core::LockKind;
use patina_core::acquire_lock;
use patina_core::append_directory_entry;
use patina_core::append_file_entry;
use patina_core::canonicalize_path;
use patina_core::discover_modules;
use patina_core::exclusive_timeout;
use patina_core::expand_tilde;
use patina_core::ignore_rules;
use patina_core::parse_module_config;
use patina_core::parse_root_config;
use patina_core::resolve_repository_root;
use patina_core::resolve_state_dir;
use patina_core::walk_files;

/// The mode flag the user selected, or answered at the prompt, before it is
/// checked against the source's on-disk kind.
///
/// [`AddMode::resolve`] turns one of these into a kind-checked [`AddMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeFlag {
    /// `--symlink`.
    Symlink,
    /// `--copy`.
    Copy,
    /// `--template` (file source only).
    Template,
    /// `--symlink-tree` (directory source only).
    SymlinkTree,
}

impl ModeFlag {
    /// The flag spelling shown in the kind-mismatch error message.
    fn flag(self) -> &'static str {
        match self {
            ModeFlag::Symlink => "--symlink",
            ModeFlag::Copy => "--copy",
            ModeFlag::Template => "--template",
            ModeFlag::SymlinkTree => "--symlink-tree",
        }
    }
}

/// Whether the source on disk is a regular file or a directory.
/// Detected from the tilde-expanded target's filesystem metadata before
/// staging, so the table-array written always matches the source kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    /// A regular file source → a `[[file]]` entry.
    File,
    /// A directory source → a `[[directory]]` entry.
    Directory,
}

impl SourceKind {
    /// The lowercase word naming the kind in the kind-mismatch error.
    fn label(self) -> &'static str {
        match self {
            SourceKind::File => "file",
            SourceKind::Directory => "directory",
        }
    }
}

/// A mode flag that has been validated against the source's on-disk kind.
///
/// Each variant fixes the [`FileMode`] the manifest writer records, and which
/// table-array (`[[file]]` vs `[[directory]]`) the entry is written to.
/// [`AddMode::resolve`] is the only constructor, and it rejects an
/// incompatible flag/kind pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddMode {
    /// A `[[file]]` `symlink` entry.
    FileSymlink,
    /// A `[[file]]` `copy` entry.
    FileCopy,
    /// A `[[file]]` implicit-template `.tmpl` source (no `mode` key; the
    /// engine derives [`FileMode::TemplateRender`] from the `.tmpl` suffix).
    FileTemplate,
    /// A `[[directory]]` `symlink` entry (an atomic whole-directory link).
    DirectorySymlink,
    /// A `[[directory]]` `symlink-tree` entry (one link per leaf file).
    DirectorySymlinkTree,
    /// A `[[directory]]` `copy` entry (a recursive directory copy).
    DirectoryCopy,
}

impl AddMode {
    /// Validate a user-selected [`ModeFlag`] against the detected
    /// [`SourceKind`]. Returns the kind-checked mode, or a typed error naming
    /// the incompatible flag and the source kind.
    fn resolve(flag: ModeFlag, kind: SourceKind) -> Result<Self> {
        match (kind, flag) {
            (SourceKind::File, ModeFlag::Symlink) => Ok(AddMode::FileSymlink),
            (SourceKind::File, ModeFlag::Copy) => Ok(AddMode::FileCopy),
            (SourceKind::File, ModeFlag::Template) => Ok(AddMode::FileTemplate),
            (SourceKind::Directory, ModeFlag::Symlink) => Ok(AddMode::DirectorySymlink),
            (SourceKind::Directory, ModeFlag::SymlinkTree) => Ok(AddMode::DirectorySymlinkTree),
            (SourceKind::Directory, ModeFlag::Copy) => Ok(AddMode::DirectoryCopy),
            (SourceKind::File, ModeFlag::SymlinkTree)
            | (SourceKind::Directory, ModeFlag::Template) => Err(anyhow!(
                "{} is not valid for a {} source",
                flag.flag(),
                kind.label()
            )),
        }
    }

    /// Whether this mode writes a `[[directory]]` (vs a `[[file]]`) entry.
    fn is_directory(self) -> bool {
        matches!(
            self,
            AddMode::DirectorySymlink | AddMode::DirectorySymlinkTree | AddMode::DirectoryCopy
        )
    }

    /// The [`FileMode`] the manifest writer records for this mode.
    fn file_mode(self) -> FileMode {
        match self {
            AddMode::FileSymlink => FileMode::Symlink,
            AddMode::FileCopy => FileMode::Copy,
            AddMode::FileTemplate => FileMode::TemplateRender,
            AddMode::DirectorySymlink => FileMode::SymlinkDir,
            AddMode::DirectorySymlinkTree => FileMode::SymlinkTree,
            AddMode::DirectoryCopy => FileMode::CopyTree,
        }
    }

    /// Whether the staged source uses the implicit `.tmpl` suffix.
    fn is_template(self) -> bool {
        matches!(self, AddMode::FileTemplate)
    }

    /// The lowercase label shown in the success line and the JSON envelope.
    fn label(self) -> &'static str {
        match self {
            AddMode::FileSymlink | AddMode::DirectorySymlink => "symlink",
            AddMode::FileCopy | AddMode::DirectoryCopy => "copy",
            AddMode::FileTemplate => "template",
            AddMode::DirectorySymlinkTree => "symlink-tree",
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
/// not exist or cannot be copied; or the manifest read/write fails.
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
    // Mode and module resolve before the lock and before any mutation, so a
    // missing-input refusal costs nothing.
    let Some(mode_flag) = resolve_mode_flag(args, tty, reader, reporter)? else {
        return Ok(ExitCode::Generic.code());
    };
    let Some(module) = resolve_module(args, tty, reader, reporter)? else {
        return Ok(ExitCode::Generic.code());
    };

    let repo_root = resolve_repository_root().map_err(EngineError::from)?;
    let home = resolve_home()?;
    let target = expand_tilde(&args.path, &home);

    // Kind-check before the lock, so an incompatible flag/kind pair refuses
    // without ever taking it.
    let kind = detect_source_kind(&target)?;
    let mode = AddMode::resolve(mode_flag, kind)?;

    let state = resolve_state_dir().map_err(EngineError::from)?;
    let lock_path = state.join("lock");
    let _guard = acquire_lock(&lock_path, LockKind::Exclusive, exclusive_timeout())
        .map_err(EngineError::from)
        .context("failed to acquire the exclusive lock")?;

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
    let basename = repo_source_name(file_name);
    // A `--template` source records the `.tmpl` suffix so the engine derives
    // the implicit template mode; the copied file on disk ends in it too.
    let source = if mode.is_template() {
        format!("{basename}.tmpl")
    } else {
        basename.clone()
    };

    let module_dir = repo_root.join(&module);
    fs_err::create_dir_all(module_dir.as_std_path())
        .with_context(|| format!("failed to create module directory {module_dir}"))?;
    let dest = module_dir.join(&source);

    // The ignore checks run before `stage_into_repo`, so a refusal leaves the
    // user's file where it was.
    if let Some(exit) = refuse_ignored_conflict(args, &repo_root, &dest, reporter)? {
        return Ok(exit);
    }
    // Tree modes only. A whole-directory `symlink` deploys through one link,
    // so a pattern cannot drop a leaf and the warning would be false.
    if matches!(mode, AddMode::DirectorySymlinkTree | AddMode::DirectoryCopy) {
        warn_on_ignored_leaves(&repo_root, &target, reporter)?;
    }

    stage_into_repo(&target, &dest, kind)?;

    // Store the unexpanded target path (e.g. `~/.zshrc`) to keep the manifest
    // portable across machines.
    let manifest_path = module_dir.join(MANIFEST_FILENAME);
    let existing_text = read_manifest_text(&manifest_path)?;
    let new_text = if mode.is_directory() {
        append_directory_entry(
            &existing_text,
            &source,
            args.path.as_str(),
            mode.file_mode(),
        )
    } else {
        append_file_entry(
            &existing_text,
            &source,
            args.path.as_str(),
            mode.file_mode(),
        )
    }
    .map_err(EngineError::from)?;
    fs_err::write(manifest_path.as_std_path(), new_text)
        .with_context(|| format!("failed to write {manifest_path}"))?;

    if args.json {
        reporter.json(&success_envelope(&args.path, &dest, &module, mode));
    } else {
        let path = paint(reporter.styles().path, args.path.as_str());
        reporter.line(&format!(
            "Added {path} to module `{module}` as {} (run `patina apply` to materialize).",
            mode.label()
        ));
    }
    Ok(ExitCode::Success.code())
}

/// Refuse the add when a tree-mode entry already excludes `dest`. Returns the
/// exit code to propagate; `None` means the add may proceed.
///
/// `--force` skips the check.
///
/// # Errors
///
/// Returns an error when the manifests cannot be read or a pattern does not
/// compile.
fn refuse_ignored_conflict(
    args: &AddArgs,
    repo_root: &Utf8Path,
    dest: &Utf8Path,
    reporter: &mut impl Reporter,
) -> Result<Option<i32>> {
    if args.force {
        return Ok(None);
    }
    let Some((module, entry_source)) = ignoring_tree_entry(repo_root, dest)? else {
        return Ok(None);
    };
    let message = format!(
        "{} would be deployed by a new entry, but module `{module}` already excludes it from its \
         `{entry_source}` tree via an `ignore` pattern; pass --force to declare it anyway, or drop \
         the pattern",
        args.path
    );
    if args.json {
        reporter.json(&error_envelope(
            "ignored_conflict",
            args.path.as_str(),
            &message,
        ));
    } else {
        reporter.warn(&message);
    }
    Ok(Some(ExitCode::Generic.code()))
}

/// Find a tree-mode entry that would exclude `dest`, the repository path the
/// added file is about to occupy. Returns the declaring module's name and that
/// entry's source.
///
/// Declaring a `[[file]]` for a path a `symlink-tree` already excludes is a
/// contradiction in one manifest: the new entry deploys exactly what the tree
/// entry was told to skip. Only an entry whose source directory contains `dest`
/// can contradict it that way. A `[[file]]` deploys its declared source with no
/// enumeration, so a repo-wide pattern matching it elsewhere is not a conflict.
///
/// # Errors
///
/// Returns an error when the root manifest, module enumeration, or a module
/// manifest cannot be read.
fn ignoring_tree_entry(
    repo_root: &Utf8Path,
    dest: &Utf8Path,
) -> Result<Option<(String, Utf8PathBuf)>> {
    let root_config =
        parse_root_config(&repo_root.join(MANIFEST_FILENAME)).map_err(EngineError::from)?;
    for module in discover_modules(repo_root).map_err(EngineError::from)? {
        let manifest = module.path.join(MANIFEST_FILENAME);
        let config = parse_module_config(&manifest).map_err(EngineError::from)?;
        for entry in &config.directories {
            if !matches!(entry.mode, FileMode::SymlinkTree | FileMode::CopyTree) {
                continue;
            }
            let entry_source = module.path.join(&entry.source);
            let Ok(relative) = dest.strip_prefix(&entry_source) else {
                continue;
            };
            let rules = ignore_rules::build(&root_config.ignore, &entry.ignore, &entry_source)
                .map_err(EngineError::from)?;
            if ignore_rules::prunes(&rules, relative) {
                return Ok(Some((module.name.clone(), entry.source.clone())));
            }
        }
    }
    Ok(None)
}

/// Warn when the directory being added contains leaves the repo-wide ignore
/// list would drop. The warning names how many.
///
/// Informational only. The new entry has no `ignore` of its own yet, so only
/// the root manifest's patterns can apply.
///
/// # Errors
///
/// Returns an error when the root manifest cannot be read or its patterns do
/// not compile.
fn warn_on_ignored_leaves(
    repo_root: &Utf8Path,
    source: &Utf8Path,
    reporter: &mut impl Reporter,
) -> Result<()> {
    let root_config =
        parse_root_config(&repo_root.join(MANIFEST_FILENAME)).map_err(EngineError::from)?;
    if root_config.ignore.is_empty() {
        return Ok(());
    }
    let rules = ignore_rules::build(&root_config.ignore, &[], source).map_err(EngineError::from)?;
    let dropped = walk_files(source, &ignore_rules::none())
        .map_err(EngineError::from)?
        .iter()
        .filter(|rel| ignore_rules::prunes(&rules, rel))
        .count();
    if dropped > 0 {
        let noun = if dropped == 1 { "path" } else { "paths" };
        reporter.warn(&format!(
            "the repo-wide `ignore` list excludes {dropped} {noun} under {source}; they will not \
             be deployed"
        ));
    }
    Ok(())
}

/// Scan every module manifest for a `[[file]]` or `[[directory]]` entry
/// whose target resolves to the same absolute path as `target`. Returns the
/// owning module's name on a match.
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
        for entry in config.files.iter().chain(config.directories.iter()) {
            for entry_target in &entry.targets {
                if expand_tilde(entry_target, home) == target {
                    return Ok(Some(module.name));
                }
            }
        }
    }
    Ok(None)
}

/// Stage the dotfile at `from` into the repository at `to`.
///
/// A directory source is copied recursively; a file source is a single byte
/// copy. The source at `from` remains an untouched regular file until a later
/// `patina apply` replaces it.
fn stage_into_repo(from: &Utf8Path, to: &Utf8Path, kind: SourceKind) -> Result<()> {
    match kind {
        SourceKind::File => {
            fs_err::copy(from.as_std_path(), to.as_std_path())
                .with_context(|| format!("failed to copy {from} into the repository at {to}"))?;
        }
        SourceKind::Directory => {
            copy_dir_recursive(from, to).with_context(|| {
                format!("failed to copy directory {from} into the repository at {to}")
            })?;
        }
    }
    Ok(())
}

/// Recursively copy the directory at `from` into a freshly-created `to`.
/// Subdirectories and regular files are mirrored. A symbolic link inside the
/// tree is followed and copied as its target bytes: `add` stages source
/// content, not link structure.
fn copy_dir_recursive(from: &Utf8Path, to: &Utf8Path) -> Result<()> {
    fs_err::create_dir_all(to.as_std_path()).with_context(|| format!("failed to create {to}"))?;
    for entry in fs_err::read_dir(from.as_std_path())? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 file name under {from}"))?;
        let child_from = from.join(name);
        let child_to = to.join(name);
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&child_from, &child_to)?;
        } else {
            fs_err::copy(child_from.as_std_path(), child_to.as_std_path())?;
        }
    }
    Ok(())
}

/// Detect whether `target` on disk is a regular file or a directory.
///
/// `fs_err::metadata` follows a symbolic link, so the staged kind matches what
/// the user pointed at. A missing or unreadable target is an error.
fn detect_source_kind(target: &Utf8Path) -> Result<SourceKind> {
    let metadata = fs_err::metadata(target.as_std_path()).with_context(|| {
        format!("cannot add `{target}`: the source does not exist or is unreadable")
    })?;
    if metadata.is_dir() {
        Ok(SourceKind::Directory)
    } else {
        Ok(SourceKind::File)
    }
}

/// Read the existing module manifest text. A missing file reads as the empty
/// document, so a fresh module's manifest is created on first `add`.
fn read_manifest_text(manifest_path: &Utf8Path) -> Result<String> {
    if manifest_path.exists() {
        fs_err::read_to_string(manifest_path.as_std_path())
            .with_context(|| format!("failed to read {manifest_path}"))
    } else {
        Ok(String::new())
    }
}

/// Resolve the selected mode flag. With no flag set, a TTY prompts for one.
/// Returns `Ok(None)` for the non-TTY-without-mode refusal (exit 1).
/// [`AddMode::resolve`] validates the returned [`ModeFlag`] against the source
/// kind later.
fn resolve_mode_flag(
    args: &AddArgs,
    tty: Tty,
    reader: &mut impl PromptReader,
    reporter: &mut impl Reporter,
) -> Result<Option<ModeFlag>> {
    // clap's mode group guarantees at most one flag is set.
    if args.symlink {
        return Ok(Some(ModeFlag::Symlink));
    }
    if args.copy {
        return Ok(Some(ModeFlag::Copy));
    }
    if args.template {
        return Ok(Some(ModeFlag::Template));
    }
    if args.symlink_tree {
        return Ok(Some(ModeFlag::SymlinkTree));
    }
    match tty {
        Tty::NonInteractive => {
            reporter
                .warn("a mode flag is required in a non-TTY shell: pass one of --symlink, --copy, --template, or --symlink-tree");
            Ok(None)
        }
        Tty::Interactive => {
            reporter.prompt("Mode? [symlink/copy/template/symlink-tree] ");
            let answer = reader.read_line().unwrap_or_default();
            match answer.trim() {
                "symlink" => Ok(Some(ModeFlag::Symlink)),
                "copy" => Ok(Some(ModeFlag::Copy)),
                "template" => Ok(Some(ModeFlag::Template)),
                "symlink-tree" => Ok(Some(ModeFlag::SymlinkTree)),
                other => Err(anyhow!(
                    "unrecognized mode `{other}`; expected symlink, copy, template, or symlink-tree"
                )),
            }
        }
    }
}

/// Resolve the module name. With `--module` absent, a TTY prompts for one.
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

/// Derive the repository source name from a target's file name. A single
/// leading dot is stripped, so a dotfile is stored as `zsh/zshrc` rather than
/// `zsh/.zshrc`.
///
/// The empty-result guard is unreachable: `Utf8Path::file_name` returns `None`
/// for `.` and `..`, and the caller rejects a target with no file name.
fn repo_source_name(file_name: &str) -> String {
    match file_name.strip_prefix('.') {
        Some(rest) if !rest.is_empty() => rest.to_owned(),
        _ => file_name.to_owned(),
    }
}

/// Resolve the user's home directory for tilde expansion. `$HOME` is read
/// first, then `$USERPROFILE` (the Windows fallback).
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
/// (no timestamps / PIDs).
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

/// Build the `--json` typed-error envelope: `error`, `path`, `message`. `init`
/// and `remove` emit the same error keys.
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
            symlink_tree: false,
            json: false,
            yes: false,
            force: false,
        }
    }

    #[test]
    fn repo_source_name_strips_one_leading_dot() {
        assert_eq!(repo_source_name(".zshrc"), "zshrc");
        assert_eq!(repo_source_name("config"), "config");
        // At most one dot comes off, and the result is never empty.
        assert_eq!(repo_source_name("."), ".");
        assert_eq!(repo_source_name(".."), ".");
    }

    #[test]
    fn add_mode_resolves_each_compatible_flag_kind_pair() {
        // The full compatibility matrix. Each row: flag, source kind,
        // resolved mode, recorded FileMode, is_directory.
        let cases = [
            (
                ModeFlag::Symlink,
                SourceKind::File,
                AddMode::FileSymlink,
                FileMode::Symlink,
                false,
            ),
            (
                ModeFlag::Copy,
                SourceKind::File,
                AddMode::FileCopy,
                FileMode::Copy,
                false,
            ),
            (
                ModeFlag::Template,
                SourceKind::File,
                AddMode::FileTemplate,
                FileMode::TemplateRender,
                false,
            ),
            (
                ModeFlag::Symlink,
                SourceKind::Directory,
                AddMode::DirectorySymlink,
                FileMode::SymlinkDir,
                true,
            ),
            (
                ModeFlag::SymlinkTree,
                SourceKind::Directory,
                AddMode::DirectorySymlinkTree,
                FileMode::SymlinkTree,
                true,
            ),
            (
                ModeFlag::Copy,
                SourceKind::Directory,
                AddMode::DirectoryCopy,
                FileMode::CopyTree,
                true,
            ),
        ];
        for (flag, kind, expected, file_mode, is_dir) in cases {
            let resolved =
                AddMode::resolve(flag, kind).expect("a compatible flag/kind pair must resolve");
            assert_eq!(resolved, expected, "{flag:?}/{kind:?}");
            assert_eq!(
                resolved.file_mode(),
                file_mode,
                "{flag:?}/{kind:?} file_mode"
            );
            assert_eq!(
                resolved.is_directory(),
                is_dir,
                "{flag:?}/{kind:?} is_directory"
            );
        }
    }

    #[test]
    fn add_mode_rejects_symlink_tree_on_a_file_naming_flag_and_kind() {
        let err = AddMode::resolve(ModeFlag::SymlinkTree, SourceKind::File)
            .expect_err("--symlink-tree on a file must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("--symlink-tree") && message.contains("file"),
            "error must name the flag and the file kind, got: {message}"
        );
    }

    #[test]
    fn add_mode_rejects_template_on_a_directory_naming_flag_and_kind() {
        let err = AddMode::resolve(ModeFlag::Template, SourceKind::Directory)
            .expect_err("--template on a directory must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("--template") && message.contains("directory"),
            "error must name the flag and the directory kind, got: {message}"
        );
    }

    #[test]
    fn resolve_mode_flag_reads_the_set_flag() {
        let args = args_with(true, false, false, None);
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let flag = resolve_mode_flag(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve")
            .expect("a mode flag was set");
        assert_eq!(flag, ModeFlag::Symlink);
    }

    #[test]
    fn resolve_mode_flag_reads_symlink_tree() {
        let mut args = args_with(false, false, false, Some("d"));
        args.symlink_tree = true;
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let flag = resolve_mode_flag(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve")
            .expect("the --symlink-tree flag was set");
        assert_eq!(flag, ModeFlag::SymlinkTree);
    }

    #[test]
    fn resolve_mode_flag_non_tty_without_flag_refuses() {
        let args = args_with(false, false, false, Some("zsh"));
        let mut reader = ScriptedReader::new(&[]);
        let mut reporter = BufferReporter::new();
        let flag = resolve_mode_flag(&args, Tty::NonInteractive, &mut reader, &mut reporter)
            .expect("resolve must not error on the refusal path");
        assert!(
            flag.is_none(),
            "a non-TTY shell without a mode flag refuses"
        );
        assert!(
            reporter.err.contains("--symlink") && reporter.err.contains("--symlink-tree"),
            "the refusal must name the mode flags, got: {}",
            reporter.err
        );
    }

    #[test]
    fn resolve_mode_flag_prompts_in_a_tty() {
        let args = args_with(false, false, false, Some("zsh"));
        let mut reader = ScriptedReader::new(&["copy\n"]);
        let mut reporter = BufferReporter::new();
        let flag = resolve_mode_flag(&args, Tty::Interactive, &mut reader, &mut reporter)
            .expect("resolve")
            .expect("the prompt yielded a mode");
        assert_eq!(flag, ModeFlag::Copy);
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
        let first = success_envelope(target, dest, "zsh", AddMode::FileSymlink);
        let second = success_envelope(target, dest, "zsh", AddMode::FileSymlink);
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
