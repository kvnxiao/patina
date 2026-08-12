//! Remote git sources: the cache, the lockfile, the update gate, and the
//! `git` subprocess layer they share.
//!
//! The root manifest declares each remote once; a managed entry naming one
//! resolves its source against a pinned checkout of that repository instead of
//! its own module directory. The pieces:
//!
//! - [`git`] wraps the `git` binary on `PATH` in typed calls.
//! - [`cache`] owns the per-machine checkout layout under `<state>/remotes/`.
//! - [`lockfile`] reads the committed `patina.lock` every machine converges to.
//! - [`gate`] holds the four checks a candidate tip must clear to become a pin.
//! - [`update`] enumerates remotes and proposes pin bumps through the gate.
//! - [`notice`] maintains the pending-update file and its throttle stamp.
//!
//! The normative behaviour for all of it is `docs/REMOTE_SOURCES.md`.

pub mod cache;
pub mod gate;
pub mod git;
pub mod lockfile;
pub mod notice;
pub mod update;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;

/// Every way the remote subsystem can fail.
///
/// Opaque over a boxed private repr for two reasons. The failure set grows as
/// the subsystem does. And [`EngineError`](crate::error::EngineError) is
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

    /// The plan-time failure for an entry naming a remote the root manifest
    /// does not declare.
    #[must_use = "the error names the remote no declaration matches"]
    pub fn undeclared_remote(name: &str) -> Self {
        RemoteRepr::UndeclaredRemote {
            name: name.to_owned(),
        }
        .into()
    }

    /// The plan-time failure for a declared remote with no pin.
    #[must_use = "the error tells the user which command creates the first pin"]
    pub fn missing_lock_entry(name: &str) -> Self {
        RemoteRepr::MissingLockEntry {
            name: name.to_owned(),
        }
        .into()
    }

    /// The plan-time failure for a remote-sourced entry whose source resolves
    /// outside its checkout (a `..` in the declared source, or a symlink the
    /// checkout shipped).
    #[must_use = "the error names the source that escaped its checkout"]
    pub fn source_escapes_checkout(name: &str, source: &Utf8Path) -> Self {
        RemoteRepr::SourceEscapesCheckout {
            name: name.to_owned(),
            declared_source: source.to_path_buf(),
        }
        .into()
    }

    /// The plan-time failure for a remote checkout that holds a symbolic link.
    #[must_use = "the error names the link that would dereference outside the checkout"]
    pub fn symlink_in_checkout(name: &str, path: &Utf8Path) -> Self {
        RemoteRepr::SymlinkInCheckout {
            name: name.to_owned(),
            path: path.to_path_buf(),
        }
        .into()
    }

    /// The plan-time failure for a remote whose checkout was expected on this
    /// machine but is not materialized.
    #[must_use = "the error names the remote whose checkout is missing"]
    pub fn checkout_not_materialized(name: &str) -> Self {
        RemoteRepr::CheckoutNotMaterialized {
            name: name.to_owned(),
        }
        .into()
    }

    /// Restate a fetch failure as the cold-cache failure, naming the remote and
    /// the rev this machine could not materialize.
    ///
    /// The underlying `git` error alone says a fetch failed. The user needs to
    /// know *which pin* they cannot converge to, because that is what a
    /// `git pull` or a network fix has to satisfy. A failure that is not a
    /// `git` failure, a cache write for instance, already names its own path
    /// and passes through.
    #[must_use = "the restated error is what the user sees"]
    pub fn into_cold_cache(self, name: &str, rev: &str) -> Self {
        match *self.0 {
            RemoteRepr::Git(source) => RemoteRepr::ColdCache {
                name: name.to_owned(),
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
        "the lock entry for remote `{name}` records `rev = \"{value}\"`, which is not a full \
         40-character commit SHA; re-pin it with `patina remote update {name}`"
    )]
    LockfileRev {
        /// The remote whose entry is malformed.
        name: String,
        /// The offending value.
        value: String,
    },

    /// A lock entry's `updated_at` is not an RFC 3339 timestamp.
    #[error(
        "the lock entry for remote `{name}` records `updated_at = \"{value}\"`, which is not an \
         RFC 3339 UTC timestamp (for example 2026-08-11T14:00:00Z)"
    )]
    LockfileTimestamp {
        /// The remote whose entry is malformed.
        name: String,
        /// The offending value.
        value: String,
    },

    /// A `[remotes.<name>]` key is not a usable remote name.
    #[error("the lock entry key `{name}` is not a usable remote name: {source}")]
    LockfileName {
        /// The offending key.
        name: String,
        /// Why the name is not usable.
        #[source]
        source: crate::config::remote::RemoteConfigError,
    },

    /// Two `[remotes.<name>]` tables address one remote.
    #[error(
        "the lockfile carries two entries for the remote `{name}`; a remote has one pin, and \
         names are compared ignoring case and Unicode normalization, so the two cannot both be \
         honoured. Delete the stale entry, or re-pin with `patina remote update {name}`"
    )]
    LockfileDuplicate {
        /// The name claimed twice.
        name: String,
    },

    /// An entry named a remote no `[[remote]]` table declares.
    #[error(
        "an entry names the remote `{name}`, which no [[remote]] table in the root patina.toml \
         declares; add one, or correct the entry's `remote` key"
    )]
    UndeclaredRemote {
        /// The name nothing declares.
        name: String,
    },

    /// A remote-sourced entry's source resolved outside its checkout directory.
    #[error(
        "an entry sourced from the remote `{name}` declares a source (`{declared_source}`) that \
         resolves outside its checkout; a remote may supply only bytes from within its own tree"
    )]
    SourceEscapesCheckout {
        /// The remote whose checkout the entry escaped.
        name: String,
        /// The declared source that resolved outside the checkout.
        declared_source: Utf8PathBuf,
    },

    /// A remote checkout holds a symbolic link, which the executors would
    /// dereference and which could therefore read or plant paths outside the
    /// checkout. Patina's own materialization writes with `core.symlinks=false`
    /// and never produces one, so the checkout was made or altered by something
    /// else.
    #[error(
        "the checkout for remote `{name}` contains a symbolic link ({path}); refusing to deploy \
         from it. Remove the cache directory and re-run `patina apply` to re-materialize"
    )]
    SymlinkInCheckout {
        /// The remote whose checkout holds the link.
        name: String,
        /// The link itself.
        path: Utf8PathBuf,
    },

    /// A remote's checkout was expected to be materialized but is not.
    #[error(
        "the checkout for remote `{name}` is not materialized on this machine; run `patina apply`"
    )]
    CheckoutNotMaterialized {
        /// The remote with no checkout.
        name: String,
    },

    /// A declared remote has no pin, so there is nothing for `apply` to
    /// materialize.
    #[error(
        "the remote `{name}` has no entry in patina.lock; run \
         `patina remote update {name}` to create its first pin"
    )]
    MissingLockEntry {
        /// The remote with no pin.
        name: String,
    },

    /// A pinned rev is neither cached nor fetchable, so this machine cannot
    /// converge to the committed lock.
    #[error(
        "the remote `{name}` is pinned to rev {rev}, which is not in the local cache and could \
         not be fetched: {source}"
    )]
    ColdCache {
        /// The remote whose pin could not be materialized.
        name: String,
        /// The pinned rev.
        rev: String,
        /// The fetch failure.
        #[source]
        source: git::GitError,
    },
}
