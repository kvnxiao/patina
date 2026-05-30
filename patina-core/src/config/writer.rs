//! Format-preserving `patina.toml` manifest writer (DEC-007).
//!
//! `patina-core::config` is parse-only on the read side
//! ([`parse_module_config`](super::parse_module_config) deserializes via
//! the `toml` crate). SPEC-0002's `init` / `add` / `remove` commands must
//! also *write* and *edit* manifests, and they must do so without
//! disturbing the user's hand-written comments, key ordering, or
//! whitespace — a one-entry delete may not rewrite sibling `[[file]]`
//! entries. DEC-007 selects `toml_edit` (format/comment-preserving) for
//! exactly that reason; this module is the write side.
//!
//! Every function here operates on manifest text (`String` in, `String`
//! out): the caller owns reading and writing the file via `fs-err`.
//!
//! # Examples
//!
//! ```
//! use patina_core::{append_file_entry, FileMode};
//!
//! let text = append_file_entry("", "zshrc", "~/.zshrc", FileMode::Symlink)?;
//! assert!(text.contains("source = \"zshrc\""));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use super::FileMode;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Table;
use toml_edit::value;

/// Failure modes returned by the manifest writer functions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigWriteError {
    /// The supplied manifest text was not a well-formed TOML document.
    #[error("failed to parse manifest text as TOML: {source}")]
    Parse {
        #[source]
        /// The underlying `toml_edit` parse error.
        source: Box<toml_edit::TomlError>,
    },

    /// [`remove_file_entry`] found no `[[file]]` entry whose `target`
    /// key equalled the requested target.
    #[error("no [[file]] entry found with target `{target}`")]
    EntryNotFound {
        /// The target that did not match any entry.
        target: String,
    },

    /// The manifest declared a `file` key that was not a `[[file]]`
    /// array of tables, so a new entry could not be appended to it.
    #[error("manifest `file` key is not a [[file]] array of tables")]
    MalformedFileArray,
}

/// Emit a root manifest declaring the repository-root marker.
///
/// The returned text is a `[patina]` table with `root = true` and a
/// `created_at` RFC 3339 string field set to `created_at` (REQ-001
/// done-when). `created_at` is the only timestamp permitted in a
/// user-facing artefact, because the manifest is configuration the user
/// keeps under version control.
///
/// # Arguments
///
/// * `created_at` - An RFC 3339 timestamp string (e.g.
///   `"2026-05-30T12:00:00Z"`), recorded verbatim.
///
/// # Examples
///
/// ```
/// use patina_core::scaffold_root_manifest;
///
/// let text = scaffold_root_manifest("2026-05-30T12:00:00Z");
/// assert!(text.contains("root = true"));
/// ```
#[must_use = "the scaffolded manifest text must be written to disk by the caller"]
pub fn scaffold_root_manifest(created_at: &str) -> String {
    let mut doc = DocumentMut::new();
    let mut patina = Table::new();
    patina.insert("root", value(true));
    patina.insert("created_at", value(created_at));
    doc.insert("patina", Item::Table(patina));
    doc.to_string()
}

