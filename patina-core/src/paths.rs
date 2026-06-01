//! Absolute-path canonicalization with lexical fallback (REQ-010).
//!
//! Every repository path, source path, target path, and
//! state-directory path the engine reads is canonicalized to absolute
//! form before it surfaces in an error message, a journal record, or
//! user-facing output. Relative paths must never appear in journal
//! records.
//!
//! [`canonicalize`] has two branches:
//!
//! 1. A path that already exists on disk is canonicalized through the
//!    filesystem, resolving symlinks and `.` / `..` components.
//! 2. A path that does not yet exist (typical for target paths whose parent
//!    directories have not been created) is canonicalized lexically: it is
//!    joined with the canonical absolute parent when the parent exists, or with
//!    the canonical current working directory otherwise.
//!
//! [`expand_tilde`] handles the separate user-input concern of
//! expanding a leading `~` to the resolved home directory (the
//! `patina.home` built-in from REQ-007). It is purely lexical and does
//! not touch the filesystem; callers pipe its output into
//! [`canonicalize`] when they want an absolute, symlink-resolved form.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::env;

/// Errors returned from [`canonicalize`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PathError {
    /// The current working directory could not be read. Only reachable
    /// on the lexical-fallback branch for a relative path with no
    /// existing parent.
    #[error("failed to read current working directory while canonicalizing {path}: {source}")]
    CwdUnavailable {
        /// The path being canonicalized when the CWD read failed.
        path: Utf8PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// The current working directory was not valid UTF-8. Rare; only on
    /// non-UTF-8 filesystems.
    #[error("current working directory {cwd} is not valid UTF-8 while canonicalizing {path}")]
    CwdNotUtf8 {
        /// The path being canonicalized when the non-UTF-8 CWD surfaced.
        path: Utf8PathBuf,
        /// The non-UTF-8 current working directory that could not be
        /// converted to a [`Utf8PathBuf`].
        cwd: std::path::PathBuf,
    },

    /// A filesystem canonicalization call failed for a path the engine
    /// believed existed (existence is re-checked, so this is a TOCTOU
    /// race or a permission error rather than a plain not-found).
    #[error("failed to canonicalize {path}: {source}")]
    Filesystem {
        /// The path that failed to canonicalize.
        path: Utf8PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// Canonicalizing the resolved path produced a non-UTF-8 result.
    /// Rare; only on non-UTF-8 filesystems.
    #[error("canonicalized form of {path} is not valid UTF-8")]
    ResultNotUtf8 {
        /// The path whose canonical form was not valid UTF-8.
        path: Utf8PathBuf,
    },
}

/// Canonicalize `p` to absolute form.
///
/// If `p` exists on disk it is canonicalized through the filesystem
/// (symlinks and `.` / `..` resolved). If `p` does not exist it is
/// canonicalized lexically by joining with the canonical parent (when
/// the parent exists) or the canonical current working directory.
///
/// This function never expands `~`; expand it with [`expand_tilde`]
/// first if the input may carry a home-relative prefix.
///
/// On Windows, any `\\?\` verbatim prefix is stripped where the plain form
/// is equivalent, so canonical paths never carry the verbatim prefix into
/// user-facing output or persisted state.
///
/// # Examples
///
/// ```
/// use camino::Utf8Path;
/// use patina_core::paths::canonicalize;
///
/// // An existing directory canonicalizes to an absolute, symlink-free
/// // path. The current directory always exists, so this branch is the
/// // filesystem one.
/// let cwd = canonicalize(Utf8Path::new("."))?;
/// assert!(cwd.is_absolute());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns [`PathError::CwdUnavailable`] / [`PathError::CwdNotUtf8`]
/// when the lexical-fallback branch needs the current working
/// directory but cannot read it as UTF-8; [`PathError::Filesystem`]
/// when an existing path fails to canonicalize; and
/// [`PathError::ResultNotUtf8`] when a canonical form is not valid
/// UTF-8.
pub fn canonicalize(p: &Utf8Path) -> Result<Utf8PathBuf, PathError> {
    if p.exists() {
        let canonical = p
            .canonicalize_utf8()
            .map_err(|source| PathError::Filesystem {
                path: p.to_path_buf(),
                source,
            })?;
        return Ok(simplified(&canonical));
    }
    canonicalize_lexical(p)
}

/// Lexically canonicalize a non-existent path to absolute form.
///
/// When the path has an existing parent directory, the parent is
/// canonicalized through the filesystem and the final component is
/// re-joined — this resolves symlinks in the parent chain while
/// tolerating a not-yet-created leaf. Otherwise the path is joined onto
/// the canonical current working directory (after which any remaining
/// `.` / `..` segments are folded out lexically).
fn canonicalize_lexical(p: &Utf8Path) -> Result<Utf8PathBuf, PathError> {
    if let (Some(parent), Some(file_name)) = (p.parent(), p.file_name())
        && !parent.as_str().is_empty()
        && parent.exists()
    {
        let canonical_parent =
            parent
                .canonicalize_utf8()
                .map_err(|source| PathError::Filesystem {
                    path: parent.to_path_buf(),
                    source,
                })?;
        return Ok(simplified(&canonical_parent).join(file_name));
    }

    let base = if p.is_absolute() {
        Utf8PathBuf::new()
    } else {
        let cwd_std = env::current_dir().map_err(|source| PathError::CwdUnavailable {
            path: p.to_path_buf(),
            source,
        })?;
        Utf8PathBuf::from_path_buf(cwd_std).map_err(|cwd| PathError::CwdNotUtf8 {
            path: p.to_path_buf(),
            cwd,
        })?
    };

    Ok(fold_dot_segments(&base.join(p)))
}

/// Strip a Windows verbatim (`\\?\` / `\\?\UNC\`) path prefix where the
/// plain form is equivalent, delegating to [`dunce::simplified`].
///
/// `canonicalize_utf8` (like `std::fs::canonicalize`) returns the verbatim
/// form on Windows; left intact it surfaces as `\\?\C:\…` in user-facing
/// hints, the persisted default-repo pointer, and journal records. `dunce`
/// is the de-facto-standard crate for this normalization and preserves the
/// verbatim prefix for paths that genuinely require it (those exceeding the
/// legacy `MAX_PATH`). On non-Windows targets it is the identity function.
#[must_use = "simplified returns the normalized path; the input is not mutated"]
pub(crate) fn simplified(path: &Utf8Path) -> Utf8PathBuf {
    // `dunce::simplified` only ever strips an ASCII prefix, so a UTF-8 input
    // always yields a UTF-8 result; the fallback is unreachable but keeps the
    // conversion total without a panic path.
    Utf8PathBuf::from_path_buf(dunce::simplified(path.as_std_path()).to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

/// String-typed wrapper over [`simplified`] for byte-exact comparison of a
/// recorded path string against a freshly-read one (the `status` symlink
/// classifier): the two may differ only by a verbatim prefix for the same
/// destination, so both fold through this before comparison.
#[must_use = "simplified_str returns the normalized path string"]
pub(crate) fn simplified_str(path: &str) -> String {
    simplified(Utf8Path::new(path)).into_string()
}

/// Fold `.` and `..` segments out of an absolute path lexically,
/// without consulting the filesystem. Used only on the
/// non-existent-path branch where the leaf cannot be canonicalized
/// through the OS.
fn fold_dot_segments(path: &Utf8Path) -> Utf8PathBuf {
    let mut out: Vec<&str> = Vec::new();
    for component in path.components() {
        match component {
            camino::Utf8Component::CurDir => {}
            camino::Utf8Component::ParentDir => {
                // Pop a normal segment when one is present; keep the
                // `..` otherwise (e.g. a path that escapes its prefix).
                if matches!(out.last(), Some(&seg) if seg != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_str()),
        }
    }
    out.iter().collect()
}

/// Expand a leading `~` in user-supplied path input to `home`.
///
/// `~` alone, or `~/...`, becomes `home/...`. A `~` that is not the
/// first component (e.g. `dir/~/file`) is left untouched — only the
/// leading-tilde shell convention is honoured. Paths that do not begin
/// with `~` are returned unchanged.
///
/// This is purely lexical: it does not canonicalize, does not consult
/// the filesystem, and does not validate that `home` exists. Pipe the
/// result through [`canonicalize`] to obtain an absolute,
/// symlink-resolved form.
///
/// # Examples
///
/// ```
/// use camino::Utf8Path;
/// use patina_core::paths::expand_tilde;
///
/// let home = Utf8Path::new("/home/kevin");
/// assert_eq!(
///     expand_tilde(Utf8Path::new("~/.zshrc"), home),
///     Utf8Path::new("/home/kevin/.zshrc"),
/// );
/// assert_eq!(
///     expand_tilde(Utf8Path::new("/etc/hosts"), home),
///     Utf8Path::new("/etc/hosts"),
/// );
/// ```
#[must_use = "expand_tilde returns the expanded path; the input is not mutated"]
pub fn expand_tilde(p: &Utf8Path, home: &Utf8Path) -> Utf8PathBuf {
    let s = p.as_str();
    if s == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest);
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("create tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        // Match the production `canonicalize` path: strip the Windows
        // verbatim prefix so equality assertions hold on every target.
        let canonical = simplified(&path.canonicalize_utf8().expect("canonicalize tempdir"));
        (td, canonical)
    }

    #[test]
    fn existing_path_canonicalizes_through_filesystem() {
        let (_td, dir) = utf8_tempdir();
        // Introduce a `.` segment that filesystem canonicalization must fold.
        let with_dot = dir.join(".");
        let resolved = canonicalize(&with_dot).expect("canonicalize existing dir");
        assert_eq!(resolved, dir);
        assert!(!resolved.as_str().contains("/./"));
    }

    #[test]
    fn nonexistent_leaf_under_existing_parent_uses_parent_canonical() {
        let (_td, dir) = utf8_tempdir();
        let target = dir.join("not-created-yet.conf");
        let resolved = canonicalize(&target).expect("lexical fallback for missing leaf");
        assert_eq!(resolved, dir.join("not-created-yet.conf"));
        assert!(resolved.is_absolute());
    }

    #[test]
    fn nonexistent_relative_path_joins_canonical_cwd() {
        // A relative path whose parent does not exist falls back to the
        // canonical CWD join. Use a deep, certainly-absent path so the
        // parent-exists branch is not taken.
        let rel = Utf8PathBuf::from("definitely-absent-7f3a/nested/leaf.txt");
        let resolved = canonicalize(&rel).expect("lexical fallback to cwd");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("definitely-absent-7f3a/nested/leaf.txt"));
        assert!(!resolved.as_str().contains("/./"));
    }

    #[test]
    fn fold_dot_segments_removes_cur_and_parent() {
        let folded = fold_dot_segments(Utf8Path::new("/a/./b/../c"));
        assert_eq!(folded, Utf8PathBuf::from("/a/c"));
    }

    #[test]
    fn expand_tilde_replaces_leading_tilde() {
        let home = Utf8Path::new("/home/kevin");
        assert_eq!(
            expand_tilde(Utf8Path::new("~/.config/foo"), home),
            Utf8PathBuf::from("/home/kevin/.config/foo")
        );
        assert_eq!(expand_tilde(Utf8Path::new("~"), home), home);
    }

    #[test]
    fn expand_tilde_leaves_non_leading_tilde_untouched() {
        let home = Utf8Path::new("/home/kevin");
        assert_eq!(
            expand_tilde(Utf8Path::new("/etc/~/hosts"), home),
            Utf8PathBuf::from("/etc/~/hosts")
        );
        assert_eq!(
            expand_tilde(Utf8Path::new("relative/path"), home),
            Utf8PathBuf::from("relative/path")
        );
    }
}
