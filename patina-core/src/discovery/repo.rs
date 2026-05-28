//! Repository root resolution (REQ-003).
//!
//! Resolves the dotfiles repository root through three sources in
//! priority order: the `PATINA_REPO` environment variable, an upward
//! walk from the current working directory, and a persisted default
//! path stored under the per-machine state directory.
//!
//! When all three sources fail, [`RepoDiscoveryError::AllSourcesFailed`]
//! is returned, naming each source attempt so T-020 can map it to
//! exit code 1 with a stderr message containing the substrings
//! `PATINA_REPO`, `walk-up`, and `persisted default`.

use super::MANIFEST_FILENAME;
use super::ManifestHeadError;
use super::read_manifest_head;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::env;

/// Name of the environment variable consulted as the highest-priority
/// repository-root source.
pub const ENV_VAR: &str = "PATINA_REPO";

/// Filename of the persisted-default file under
/// `<state>/patina/default_repo`.
pub const PERSISTED_DEFAULT_FILENAME: &str = "default_repo";

/// Errors returned from [`resolve_repository_root`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepoDiscoveryError {
    /// All three resolution sources failed. The Display impl names each
    /// source so the CLI's stderr renderer can satisfy CHK-007 without
    /// reformatting.
    #[error(
        "could not resolve a Patina repository root.\n\
         tried PATINA_REPO env var: {env_attempt}\n\
         tried walk-up from {walk_up_from}: no patina.toml with root = true found between this directory and the filesystem root\n\
         tried persisted default at {persisted_default_attempt}"
    )]
    AllSourcesFailed {
        /// Description of what was tried for the `PATINA_REPO` source.
        env_attempt: String,
        /// Directory the walk-up search started from.
        walk_up_from: Utf8PathBuf,
        /// Description of what was tried for the persisted-default source.
        persisted_default_attempt: String,
    },

    /// The `PATINA_REPO` value pointed at a path that did not exist, was
    /// not a directory, or whose manifest could not be loaded.
    #[error("PATINA_REPO points at {path} but no valid root patina.toml was found there: {reason}")]
    EnvVarInvalid {
        /// The path read from `PATINA_REPO`.
        path: Utf8PathBuf,
        /// Human-readable reason the path was rejected.
        reason: String,
    },

    /// The current working directory was not valid UTF-8 (rare; only on
    /// non-UTF-8 filesystems).
    #[error("current working directory is not valid UTF-8: {0}")]
    CwdNotUtf8(std::path::PathBuf),

    /// The current working directory could not be read at all.
    #[error("failed to read current working directory: {0}")]
    CwdUnavailable(#[source] std::io::Error),

    /// Canonicalizing the resolved path failed. T-009 will replace the
    /// direct `canonicalize_utf8` call with a lexical-fallback helper;
    /// for now bubble the IO error verbatim.
    #[error("failed to canonicalize repository root {path}: {source}")]
    Canonicalize {
        /// The path that failed to canonicalize.
        path: Utf8PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

/// Resolve the dotfiles repository root.
///
/// Tries `PATINA_REPO`, then a walk-up from the current working
/// directory, then the persisted default file under the per-machine
/// state directory. The returned path is canonicalized.
///
/// # Errors
///
/// Returns [`RepoDiscoveryError::EnvVarInvalid`] when `PATINA_REPO` is
/// set but unusable; [`RepoDiscoveryError::AllSourcesFailed`] when no
/// source succeeded; or [`RepoDiscoveryError::Canonicalize`] when the
/// resolved path cannot be canonicalized.
pub fn resolve_repository_root() -> Result<Utf8PathBuf, RepoDiscoveryError> {
    let env_value = env::var(ENV_VAR).ok().filter(|v| !v.is_empty());
    let cwd_std = env::current_dir().map_err(RepoDiscoveryError::CwdUnavailable)?;
    let cwd = Utf8PathBuf::from_path_buf(cwd_std).map_err(RepoDiscoveryError::CwdNotUtf8)?;
    let persisted = persisted_default_repo_path();
    resolve_repository_root_with(env_value.as_deref(), &cwd, persisted.as_deref())
}

/// Test seam for [`resolve_repository_root`]. Production code calls
/// the no-arg variant which captures process state; tests inject all
/// three sources explicitly to avoid the env-var race that comes with
/// parallel test execution.
///
/// # Errors
///
/// See [`RepoDiscoveryError`].
pub fn resolve_repository_root_with(
    env_value: Option<&str>,
    cwd: &Utf8Path,
    persisted_default: Option<&Utf8Path>,
) -> Result<Utf8PathBuf, RepoDiscoveryError> {
    // Source 1: PATINA_REPO.
    let env_attempt = match env_value.filter(|v| !v.is_empty()) {
        Some(raw) => {
            let path = Utf8PathBuf::from(raw);
            match validate_root(&path) {
                Ok(canonical) => return Ok(canonical),
                Err(reason) => {
                    return Err(RepoDiscoveryError::EnvVarInvalid { path, reason });
                }
            }
        }
        None => "not set".to_owned(),
    };

    // Source 2: walk-up from CWD.
    if let Some(found) = walk_up_for_root(cwd)? {
        return Ok(found);
    }

    // Source 3: persisted default.
    let persisted_attempt = match persisted_default {
        Some(p) => p.to_string(),
        None => "no per-machine state directory available".to_owned(),
    };

    if let Some(p) = persisted_default
        && let Some(repo) = read_persisted_default(p)
    {
        return Ok(repo);
    }

    Err(RepoDiscoveryError::AllSourcesFailed {
        env_attempt,
        walk_up_from: cwd.to_path_buf(),
        persisted_default_attempt: persisted_attempt,
    })
}

/// Confirm `path` is a directory containing a `patina.toml` whose
/// `[patina].root` is `true`, then canonicalize and return it.
fn validate_root(path: &Utf8Path) -> Result<Utf8PathBuf, String> {
    if !path.is_dir() {
        return Err(format!("{path} is not a directory"));
    }
    let manifest = path.join(MANIFEST_FILENAME);
    if !manifest.is_file() {
        return Err(format!("no {MANIFEST_FILENAME} found at {manifest}"));
    }
    match read_manifest_head(&manifest) {
        Ok(head) if head.patina.root == Some(true) => path
            .canonicalize_utf8()
            .map_err(|e| format!("canonicalize failed: {e}")),
        Ok(_) => Err(format!(
            "{manifest} is missing `root = true` in its `[patina]` table"
        )),
        Err(ManifestHeadError::Io { source, .. }) => Err(format!("read failed: {source}")),
        Err(ManifestHeadError::Parse { source, .. }) => Err(format!("parse failed: {source}")),
    }
}

/// Walk upward from `start` looking for a `patina.toml` with
/// `root = true`. Stops at the filesystem root.
fn walk_up_for_root(start: &Utf8Path) -> Result<Option<Utf8PathBuf>, RepoDiscoveryError> {
    let mut cursor: Option<&Utf8Path> = Some(start);
    while let Some(dir) = cursor {
        let candidate = dir.join(MANIFEST_FILENAME);
        if candidate.is_file()
            && let Ok(head) = read_manifest_head(&candidate)
            && head.patina.root == Some(true)
        {
            let canonical =
                dir.canonicalize_utf8()
                    .map_err(|source| RepoDiscoveryError::Canonicalize {
                        path: dir.to_path_buf(),
                        source,
                    })?;
            return Ok(Some(canonical));
        }
        cursor = dir.parent();
    }
    Ok(None)
}

/// Compute the path of the persisted-default file under the
/// per-machine state directory. Mirrors T-005's layout so a later
/// refactor can replace this with a `state_dir::resolve()` call.
///
/// Returns `None` when the OS-specific environment variables that
/// drive state-dir resolution are not set (e.g. tests with `HOME`
/// unset).
fn persisted_default_repo_path() -> Option<Utf8PathBuf> {
    state_dir_root().map(|root| root.join("patina").join(PERSISTED_DEFAULT_FILENAME))
}

#[cfg(target_os = "linux")]
fn state_dir_root() -> Option<Utf8PathBuf> {
    if let Ok(xdg) = env::var("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return Some(Utf8PathBuf::from(xdg));
    }
    let home = env::var("HOME").ok().filter(|v| !v.is_empty())?;
    Some(Utf8PathBuf::from(home).join(".local").join("state"))
}

#[cfg(target_os = "macos")]
fn state_dir_root() -> Option<Utf8PathBuf> {
    let home = env::var("HOME").ok().filter(|v| !v.is_empty())?;
    Some(
        Utf8PathBuf::from(home)
            .join("Library")
            .join("Application Support"),
    )
}

#[cfg(target_os = "windows")]
fn state_dir_root() -> Option<Utf8PathBuf> {
    let local = env::var("LOCALAPPDATA").ok().filter(|v| !v.is_empty())?;
    Some(Utf8PathBuf::from(local))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn state_dir_root() -> Option<Utf8PathBuf> {
    None
}

/// Read the persisted-default file and return the contained
/// repository root if it points at a valid root.
fn read_persisted_default(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let text = fs_err::read_to_string(path.as_std_path()).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = Utf8PathBuf::from(trimmed);
    validate_root(&candidate).ok()
}