/// Append one `[[file]]` array-of-tables element to a module manifest.
///
/// Parses `doc_text` as a [`DocumentMut`] (an empty string yields an
/// empty document), pushes a single `[[file]]` element carrying
/// `source`, `target`, and (for the four user-declarable modes) `mode`,
/// and returns the serialized text with every pre-existing table,
/// comment, key ordering, and whitespace intact.
///
/// [`FileMode::TemplateRender`] never emits a `mode` key: templating is
/// implied by a `.tmpl` source suffix and the parser rejects an explicit
/// template mode
/// (see [`FileEntryError::ImplicitTemplateModeDeclared`](super::FileEntryError::ImplicitTemplateModeDeclared)).
///
/// # Arguments
///
/// * `doc_text` - The existing manifest text (`""` for a fresh module).
/// * `source` - The `source` value for the new entry.
/// * `target` - The `target` value for the new entry.
/// * `mode` - The materialization mode; maps to the same spelling the parser
///   accepts, or is omitted entirely for `TemplateRender`.
///
/// # Errors
///
/// Returns [`ConfigWriteError::Parse`] when `doc_text` is not a
/// well-formed TOML document.
///
/// # Examples
///
/// ```
/// use patina_core::{append_file_entry, FileMode};
///
/// let text = append_file_entry("", "vimrc", "~/.vimrc", FileMode::Copy)?;
/// assert!(text.contains("mode = \"copy\""));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn append_file_entry(
    doc_text: &str,
    source: &str,
    target: &str,
    mode: FileMode,
) -> Result<String, ConfigWriteError> {
    let mut doc = parse_document(doc_text)?;

    let mut entry = Table::new();
    entry.insert("source", value(source));
    entry.insert("target", value(target));
    if let Some(mode_str) = mode_manifest_str(mode) {
        entry.insert("mode", value(mode_str));
    }

    let files = doc
        .entry("file")
        .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    if let Item::ArrayOfTables(array) = files {
        array.push(entry);
    } else {
        // An existing `file` key of the wrong shape is a malformed
        // manifest; surface it as a typed error rather than panicking.
        return Err(ConfigWriteError::MalformedFileArray);
    }

    Ok(doc.to_string())
}

/// Delete exactly the one `[[file]]` element whose `target` key equals
/// `target`, leaving every sibling `[[file]]`, every `[[hook]]`, the
/// `[variables]` table, comments, and formatting untouched.
///
/// # Arguments
///
/// * `doc_text` - The existing manifest text.
/// * `target` - The `target` value identifying the entry to remove.
///
/// # Errors
///
/// Returns [`ConfigWriteError::Parse`] when `doc_text` is malformed, and
/// [`ConfigWriteError::EntryNotFound`] when no `[[file]]` element carries
/// the requested `target`.
///
/// # Examples
///
/// ```
/// use patina_core::{append_file_entry, remove_file_entry, FileMode};
///
/// let text = append_file_entry("", "zshrc", "~/.zshrc", FileMode::Symlink)?;
/// let pruned = remove_file_entry(&text, "~/.zshrc")?;
/// assert!(!pruned.contains("~/.zshrc"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn remove_file_entry(doc_text: &str, target: &str) -> Result<String, ConfigWriteError> {
    let mut doc = parse_document(doc_text)?;

    let Some(Item::ArrayOfTables(array)) = doc.get_mut("file") else {
        return Err(ConfigWriteError::EntryNotFound {
            target: target.to_owned(),
        });
    };

    let index = array
        .iter()
        .position(|entry| entry.get("target").and_then(Item::as_str) == Some(target));

    match index {
        Some(index) => {
            array.remove(index);
            Ok(doc.to_string())
        }
        None => Err(ConfigWriteError::EntryNotFound {
            target: target.to_owned(),
        }),
    }
}

/// Parse manifest text into an editable document, treating empty text as
/// an empty document.
fn parse_document(doc_text: &str) -> Result<DocumentMut, ConfigWriteError> {
    doc_text
        .parse::<DocumentMut>()
        .map_err(|source| ConfigWriteError::Parse {
            source: Box::new(source),
        })
}

