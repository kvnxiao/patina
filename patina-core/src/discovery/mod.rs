//! Repository and module discovery for Patina.
//!
//! This module covers two adjacent concerns that share a parser for
//! the root `patina.toml`'s `[patina]` table:
//!
//! - [`repo`] resolves the dotfiles repository root from the `PATINA_REPO`
//!   environment variable, a walk-up from the current working directory, or a
//!   persisted default path under the per-machine state directory.
//! - [`modules`] enumerates per-module `patina.toml` files in immediate
//!   subdirectories of the resolved root.
//!
//! The persisted-default path is computed OS-specifically inside
//! [`repo`] so [`repo::resolve_repository_root`] can attempt all three
//! sources for its triple-fail error message.

pub mod modules;
pub mod repo;

use camino::Utf8Path;
pub use modules::ModuleDiscoveryError;
pub use modules::ModuleHandle;
pub use modules::discover_modules;
pub use repo::PERSISTED_DEFAULT_FILENAME;
pub use repo::RepoDiscoveryError;
pub use repo::default_repo_pointer_path;
pub use repo::persisted_default_present;
pub use repo::resolve_repository_root;
pub use repo::resolve_repository_root_with;
pub use repo::write_persisted_default;
use serde::Deserialize;

/// Filename of the per-module / per-root Patina manifest.
pub(crate) const MANIFEST_FILENAME: &str = "patina.toml";

/// Minimal projection of `patina.toml` covering only the `[patina]`
/// table fields this module needs to validate.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct ManifestHead {
    #[serde(default)]
    pub(crate) patina: PatinaTable,
}

/// Subset of `[patina]` table fields consumed by discovery.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct PatinaTable {
    #[serde(default)]
    pub(crate) root: Option<bool>,
}

/// Parse the minimal manifest head from a `patina.toml` file on disk.
///
/// Returns the IO error (with the path attached via `fs-err`) on read
/// failure and the TOML parse error otherwise. The caller decides
/// which discovery error variant to wrap it in.
pub(crate) fn read_manifest_head(path: &Utf8Path) -> Result<ManifestHead, ManifestHeadError> {
    let text =
        fs_err::read_to_string(path.as_std_path()).map_err(|source| ManifestHeadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    toml::from_str::<ManifestHead>(&text).map_err(|source| ManifestHeadError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// Failure modes when reading the `[patina]` head of a manifest.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ManifestHeadError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: camino::Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path} as TOML: {source}")]
    Parse {
        path: camino::Utf8PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
}
