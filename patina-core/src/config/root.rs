//! Parsing of the repository root `patina.toml`'s variable tables.
//!
//! The root manifest carries two variable sources that planning layers
//! into the resolver (wired elsewhere, not here):
//!
//! - the repo-shared `[variables]` table, and
//! - one `[profiles.<name>.variables]` table per declared profile.
//!
//! This module is parse-and-return only: it reads the root manifest,
//! validates each table against the reserved `patina.*` namespace via
//! [`crate::variables::reject_reserved_keys`], and hands back the raw
//! [`toml::value::Table`]s for the resolver to ingest. It does
//! no layering and no precedence work.
//!
//! A missing manifest, a missing `[variables]` table, or a missing
//! `[profiles]` section each yield empty results rather than an error,
//! mirroring [`crate::profile::load_auto_match_rules`]'s `NotFound`
//! handling: a repository need not declare any root variables.
//!
//! The `[[auto_match]]` table-array in the same manifest is parsed
//! separately by [`crate::profile::load_auto_match_rules`]; this parser
//! ignores it.

use crate::variables::VariableError;
use crate::variables::reject_reserved_keys;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The repo-shared `[variables]` table and per-profile
/// `[profiles.<name>.variables]` tables parsed from the root
/// `patina.toml`.
///
/// Both fields are empty when the manifest declares no such tables (or
/// is absent). The raw [`toml::value::Table`] form is preserved
/// verbatim so the resolver can ingest these without a second
/// TOML pass, exactly as the per-module parser preserves its
/// `[variables]` table.
#[derive(Debug, Clone, Default)]
pub struct RootConfig {
    /// The root `[variables]` table — the repo-shared layer. Empty when
    /// the manifest declares no `[variables]` table.
    pub repo_shared: toml::value::Table,
    /// One entry per declared profile, keyed by profile name, each
    /// holding that profile's `[profiles.<name>.variables]` table. A
    /// profile with no `variables` table contributes an empty table.
    pub per_profile: BTreeMap<String, toml::value::Table>,
}