/// Map a [`FileMode`] to the manifest `mode` string the parser accepts,
/// or `None` for the implicit [`FileMode::TemplateRender`] which never
/// declares a `mode`. The spellings mirror those in
/// [`FileEntry::from_raw`](super::FileEntry).
fn mode_manifest_str(mode: FileMode) -> Option<&'static str> {
    match mode {
        FileMode::Symlink => Some("symlink"),
        FileMode::SymlinkDir => Some("symlink-dir"),
        FileMode::Copy => Some("copy"),
        FileMode::CopyTree => Some("copy-tree"),
        FileMode::TemplateRender => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_module_config_str;

    #[test]
    fn remove_file_entry_preserves_siblings_hook_variables_and_comments() {
        let text = "\
[[file]]
source = \"zshrc\"
target = \"~/.zshrc\"
mode = \"symlink\"

# hand-written comment
[[file]]
source = \"vimrc\"
target = \"~/.vimrc\"
mode = \"copy\"

[[hook]]
event = \"pre_apply\"
command = \"echo hi\"

[variables]
editor = \"vim\"
";

        let pruned = remove_file_entry(text, "~/.zshrc").expect("removal succeeds");

        // Re-parses cleanly via the read side, with the survivor intact.
        let config = parse_module_config_str(&pruned).expect("pruned text parses");
        assert_eq!(config.files.len(), 1, "only ~/.vimrc remains");
        let survivor = config.files.first().expect("one entry remains");
        assert_eq!(survivor.targets, vec!["~/.vimrc"]);
        assert_eq!(config.hooks.len(), 1, "the [[hook]] survives");
        assert!(config.variables.is_some(), "the [variables] table survives");

        // The hand-written comment and the survivor's formatting survive.
        assert!(
            pruned.contains("# hand-written comment"),
            "comment must be preserved verbatim"
        );
        assert!(
            !pruned.contains("~/.zshrc"),
            "no [[file]] entry for the removed target remains"
        );
    }

    #[test]
    fn append_file_entry_into_empty_text_round_trips_through_parser() {
        let text =
            append_file_entry("", "zshrc", "~/.zshrc", FileMode::Symlink).expect("append succeeds");

        let config = parse_module_config_str(&text).expect("appended text parses");
        assert_eq!(config.files.len(), 1);
        let entry = config.files.first().expect("one entry present");
        assert_eq!(entry.source, "zshrc");
        assert_eq!(entry.targets, vec!["~/.zshrc"]);
        assert_eq!(entry.mode, FileMode::Symlink);
    }

    #[test]
    fn scaffold_root_manifest_emits_root_marker_and_timestamp() {
        let text = scaffold_root_manifest("2026-05-30T12:00:00Z");

        let parsed: toml::Value = toml::from_str(&text).expect("scaffold parses as TOML");
        let patina = parsed
            .get("patina")
            .and_then(toml::Value::as_table)
            .expect("[patina] table present");
        assert_eq!(
            patina.get("root").and_then(toml::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            patina.get("created_at").and_then(toml::Value::as_str),
            Some("2026-05-30T12:00:00Z")
        );
    }

    #[test]
    fn append_each_mode_uses_the_parser_accepted_spelling() {
        for (mode, spelling) in [
            (FileMode::Symlink, "symlink"),
            (FileMode::SymlinkDir, "symlink-dir"),
            (FileMode::Copy, "copy"),
            (FileMode::CopyTree, "copy-tree"),
        ] {
            let text = append_file_entry("", "src", "~/.dst", mode).expect("append succeeds");
            assert!(
                text.contains(&format!("mode = \"{spelling}\"")),
                "mode {mode:?} should serialize as `{spelling}`, got:\n{text}"
            );
        }
    }

    #[test]
    fn append_template_render_omits_mode_key() {
        let text =
            append_file_entry("", "dot.tmpl", "~/.dot", FileMode::TemplateRender).expect("appends");
        assert!(
            !text.contains("mode ="),
            "TemplateRender must not emit a mode key, got:\n{text}"
        );
    }

    #[test]
    fn remove_file_entry_errors_when_no_entry_matches() {
        let text = append_file_entry("", "zshrc", "~/.zshrc", FileMode::Symlink).expect("appends");
        let err = remove_file_entry(&text, "~/.nope").expect_err("missing target errors");
        assert!(matches!(err, ConfigWriteError::EntryNotFound { .. }));
    }

    #[test]
    fn append_to_malformed_text_surfaces_a_parse_error() {
        let err = append_file_entry("not = = toml", "s", "t", FileMode::Symlink)
            .expect_err("malformed text errors");
        assert!(matches!(err, ConfigWriteError::Parse { .. }));
    }
}
