//! TOML schema parsing for the `[[file]]`, `[[directory]]`, and
//! `[[hook]]` table arrays declared inside a module's `patina.toml`
//! (REQ-001 / SPEC-0001 REQ-006).
//!
//! This module owns parsing and validation only — the resulting
//! [`ModuleConfig`] is consumed by later subsystems:
//!
//! - File mode executors read [`ManagedEntry`] / [`FileMode`].
//! - Hook execution reads [`HookEntry`] / [`HookEvent`].
//! - The variable resolver (T-006) consumes the raw `[variables]` table
//!   preserved here so it does not need a second TOML pass.
//!
//! `[variables]` is intentionally captured as a raw `toml::Value::Table`
//! (boxed to keep the [`ModuleConfig`] enum size bounded). This task
//! does not validate variable keys against the reserved `patina.*`
//! namespace — that is T-006's job. Capturing the raw table here is the
//! handoff.

pub mod file_entry;
pub mod hook_entry;
pub mod root;
pub mod writer;

use camino::Utf8Path;
use camino::Utf8PathBuf;
pub use file_entry::EntryKind;
pub use file_entry::FileEntry;
pub use file_entry::FileEntryError;
pub use file_entry::FileMode;
pub use file_entry::ManagedEntry;
pub use hook_entry::HookEntry;
pub use hook_entry::HookEntryError;
pub use hook_entry::HookEvent;
pub use root::RootConfig;
pub use root::RootConfigError;
pub use root::parse_root_config;
pub use root::parse_root_config_str;
use serde::Deserialize;
pub use writer::ConfigWriteError;
pub use writer::append_file_entry;
pub use writer::remove_file_entry;
pub use writer::scaffold_root_manifest;

/// Parsed and validated contents of a module's `patina.toml`.
///
/// Carries the two table arrays defined by REQ-005 / REQ-006 plus the
/// raw `[variables]` table preserved for T-006's resolver to ingest.
#[derive(Debug, Clone, Default)]
pub struct ModuleConfig {
    /// Validated `[[file]]`-kind entries in declaration order.
    pub files: Vec<ManagedEntry>,
    /// Validated `[[directory]]`-kind entries in declaration order.
    /// Consumed by the plan loop in T-002; this task only parses them.
    pub directories: Vec<ManagedEntry>,
    /// Validated `[[hook]]` entries in declaration order.
    pub hooks: Vec<HookEntry>,
    /// Raw `[variables]` table for T-006 to consume. `None` when
    /// the module declares no `[variables]` table.
    pub variables: Option<toml::value::Table>,
}

/// Failure modes returned by [`parse_module_config`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigParseError {
    /// IO failure reading the manifest file.
    #[error("failed to read {path}: {source}")]
    Io {
        /// The manifest path that failed to read.
        path: Utf8PathBuf,
        #[source]
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// TOML deserialization of the raw document failed.
    #[error("failed to parse {path} as TOML: {source}")]
    Toml {
        /// The manifest path whose TOML failed to parse.
        path: Utf8PathBuf,
        #[source]
        /// The underlying TOML deserialization error.
        source: Box<toml::de::Error>,
    },

    /// A `[[file]]` entry violated REQ-005's parse-time rules.
    #[error(transparent)]
    FileEntry(#[from] file_entry::FileEntryError),

    /// A `[[hook]]` entry violated REQ-006's parse-time rules.
    #[error(transparent)]
    HookEntry(#[from] hook_entry::HookEntryError),

    /// A `[variables]` table declared a key inside the reserved
    /// `patina.*` namespace (REQ-007).
    #[error(transparent)]
    Variable(#[from] crate::variables::VariableError),
}

/// Read and parse a module manifest at `path`, returning a fully
/// validated [`ModuleConfig`].
///
/// # Errors
///
/// Returns [`ConfigParseError::Io`] on IO failure, [`ConfigParseError::Toml`]
/// on a malformed TOML document, a `FileEntry` / `HookEntry`
/// variant when one of the table-array rules in REQ-005 / REQ-006 is
/// violated, and [`ConfigParseError::Variable`] when a `[variables]`
/// key falls inside the reserved `patina.*` namespace (REQ-007).
pub fn parse_module_config(path: &Utf8Path) -> Result<ModuleConfig, ConfigParseError> {
    let text =
        fs_err::read_to_string(path.as_std_path()).map_err(|source| ConfigParseError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    parse_module_config_str(&text)
}

/// Parse a module manifest from an in-memory string. Used by tests and
/// callers that have already read the file.
///
/// # Errors
///
/// Returns [`ConfigParseError::Toml`] on a malformed TOML document, a
/// `FileEntry` / `HookEntry` variant when one of the table-array rules
/// in REQ-005 / REQ-006 is violated, and [`ConfigParseError::Variable`]
/// when a `[variables]` key falls inside the reserved `patina.*`
/// namespace (REQ-007).
pub fn parse_module_config_str(text: &str) -> Result<ModuleConfig, ConfigParseError> {
    let raw: RawModule = toml::from_str(text).map_err(|source| ConfigParseError::Toml {
        path: Utf8PathBuf::from("<memory>"),
        source: Box::new(source),
    })?;

    let mut files = Vec::with_capacity(raw.file.len());
    for raw_file in raw.file {
        files.push(ManagedEntry::from_raw_file(raw_file)?);
    }

    let mut directories = Vec::with_capacity(raw.directory.len());
    for raw_dir in raw.directory {
        directories.push(ManagedEntry::from_raw_directory(raw_dir)?);
    }

    let mut hooks = Vec::with_capacity(raw.hook.len());
    for raw_hook in raw.hook {
        hooks.push(HookEntry::from_raw(raw_hook)?);
    }

    if let Some(table) = raw.variables.as_ref() {
        crate::variables::reject_reserved_keys(table.keys().map(String::as_str))?;
    }

    Ok(ModuleConfig {
        files,
        directories,
        hooks,
        variables: raw.variables,
    })
}

/// Raw TOML projection of a module manifest. The `[[file]]`,
/// `[[directory]]`, and `[[hook]]` table arrays are captured as their raw
/// forms; the `from_raw_*` constructors on [`ManagedEntry`] /
/// [`HookEntry`] apply REQ-001 / SPEC-0001 REQ-006's validation rules.
#[derive(Debug, Default, Deserialize)]
struct RawModule {
    /// Repository-root marker; preserved so the de-serializer accepts
    /// (and ignores) the root manifest's `[patina]` table without
    /// erroring on the unknown key.
    #[serde(default, rename = "patina")]
    _patina: Option<toml::Value>,

    /// Per-module `[variables]` table, preserved verbatim for T-006.
    #[serde(default)]
    variables: Option<toml::value::Table>,

    /// Raw `[[file]]` table-array entries; validated by
    /// [`ManagedEntry::from_raw_file`].
    #[serde(default)]
    file: Vec<file_entry::RawEntry>,

    /// Raw `[[directory]]` table-array entries; validated by
    /// [`ManagedEntry::from_raw_directory`].
    #[serde(default)]
    directory: Vec<file_entry::RawEntry>,

    /// Raw `[[hook]]` table-array entries; validated by
    /// [`HookEntry::from_raw`].
    #[serde(default)]
    hook: Vec<hook_entry::RawHookEntry>,
}
