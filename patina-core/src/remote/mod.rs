//! Remote git sources: the cache, the lockfile, the update gate, and the
//! `git` subprocess layer they share.
//!
//! A module carrying a `[remote]` table resolves its entry sources against a
//! pinned checkout of another repository instead of its own directory. The
//! pieces:
//!
//! - [`git`] — typed wrappers over the `git` binary on `PATH`.
//! - [`cache`] — the per-machine checkout layout under `<state>/remotes/`.
//!
//! The normative behaviour for all of it is `docs/REMOTE_SOURCES.md`.

pub mod cache;
pub mod git;

use thiserror::Error;

/// Every way the remote subsystem can fail.
///
/// Opaque over a boxed private repr for two reasons: the failure set grows as
/// the subsystem does, and [`EngineError`](crate::error::EngineError) is
/// returned by value from every fallible engine entry point, so a wide variant
/// here would widen every `Result` in the crate.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct RemoteError(Box<RemoteRepr>);

impl<T: Into<RemoteRepr>> From<T> for RemoteError {
    fn from(value: T) -> Self {
        Self(Box::new(value.into()))
    }
}

/// The remote subsystem's failure set, private so additions stay additive.
#[derive(Debug, Error)]
pub(crate) enum RemoteRepr {
    /// A `git` invocation failed.
    #[error(transparent)]
    Git(#[from] git::GitError),

    /// A filesystem operation on the remote cache failed.
    #[error("{action} {path} failed: {source}")]
    Cache {
        /// What was being attempted, phrased to read before the path
        /// (`"reading"`, `"removing"`).
        action: &'static str,
        /// The path involved.
        path: camino::Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}