/// Failure modes returned by [`parse_root_config`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RootConfigError {
    /// IO failure reading the root manifest, other than `NotFound`
    /// (which yields an empty [`RootConfig`]).
    #[error("failed to read root patina.toml at {path}: {source}")]
    Io {
        /// The manifest path that failed to read.
        path: Utf8PathBuf,
        #[source]
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// TOML deserialization of the root manifest failed.
    #[error("failed to parse root patina.toml at {path} as TOML: {source}")]
    Toml {
        /// The manifest path whose TOML failed to parse.
        path: Utf8PathBuf,
        #[source]
        /// The underlying TOML deserialization error.
        source: Box<toml::de::Error>,
    },

    /// A root `[variables]` or `[profiles.<name>.variables]` table
    /// declared a key inside the reserved `patina.*` namespace.
    #[error(transparent)]
    Variable(#[from] VariableError),
}

/// Read and parse the root manifest at `path`, returning its repo-shared
/// and per-profile variable tables.
///
/// A missing manifest (`NotFound`) yields an empty [`RootConfig`], so a
/// repository that declares no root variables is not an error.
///
/// # Errors
///
/// Returns [`RootConfigError::Io`] on an IO failure other than
/// `NotFound`, [`RootConfigError::Toml`] on a malformed TOML document,
/// and [`RootConfigError::Variable`] when any variable table declares a
/// key inside the reserved `patina.*` namespace.
pub fn parse_root_config(path: &Utf8Path) -> Result<RootConfig, RootConfigError> {
    let text = match fs_err::read_to_string(path.as_std_path()) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RootConfig::default());
        }
        Err(source) => {
            return Err(RootConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse_root_config_str(&text).map_err(|err| match err {
        RootConfigError::Toml { source, .. } => RootConfigError::Toml {
            path: path.to_path_buf(),
            source,
        },
        other => other,
    })
}

/// Parse the root manifest's variable tables from an in-memory string.
///
/// Used by tests and callers that have already read the file. Unlike
/// [`parse_root_config`] there is no `NotFound` fall-through; an absent
/// `[variables]` table or `[profiles]` section simply yields empties.
///
/// # Errors
///
/// Returns [`RootConfigError::Toml`] on a malformed TOML document and
/// [`RootConfigError::Variable`] when any variable table declares a key
/// inside the reserved `patina.*` namespace.
pub fn parse_root_config_str(text: &str) -> Result<RootConfig, RootConfigError> {
    let raw: RawRoot = toml::from_str(text).map_err(|source| RootConfigError::Toml {
        path: Utf8PathBuf::from("<memory>"),
        source: Box::new(source),
    })?;

    let repo_shared = raw.variables.unwrap_or_default();
    reject_reserved_keys(repo_shared.keys().map(String::as_str))?;

    let mut per_profile = BTreeMap::new();
    for (name, profile) in raw.profiles {
        let table = profile.variables.unwrap_or_default();
        reject_reserved_keys(table.keys().map(String::as_str))?;
        per_profile.insert(name, table);
    }

    Ok(RootConfig {
        repo_shared,
        per_profile,
    })
}

/// Raw TOML projection of the root manifest's variable surface. Every
/// other root section (`[patina]`, `[[auto_match]]`, …) is ignored.
#[derive(Debug, Default, Deserialize)]
struct RawRoot {
    /// The repo-shared `[variables]` table.
    #[serde(default)]
    variables: Option<toml::value::Table>,

    /// The `[profiles.<name>]` sections, keyed by profile name. Only the
    /// nested `variables` table of each is read.
    #[serde(default)]
    profiles: BTreeMap<String, RawProfile>,
}

/// Raw projection of a single `[profiles.<name>]` section. Only its
/// `[profiles.<name>.variables]` table is consumed here.
#[derive(Debug, Default, Deserialize)]
struct RawProfile {
    #[serde(default)]
    variables: Option<toml::value::Table>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reads_root_repo_shared_variables() {
        let config = parse_root_config_str("[variables]\neditor = \"nvim\"\n")
            .expect("root variables parse");
        assert_eq!(
            config
                .repo_shared
                .get("editor")
                .and_then(toml::Value::as_str),
            Some("nvim"),
        );
        assert!(config.per_profile.is_empty());
    }

    #[test]
    fn reads_per_profile_variables_keyed_by_name() {
        let config = parse_root_config_str("[profiles.work.variables]\neditor = \"code\"\n")
            .expect("profile variables parse");
        let work = config
            .per_profile
            .get("work")
            .expect("work profile present");
        assert_eq!(
            work.get("editor").and_then(toml::Value::as_str),
            Some("code"),
        );
        // The repo-shared table is independent of the profile table.
        assert!(config.repo_shared.is_empty());
    }

    #[test]
    fn reads_both_repo_shared_and_per_profile_together() {
        let text = "[variables]\neditor = \"nvim\"\n\n\
             [profiles.work.variables]\neditor = \"code\"\n";
        let config = parse_root_config_str(text).expect("combined parse");
        assert_eq!(
            config
                .repo_shared
                .get("editor")
                .and_then(toml::Value::as_str),
            Some("nvim"),
        );
        assert_eq!(
            config
                .per_profile
                .get("work")
                .and_then(|t| t.get("editor"))
                .and_then(toml::Value::as_str),
            Some("code"),
        );
    }

    #[test]
    fn rejects_reserved_key_in_repo_shared_table() {
        let err = parse_root_config_str("[variables]\n\"patina.os\" = \"linux\"\n")
            .expect_err("reserved key in repo-shared must be rejected");
        assert!(matches!(
            err,
            RootConfigError::Variable(VariableError::ReservedKey { ref key }) if key == "patina.os"
        ));
    }

    #[test]
    fn rejects_reserved_key_in_per_profile_table() {
        let err = parse_root_config_str("[profiles.work.variables]\n\"patina.os\" = \"linux\"\n")
            .expect_err("reserved key in per-profile must be rejected");
        assert!(matches!(
            err,
            RootConfigError::Variable(VariableError::ReservedKey { ref key }) if key == "patina.os"
        ));
    }

    #[test]
    fn absent_sections_yield_empty_results() {
        let config = parse_root_config_str("[patina]\nname = \"dots\"\n")
            .expect("manifest with no variable tables parses");
        assert!(config.repo_shared.is_empty());
        assert!(config.per_profile.is_empty());
    }

    #[test]
    fn a_profile_without_a_variables_table_yields_an_empty_table() {
        // A `[profiles.work]` section that declares no nested
        // `variables` table still registers the profile name with an
        // empty table, rather than being absent.
        let config = parse_root_config_str("[profiles.work]\n").expect("bare profile parses");
        let work = config
            .per_profile
            .get("work")
            .expect("work profile registered");
        assert!(work.is_empty());
    }

    #[test]
    fn missing_manifest_file_yields_empty_results() {
        let dir = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(dir.path().join("patina.toml")).expect("utf8 tempdir path");
        // The file is never created; NotFound must fall through to an
        // empty config, not an IO error.
        let config = parse_root_config(&path).expect("missing manifest yields empty config");
        assert!(config.repo_shared.is_empty());
        assert!(config.per_profile.is_empty());
    }

    #[test]
    fn reads_variables_from_an_on_disk_manifest() {
        let dir = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(dir.path().join("patina.toml")).expect("utf8 tempdir path");
        fs_err::write(path.as_std_path(), "[variables]\neditor = \"nvim\"\n")
            .expect("write root manifest");
        let config = parse_root_config(&path).expect("on-disk manifest parses");
        assert_eq!(
            config
                .repo_shared
                .get("editor")
                .and_then(toml::Value::as_str),
            Some("nvim"),
        );
    }

    #[test]
    fn malformed_toml_is_a_typed_error_naming_the_path() {
        let dir = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(dir.path().join("patina.toml")).expect("utf8 tempdir path");
        fs_err::write(path.as_std_path(), "[variables\n").expect("write malformed manifest");
        let err = parse_root_config(&path).expect_err("malformed TOML must error");
        assert!(matches!(
            err,
            RootConfigError::Toml { path: ref errored, .. } if *errored == path
        ));
    }
}
