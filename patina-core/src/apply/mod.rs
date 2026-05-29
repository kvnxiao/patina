//! File-mode executors with multi-target fan-out (REQ-005).
//!
//! Each [`FileMode`] has an executor that
//! materializes a single source path at one or more target paths. The
//! five modes split across three submodules:
//!
//! - `symlink` — per-file [`Symlink`](crate::config::FileMode::Symlink) (with
//!   the directory-source per-file walk) and atomic
//!   [`SymlinkDir`](crate::config::FileMode::SymlinkDir).
//! - `copy` — [`Copy`](crate::config::FileMode::Copy) and recursive
//!   [`CopyTree`](crate::config::FileMode::CopyTree).
//! - `template` — implicit
//!   [`TemplateRender`](crate::config::FileMode::TemplateRender) of a `.tmpl`
//!   source, rendered once and written to each declared (suffix-less) target.
//!
//! # Per-target completion records
//!
//! Every executor returns a `Vec<`[`CompletionRecord`]`>`: one record per
//! materialized filesystem object. A single-file source produces one
//! record per target; a directory-source symlink walk produces one record
//! per walked file per target. Keeping this per-target (and per-walked-file)
//! granularity throughout lets T-010's progress cursor record one entry per
//! materialized object so backups (T-012), status (T-017), and rollback
//! (T-018) inherit the same unit without special-casing the multi-target
//! shape.
//!
//! # Scope
//!
//! These functions are the execution side only. They take an
//! already-canonicalized absolute source and already-resolved absolute
//! target paths (tilde expansion and canonicalization live in
//! [`crate::paths`]). Plan construction, journal persistence, the
//! diff-and-prompt loop, and the apply orchestration that calls these
//! executors land in their own tasks.

pub mod engine;
pub mod hooks;

mod copy;
mod symlink;
mod template;

use crate::config::FileMode;
use crate::template::Engine;
use crate::template::TemplateError;
use crate::variables::Resolver;
use camino::Utf8Path;
use camino::Utf8PathBuf;
pub use hooks::ForceDeploy;
pub use hooks::HookError;
pub use hooks::HookOutcome;
pub use hooks::ResolvedHook;
pub use hooks::resolve_on_path;
pub use hooks::resolve_shells;
pub use hooks::run_hook;
pub use hooks::should_run;

/// What an executor materialized at a target, for the
/// [`CompletionRecord`] one-per-object handoff to T-010.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Materialization {
    /// A symbolic link was created at the target. `link_target` is the
    /// canonical absolute path the link points at (the source file or
    /// source directory).
    Symlink {
        /// The canonical absolute path the created link points at.
        link_target: Utf8PathBuf,
    },
    /// A regular file was written at the target as a byte copy of the
    /// source.
    Copy,
    /// A regular file was written at the target from rendered template
    /// output.
    Render,
}

/// One materialized filesystem object, returned per `(source, target)`
/// (and, for the directory-symlink walk, per walked file).
///
/// `source` and `target` are canonical absolute paths so the record can be
/// journaled, probed during recovery, and surfaced in status/rollback
/// without re-resolving anything.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompletionRecord {
    /// Canonical absolute source path materialized.
    pub source: Utf8PathBuf,
    /// Canonical absolute target path the object was materialized at.
    pub target: Utf8PathBuf,
    /// What was materialized at `target`.
    pub materialization: Materialization,
}

impl CompletionRecord {
    /// Construct a record for a created symbolic link.
    fn symlink(source: Utf8PathBuf, target: Utf8PathBuf, link_target: Utf8PathBuf) -> Self {
        Self {
            source,
            target,
            materialization: Materialization::Symlink { link_target },
        }
    }

    /// Construct a record for a byte copy.
    fn copy(source: Utf8PathBuf, target: Utf8PathBuf) -> Self {
        Self {
            source,
            target,
            materialization: Materialization::Copy,
        }
    }

    /// Construct a record for a rendered template write.
    fn render(source: Utf8PathBuf, target: Utf8PathBuf) -> Self {
        Self {
            source,
            target,
            materialization: Materialization::Render,
        }
    }
}

/// Failures a file-mode executor can surface (REQ-005).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExecutorError {
    /// A symbolic-link creation, copy, directory walk, or write hit an
    /// underlying IO error. The path that failed is named so the message
    /// is actionable.
    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        /// The path whose operation failed.
        path: Utf8PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// A symlink-family mode was asked to point at a source that does not
    /// exist (the engine canonicalizes before reaching here, so this is a
    /// genuine missing source rather than a relative-path slip).
    #[error("symlink source {path} does not exist")]
    SourceMissing {
        /// The missing source path.
        path: Utf8PathBuf,
    },

    /// A [`SymlinkDir`](crate::config::FileMode::SymlinkDir) or
    /// [`CopyTree`](crate::config::FileMode::CopyTree) entry named a source
    /// that is not a directory.
    #[error("mode requires a directory source but {path} is not a directory")]
    NotADirectory {
        /// The source path that was expected to be a directory.
        path: Utf8PathBuf,
    },

    /// The render executor was handed a source that does not carry the
    /// `.tmpl` suffix. Templating is keyed off the source suffix, so the
    /// engine should never classify a non-`.tmpl` source as a render; this
    /// guards that invariant rather than rendering an arbitrary file.
    #[error("template source {path} does not carry a `.tmpl` suffix")]
    NotATemplate {
        /// The source path that lacked the `.tmpl` suffix.
        path: Utf8PathBuf,
    },

    /// Creating a symbolic link on Windows failed because the process
    /// lacks the privilege (Developer Mode off and not elevated). The
    /// prompt/elevate flow is deferred to SPEC-0002; this SPEC surfaces
    /// the typed error so the CLI can exit non-zero with the message.
    #[error(
        "creating a symbolic link at {target} requires Windows Developer Mode or an elevated process: {source}"
    )]
    WindowsSymlinkPermission {
        /// The target path the link creation was attempted at.
        target: Utf8PathBuf,
        /// The underlying permission error.
        #[source]
        source: std::io::Error,
    },

    /// Rendering a `.tmpl` source through `MiniJinja` failed (an undefined
    /// variable under strict-undefined, or a syntax/evaluation error).
    #[error(transparent)]
    Template(#[from] TemplateError),
}

