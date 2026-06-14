//! Module enumeration.
//!
//! Walks the resolved repository root and returns module handles for
//! every `patina.toml` in an immediate subdirectory of the root.
//! Three structural failure modes are surfaced as distinguishable
//! typed errors: depth-≥2 manifests, non-root manifests declaring
//! `root = true`, and root manifests missing `root = true`.

use super::MANIFEST_FILENAME;
use super::ManifestHeadError;
use super::read_manifest_head;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Resolved per-module handle. Carries the module's directory name
/// (the immediate subdirectory of the repository root) and the
/// absolute path to its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleHandle {
    /// The module's directory name (e.g. `"zsh"`).
    pub name: String,
    /// Absolute path to the module's directory.
    pub path: Utf8PathBuf,
}

/// Errors returned from [`discover_modules`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModuleDiscoveryError {
    /// A `patina.toml` was found at depth ≥ 2 below the root.
    #[error(
        "{path}: maximum module depth (1) exceeded; modules must live in immediate subdirectories of the repository root"
    )]
    MaximumModuleDepth {
        /// The offending `patina.toml` path at depth ≥ 2.
        path: Utf8PathBuf,
    },

    /// A non-root `patina.toml` declared the reserved `root = true`
    /// key. The Display names both the offending file and the
    /// unexpected key.
    #[error(
        "{path}: unexpected `root = true` key; only the repository-root patina.toml may declare `root`"
    )]
    UnexpectedRootKey {
        /// The non-root manifest that declared `root = true`.
        path: Utf8PathBuf,
    },

    /// The root `patina.toml` did not declare `root = true`.
    #[error(
        "{path}: missing `root = true` in `[patina]` table; the repository-root manifest must declare it"
    )]
    MissingRootKey {
        /// The root manifest missing the `root = true` declaration.
        path: Utf8PathBuf,
    },

    /// An IO error while traversing the repository.
    #[error("failed to read {path}: {source}")]
    Io {
        /// The path being traversed when IO failed.
        path: Utf8PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// A `patina.toml` failed to parse.
    #[error("failed to parse {path} as TOML: {source}")]
    Parse {
        /// The manifest file that failed to parse.
        path: Utf8PathBuf,
        /// The underlying TOML parse error.
        #[source]
        source: Box<toml::de::Error>,
    },
}

impl From<ManifestHeadError> for ModuleDiscoveryError {
    fn from(value: ManifestHeadError) -> Self {
        match value {
            ManifestHeadError::Io { path, source } => Self::Io { path, source },
            ManifestHeadError::Parse { path, source } => Self::Parse { path, source },
        }
    }
}

/// Walk the resolved repository `root` and return every module
/// declared by an immediate-subdirectory `patina.toml`.
///
/// Returns a vector ordered alphabetically by module name. The root
/// manifest is validated (must declare `root = true`) and per-module
/// manifests are validated (must omit `root` or declare it `false`).
/// A `patina.toml` at depth ≥ 2 below the root is a hard error
/// (here we only validate structure).
///
/// # Errors
///
/// See [`ModuleDiscoveryError`].
pub fn discover_modules(root: &Utf8Path) -> Result<Vec<ModuleHandle>, ModuleDiscoveryError> {
    // Validate root manifest.
    let root_manifest = root.join(MANIFEST_FILENAME);
    let root_head = read_manifest_head(&root_manifest)?;
    if root_head.patina.root != Some(true) {
        return Err(ModuleDiscoveryError::MissingRootKey {
            path: root_manifest,
        });
    }

    let mut handles: Vec<ModuleHandle> = Vec::new();

    // Iterate immediate children of root.
    let entries =
        fs_err::read_dir(root.as_std_path()).map_err(|source| ModuleDiscoveryError::Io {
            path: root.to_path_buf(),
            source,
        })?;

    for entry in entries {
        let entry = entry.map_err(|source| ModuleDiscoveryError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| ModuleDiscoveryError::Io {
                path: root.to_path_buf(),
                source,
            })?;
        if !file_type.is_dir() {
            continue;
        }
        let child_std = entry.path();
        let child =
            Utf8PathBuf::from_path_buf(child_std).map_err(|p| ModuleDiscoveryError::Io {
                path: root.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("non-UTF-8 path under repository root: {}", p.display()),
                ),
            })?;
        let module_manifest = child.join(MANIFEST_FILENAME);

        // First check for depth-≥2 manifests under this subdirectory.
        check_no_deep_manifests(&child)?;

        if !module_manifest.is_file() {
            // Subdirectory without a patina.toml is silently skipped —
            // not every directory under the root is a module (e.g.
            // `.git/`, scratch files). Stricter module-content
            // validation is added later.
            continue;
        }

        let head = read_manifest_head(&module_manifest)?;
        if head.patina.root == Some(true) {
            return Err(ModuleDiscoveryError::UnexpectedRootKey {
                path: module_manifest,
            });
        }

        let name = child
            .file_name()
            .ok_or_else(|| ModuleDiscoveryError::Io {
                path: child.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "subdirectory has no file name component",
                ),
            })?
            .to_owned();
        handles.push(ModuleHandle { name, path: child });
    }

    handles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(handles)
}

/// Recursively check that no `patina.toml` lives at depth ≥ 1 below
/// `module_dir` (i.e. depth ≥ 2 below the repository root).
fn check_no_deep_manifests(module_dir: &Utf8Path) -> Result<(), ModuleDiscoveryError> {
    let Ok(entries) = fs_err::read_dir(module_dir.as_std_path()) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|source| ModuleDiscoveryError::Io {
            path: module_dir.to_path_buf(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| ModuleDiscoveryError::Io {
                path: module_dir.to_path_buf(),
                source,
            })?;
        if !file_type.is_dir() {
            continue;
        }
        let child_std = entry.path();
        let child =
            Utf8PathBuf::from_path_buf(child_std).map_err(|p| ModuleDiscoveryError::Io {
                path: module_dir.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("non-UTF-8 path under repository root: {}", p.display()),
                ),
            })?;
        let candidate = child.join(MANIFEST_FILENAME);
        if candidate.is_file() {
            return Err(ModuleDiscoveryError::MaximumModuleDepth { path: candidate });
        }
        check_no_deep_manifests(&child)?;
    }
    Ok(())
}
