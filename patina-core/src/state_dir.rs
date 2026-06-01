//! Per-machine state directory resolution (REQ-016).
//!
//! The engine stores its journal, backups, persisted profile choice,
//! persisted default repository path, and advisory lock under a single
//! per-machine directory whose location depends on the host OS:
//!
//! - **Linux:** `$XDG_STATE_HOME/patina/` when `XDG_STATE_HOME` is set and
//!   non-empty; otherwise `$HOME/.local/state/patina/`.
//! - **macOS:** `$HOME/Library/Application Support/patina/`.
//! - **Windows:** `%LOCALAPPDATA%\patina\`.
//!
//! [`resolve`] is the public entry point — it inspects the running
//! host, reads the process environment, materializes the directory
//! tree (`<state>/patina/`, `<state>/patina/journal/`,
//! `<state>/patina/backups/`) on first call, and returns the
//! canonical absolute path. It is idempotent — a second call on the
//! same host returns the same path and is a filesystem no-op.
//!
//! The lazily-created files `profile`, `default_repo`, and `lock`
//! belong to their owning subsystems (T-007, T-003, T-013); this
//! module only creates the directory tree.
//!
//! `<state>/patina/logs/` is deliberately NOT created here. The watcher
//! owns that directory and its rotating-log stack (SPEC-0003 REQ-009),
//! creating it lazily on first start via [`crate::watch::logging`];
//! [`resolve`] creates only `journal/` and `backups/`.
//!
//! The dotfiles repository is never written to by this module.
//!
//! # Examples
//!
//! ```no_run
//! let state = patina_core::state_dir::resolve()?;
//! assert!(state.join("journal").is_dir());
//! assert!(state.join("backups").is_dir());
//! # Ok::<(), patina_core::state_dir::StateDirError>(())
//! ```

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;

/// Host operating-system family relevant to state-directory layout.
///
/// Linux and the BSDs share the XDG layout; everything that is not
/// macOS or Windows is treated as the XDG family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    /// Linux and other XDG-Base-Directory systems.
    Linux,
    /// Apple macOS (and other Darwin variants).
    MacOs,
    /// Microsoft Windows.
    Windows,
}

impl HostOs {
    /// Return the host family detected at compile time from
    /// `std::env::consts::OS`. Falls back to [`HostOs::Linux`] for any
    /// unknown OS string so XDG semantics apply on the BSDs.
    #[must_use = "the resolved host family selects the state-directory layout"]
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::MacOs,
            "windows" => Self::Windows,
            _ => Self::Linux,
        }
    }
}