/// Materialize a single resolved `[[file]]` entry at every target path.
///
/// `source` is the canonical absolute source path; `targets` are the
/// canonical absolute target paths the entry fans out to. For
/// [`TemplateRender`](FileMode::TemplateRender) the template is rendered
/// **once** against `resolver` and the same bytes are written to each
/// target; `resolver`/`engine` are unused by the non-template modes.
///
/// Returns one [`CompletionRecord`] per materialized filesystem object,
/// in target order (and, for the directory-source symlink walk, in
/// walk order within each target).
///
/// # Errors
///
/// Returns an [`ExecutorError`] for the first target that fails. Already
/// materialized targets are left in place; the multi-target atomic
/// revert contract (REQ-005 / REQ-013) is the orchestrator's
/// responsibility via the journaled per-object records, not this
/// executor's.
pub fn materialize(
    mode: FileMode,
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
    engine: &Engine,
    resolver: &Resolver,
) -> Result<Vec<CompletionRecord>, ExecutorError> {
    match mode {
        FileMode::Symlink => symlink::per_file_symlink(source, targets),
        FileMode::SymlinkDir => symlink::dir_symlink(source, targets),
        FileMode::Copy => copy::copy_file(source, targets),
        FileMode::CopyTree => copy::copy_tree(source, targets),
        FileMode::TemplateRender => template::render(source, targets, engine, resolver),
    }
}

/// Create the parent directory chain for `target` if it is absent.
///
/// Shared by every executor: a target whose parent directories have not
/// been created yet (the common case for a fresh `$HOME`) needs the chain
/// in place before the link/copy/write.
fn ensure_parent(target: &Utf8Path) -> Result<(), ExecutorError> {
    if let Some(parent) = target.parent()
        && !parent.as_str().is_empty()
        && !parent.exists()
    {
        fs_err::create_dir_all(parent).map_err(|source| ExecutorError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// Walk `root` and collect every regular file as a path relative to
/// `root`, in deterministic (sorted) order so the same source tree
/// produces the same record sequence across runs and platforms.
///
/// Shared by the directory-source symlink walk ([`symlink`]) and the
/// recursive copy ([`copy`]): both mirror a source tree to each target
/// one file at a time, so both need the same relative-file enumeration.
fn walk_files(root: &Utf8Path) -> Result<Vec<Utf8PathBuf>, ExecutorError> {
    let mut out = Vec::new();
    walk_into(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

/// Recursive helper for [`walk_files`]: descend `dir`, pushing each
/// regular file's path relative to `base` into `out`.
fn walk_into(
    base: &Utf8Path,
    dir: &Utf8Path,
    out: &mut Vec<Utf8PathBuf>,
) -> Result<(), ExecutorError> {
    let entries = fs_err::read_dir(dir.as_std_path()).map_err(|source| ExecutorError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ExecutorError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|bad| ExecutorError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("non-UTF-8 path under source tree: {}", bad.display()),
            ),
        })?;
        let file_type = entry.file_type().map_err(|source| ExecutorError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            walk_into(base, &path, out)?;
        } else {
            // `path` was built by descending from `base`, so it always
            // carries `base` as a prefix; fall back to the full path only
            // to keep the helper total.
            let rel = match path.strip_prefix(base) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => path.clone(),
            };
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variables::Builtins;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("create tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
        (td, canonical)
    }

    fn resolver() -> Resolver {
        Resolver::new(Builtins::for_tests())
    }

    #[test]
    fn materialize_dispatches_copy_mode() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("source.txt");
        fs_err::write(&source, b"payload").expect("write source");
        let target = dir.join("out").join("dest.txt");

        let records = materialize(
            FileMode::Copy,
            &source,
            std::slice::from_ref(&target),
            &Engine::new(),
            &resolver(),
        )
        .expect("copy materializes");

        assert_eq!(records.len(), 1);
        let record = records.first().expect("one completion record");
        assert_eq!(record.materialization, Materialization::Copy);
        assert_eq!(record.target, target);
        assert_eq!(fs_err::read(&target).expect("read copied file"), b"payload");
    }

    #[test]
    fn ensure_parent_creates_missing_chain() {
        let (_td, dir) = utf8_tempdir();
        let target = dir.join("a").join("b").join("c.txt");
        ensure_parent(&target).expect("parent chain created");
        assert!(dir.join("a").join("b").exists());
    }

    #[test]
    fn walk_files_is_sorted_and_relative() {
        let (_td, dir) = utf8_tempdir();
        let root = dir.join("tree");
        fs_err::create_dir_all(root.join("z")).expect("mkdir z");
        fs_err::write(root.join("z").join("zz.txt"), b"1").expect("write zz");
        fs_err::write(root.join("a.txt"), b"2").expect("write a");
        let files = walk_files(&root).expect("walk");
        // Compare on component sequences so the assertion is independent
        // of the platform path separator.
        let as_components: Vec<Vec<&str>> = files
            .iter()
            .map(|p| p.components().map(|c| c.as_str()).collect())
            .collect();
        assert_eq!(as_components, vec![vec!["a.txt"], vec!["z", "zz.txt"]]);
    }
}
