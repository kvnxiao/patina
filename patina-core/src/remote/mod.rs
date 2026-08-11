//! Remote git sources: the cache, the lockfile, the update gate, and the
//! `git` subprocess layer they share.
//!
//! A module carrying a `[remote]` table resolves its entry sources against a
//! pinned checkout of another repository instead of its own directory. The
//! pieces:
//!
//! - [`git`] — typed wrappers over the `git` binary on `PATH`.
//! - [`cache`] — the per-machine checkout layout under `<state>/remotes/`.
//! - [`lockfile`] — the committed `patina.lock` every machine converges to.
//!
//! The normative behaviour for all of it is `docs/REMOTE_SOURCES.md`.

pub mod cache;
pub mod git;
pub mod lockfile;

use camino::Utf8Path;
use camino::Utf8PathBuf;
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

impl RemoteError {
    /// Replace the placeholder lockfile path a string-level parse carries with
    /// the real one, so the message names the file the user can open.
    ///
    /// [`lockfile::Lockfile::parse`] works on text and cannot know the path;
    /// [`lockfile::Lockfile::load`] does, and calls this on the way out.
    pub(crate) fn with_lockfile_path(mut self, path: &Utf8Path) -> Self {
        if let RemoteRepr::LockfileToml {
            path: placeholder, ..
        } = self.0.as_mut()
        {
            *placeholder = path.to_path_buf();
        }
        self
    }

    /// The plan-time failure for a remote-backed module with no pin.
    #[must_use = "the error tells the user which command creates the first pin"]
    pub fn missing_lock_entry(module: &str) -> Self {
        RemoteRepr::MissingLockEntry {
            module: module.to_owned(),
        }
        .into()
    }

    /// Restate a fetch failure as the cold-cache failure, naming the module and
    /// the rev this machine could not materialize.
    ///
    /// The underlying `git` error alone says a fetch failed; the user needs to
    /// know *which pin* they cannot converge to, since that is what a `git
    /// pull` or a network fix has to satisfy. A failure that is not a `git`
    /// failure (a cache write, say) already names its own path and passes
    /// through.
    #[must_use = "the restated error is what the user sees"]
    pub fn into_cold_cache(self, module: &str, rev: &str) -> Self {
        match *self.0 {
            RemoteRepr::Git(source) => RemoteRepr::ColdCache {
                module: module.to_owned(),
                rev: rev.to_owned(),
                source,
            }
            .into(),
            other => Self(Box::new(other)),
        }
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
        path: Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The lockfile could not be read or written.
    #[error("failed to access the lockfile {path}: {source}")]
    LockfileIo {
        /// The lockfile path.
        path: Utf8PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The lockfile is not valid TOML.
    #[error("failed to parse {path} as TOML: {source}")]
    LockfileToml {
        /// The lockfile path.
        path: Utf8PathBuf,
        /// The underlying TOML error.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// The lockfile declares a layout this binary does not understand.
    #[error(
        "patina.lock declares version {found}, but this patina understands version \
         {supported}; upgrade patina to use this repository"
    )]
    LockfileVersion {
        /// The version read from the file.
        found: u32,
        /// The version this binary writes and reads.
        supported: u32,
    },

    /// A lock entry's `rev` is not a full commit SHA.
    #[error(
        "the lock entry for remote `{module}` records `rev = \"{value}\"`, which is not a full \
         40-character commit SHA; re-pin it with `patina remote update {module}`"
    )]
    LockfileRev {
        /// The module whose entry is malformed.
        module: String,
        /// The offending value.
        value: String,
    },

    /// A lock entry's `updated_at` is not an RFC 3339 timestamp.
    #[error(
        "the lock entry for remote `{module}` records `updated_at = \"{value}\"`, which is not an \
         RFC 3339 UTC timestamp (for example 2026-08-11T14:00:00Z)"
    )]
    LockfileTimestamp {
        /// The module whose entry is malformed.
        module: String,
        /// The offending value.
        value: String,
    },

    /// A remote-backed module has no pin, so there is nothing for `apply` to
    /// materialize.
    #[error(
        "the remote-backed module `{module}` has no entry in patina.lock; run \
         `patina remote update {module}` to create its first pin"
    )]
    MissingLockEntry {
        /// The module with no pin.
        module: String,
    },

    /// A pinned rev is neither cached nor fetchable, so this machine cannot
    /// converge to the committed lock.
    #[error(
        "the remote-backed module `{module}` is pinned to rev {rev}, which is not in the local \
         cache and could not be fetched: {source}"
    )]
    ColdCache {
        /// The module whose pin could not be materialized.
        module: String,
        /// The pinned rev.
        rev: String,
        /// The fetch failure.
        #[source]
        source: git::GitError,
    },
}