/// Errors returned by [`resolve`] and [`resolve_with_env`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StateDirError {
    /// A required environment variable was unset or empty and no
    /// fallback was available. On Linux this is `HOME`; on macOS this
    /// is `HOME`; on Windows this is `LOCALAPPDATA`.
    #[error(
        "state directory cannot be resolved: required environment variable `{name}` is unset or empty"
    )]
    MissingEnv {
        /// The environment variable whose absence triggered the error.
        name: &'static str,
    },

    /// A resolved path was not valid UTF-8. Patina mandates UTF-8
    /// paths everywhere except at OS-API boundaries.
    #[error("state directory path is not valid UTF-8: {raw}")]
    NonUtf8Path {
        /// The lossy rendering of the offending path.
        raw: String,
    },

    /// Creating one of the state-directory tree members failed.
    #[error("failed to create state directory entry `{path}`: {source}")]
    Io {
        /// The path the engine attempted to create.
        path: Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Resolve and materialize the per-machine state directory for the
/// running host.
///
/// On first call this creates `<state>/patina/`,
/// `<state>/patina/journal/`, and `<state>/patina/backups/`. On
/// subsequent calls the function is a filesystem no-op and returns
/// the same path.
///
/// # Errors
///
/// Returns [`StateDirError::MissingEnv`] when a required environment
/// variable is unset or empty, [`StateDirError::NonUtf8Path`] when a
/// resolved path is not valid UTF-8, and [`StateDirError::Io`] when
/// directory creation fails.
pub fn resolve() -> Result<Utf8PathBuf, StateDirError> {
    resolve_with_env(HostOs::current(), |name| {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    })
}

/// Resolve and materialize the per-machine state directory using an
/// explicit host family and environment-lookup closure.
///
/// This is the testable core of [`resolve`]. The closure receives an
/// environment-variable name and returns `Some(value)` when the
/// variable is set to a non-empty string, `None` otherwise.
///
/// # Errors
///
/// See [`resolve`].
pub fn resolve_with_env<F>(host: HostOs, env: F) -> Result<Utf8PathBuf, StateDirError>
where
    F: Fn(&str) -> Option<String>,
{
    let root = compute_root(host, &env)?;
    create_tree(&root)?;
    Ok(root)
}

/// Compute the state-directory root for `host` from `env` without
/// touching the filesystem.
///
/// This is the pure, side-effect-free core of [`resolve`]: it does not
/// create any directory. [`resolve`] layers directory materialization on
/// top; repository discovery ([`crate::discovery`]) calls this directly so
/// it can locate the persisted-default file without the side effect of
/// creating the state tree on a read-only path.
pub(crate) fn compute_root<F>(host: HostOs, env: &F) -> Result<Utf8PathBuf, StateDirError>
where
    F: Fn(&str) -> Option<String>,
{
    match host {
        HostOs::Linux => {
            if let Some(xdg) = env("XDG_STATE_HOME") {
                Ok(Utf8PathBuf::from(xdg).join("patina"))
            } else {
                let home = env("HOME").ok_or(StateDirError::MissingEnv { name: "HOME" })?;
                Ok(Utf8PathBuf::from(home)
                    .join(".local")
                    .join("state")
                    .join("patina"))
            }
        }
        HostOs::MacOs => {
            let home = env("HOME").ok_or(StateDirError::MissingEnv { name: "HOME" })?;
            Ok(Utf8PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("patina"))
        }
        HostOs::Windows => {
            let local = env("LOCALAPPDATA").ok_or(StateDirError::MissingEnv {
                name: "LOCALAPPDATA",
            })?;
            Ok(Utf8PathBuf::from(local).join("patina"))
        }
    }
}

/// Create the patina state root and its two required subdirectories
/// (`journal/` and `backups/`). Idempotent — pre-existing directories
/// are not an error.
fn create_tree(root: &Utf8Path) -> Result<(), StateDirError> {
    create_dir_idempotent(root)?;
    create_dir_idempotent(&root.join("journal"))?;
    create_dir_idempotent(&root.join("backups"))?;
    Ok(())
}

fn create_dir_idempotent(path: &Utf8Path) -> Result<(), StateDirError> {
    fs_err::create_dir_all(path.as_std_path()).map_err(|source| StateDirError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            owned
                .iter()
                .find_map(|(k, v)| (k == name).then(|| v.clone()))
                .filter(|v| !v.is_empty())
        }
    }

    #[test]
    fn linux_with_xdg_state_home_uses_xdg_layout() {
        let env = env_map(&[("XDG_STATE_HOME", "/x/y"), ("HOME", "/home/alice")]);
        let root = compute_root(HostOs::Linux, &env).expect("compute root");
        assert_eq!(root, Utf8PathBuf::from("/x/y/patina"));
    }

    #[test]
    fn linux_with_empty_xdg_state_home_falls_back_to_home() {
        let env = env_map(&[("XDG_STATE_HOME", ""), ("HOME", "/home/alice")]);
        let root = compute_root(HostOs::Linux, &env).expect("compute root");
        assert_eq!(root, Utf8PathBuf::from("/home/alice/.local/state/patina"));
    }

    #[test]
    fn linux_without_xdg_state_home_falls_back_to_home() {
        let env = env_map(&[("HOME", "/home/alice")]);
        let root = compute_root(HostOs::Linux, &env).expect("compute root");
        assert_eq!(root, Utf8PathBuf::from("/home/alice/.local/state/patina"));
    }

    #[test]
    fn linux_without_home_or_xdg_errors_with_home_named() {
        let env = env_map(&[]);
        let err = compute_root(HostOs::Linux, &env).expect_err("must error");
        assert!(
            matches!(err, StateDirError::MissingEnv { name: "HOME" }),
            "got {err:?}"
        );
    }

    #[test]
    fn macos_uses_application_support_under_home() {
        let env = env_map(&[("HOME", "/Users/alice")]);
        let root = compute_root(HostOs::MacOs, &env).expect("compute root");
        assert_eq!(
            root,
            Utf8PathBuf::from("/Users/alice/Library/Application Support/patina")
        );
    }

    #[test]
    fn macos_without_home_errors_with_home_named() {
        let env = env_map(&[]);
        let err = compute_root(HostOs::MacOs, &env).expect_err("must error");
        assert!(
            matches!(err, StateDirError::MissingEnv { name: "HOME" }),
            "got {err:?}"
        );
    }

    #[test]
    fn windows_uses_localappdata() {
        let env = env_map(&[("LOCALAPPDATA", r"C:\Users\Kevin\AppData\Local")]);
        let root = compute_root(HostOs::Windows, &env).expect("compute root");
        assert_eq!(
            root,
            Utf8PathBuf::from(r"C:\Users\Kevin\AppData\Local").join("patina")
        );
    }

    #[test]
    fn windows_without_localappdata_errors_with_localappdata_named() {
        let env = env_map(&[]);
        let err = compute_root(HostOs::Windows, &env).expect_err("must error");
        assert!(
            matches!(
                err,
                StateDirError::MissingEnv {
                    name: "LOCALAPPDATA"
                }
            ),
            "got {err:?}"
        );
    }
}
