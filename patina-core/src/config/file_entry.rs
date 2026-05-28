//! `[[file]]` table-array schema (REQ-005).
//!
//! Each `[[file]]` entry resolves to a [`FileEntry`] carrying a source
//! path, one-or-more target paths, and a [`FileMode`]. The parse-time
//! rules — exactly-one-of `target`/`targets`, the accepted-mode
//! allowlist, and the implicit-template `.tmpl` rule — are enforced in
//! [`FileEntry::from_raw`] and surface as typed
//! [`FileEntryError`] variants whose `Display` impls satisfy the
//! substring contracts in the SPEC's CHKs and task scenarios.

use camino::Utf8PathBuf;
use serde::Deserialize;

/// File-materialization mode for a `[[file]]` entry.
///
/// The four user-declarable variants — [`Symlink`](Self::Symlink),
/// [`SymlinkDir`](Self::SymlinkDir), [`Copy`](Self::Copy), and
/// [`CopyTree`](Self::CopyTree) — match the strings accepted in the
/// `mode = "…"` field. The fifth variant,
/// [`TemplateRender`](Self::TemplateRender), is implicit: the parser
/// sets it when the source filename ends with `.tmpl` and rejects any
/// `mode` declaration alongside that suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// Symbolic link to a single file (the default when `mode` is
    /// omitted).
    Symlink,
    /// Symbolic link to a directory.
    SymlinkDir,
    /// Byte-for-byte copy of a single file.
    Copy,
    /// Recursive byte-for-byte copy of a directory tree.
    CopyTree,
    /// MiniJinja-rendered output of a `.tmpl` source file (implicit;
    /// derived from the source's `.tmpl` suffix).
    TemplateRender,
}

/// A validated `[[file]]` table-array entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Source path relative to the module directory (e.g. `"zshrc"`).
    pub source: Utf8PathBuf,
    /// One or more target paths. Always non-empty after validation;
    /// single-target entries become a one-element vec internally so
    /// downstream consumers do not need to special-case the shape.
    pub targets: Vec<Utf8PathBuf>,
    /// Resolved materialization mode.
    pub mode: FileMode,
}

/// Parse-time failures from REQ-005's `[[file]]` table-array rules.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileEntryError {
    /// Both `target` and `targets` were declared on the same entry.
    /// REQ-005 mandates exactly one.
    #[error("[[file]] entry declares both `target` and `targets`; exactly one must be set")]
    TargetAndTargets,

    /// Neither `target` nor `targets` was declared. REQ-005 requires
    /// exactly one.
    #[error("[[file]] entry is missing both `target` and `targets`; exactly one must be set")]
    TargetMissing,

    /// `targets = []` was declared; the array must be non-empty.
    #[error("[[file]] entry declares `targets = []`; the array must be non-empty")]
    TargetsEmpty,

    /// `mode` was set to a value outside the accepted allowlist.
    /// The four accepted values are listed in the message so the
    /// CHK-012 substring contract holds.
    #[error(
        "[[file]] entry declares unsupported mode `{value}`; the accepted values are `symlink`, `symlink-dir`, `copy`, `copy-tree`"
    )]
    UnsupportedMode {
        /// The offending mode string.
        value: String,
    },

    /// A `.tmpl` source declared an explicit `mode`. The fifth mode
    /// (`TemplateRender`) is implicit and may never be declared.
    #[error(
        "[[file]] entry source `{source_path}` has the `.tmpl` suffix and declares `mode = \"{mode}\"`; the implicit-template rule forbids declaring any `mode` on a `.tmpl` source"
    )]
    ImplicitTemplateModeDeclared {
        /// The `.tmpl` source path.
        source_path: String,
        /// The offending declared mode string.
        mode: String,
    },
}

impl FileEntry {
    /// Build a [`FileEntry`] from a raw deserialized [`RawFileEntry`],
    /// applying REQ-005's three parse-time rules.
    pub(super) fn from_raw(raw: RawFileEntry) -> Result<Self, FileEntryError> {
        let RawFileEntry {
            source,
            target,
            targets,
            mode,
        } = raw;

        // Rule 1: exactly one of `target` / `targets`.
        let resolved_targets: Vec<Utf8PathBuf> = match (target, targets) {
            (Some(_), Some(_)) => return Err(FileEntryError::TargetAndTargets),
            (None, None) => return Err(FileEntryError::TargetMissing),
            (Some(single), None) => vec![single],
            (None, Some(many)) => {
                if many.is_empty() {
                    return Err(FileEntryError::TargetsEmpty);
                }
                many
            }
        };

        // Rule 3 (checked before rule 2 so the .tmpl-with-mode case
        // surfaces ImplicitTemplateModeDeclared rather than an
        // UnsupportedMode false-positive when the user wrote
        // `mode = "template"`): a `.tmpl` source plus any declared
        // `mode` is rejected.
        let is_tmpl = source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tmpl"));
        if is_tmpl && let Some(declared) = mode.as_deref() {
            return Err(FileEntryError::ImplicitTemplateModeDeclared {
                source_path: source.to_string(),
                mode: declared.to_string(),
            });
        }

        // Rule 2: mode allowlist (or default to Symlink when omitted,
        // or TemplateRender when the source is `.tmpl`).
        let resolved_mode = if is_tmpl {
            FileMode::TemplateRender
        } else {
            match mode.as_deref() {
                None | Some("symlink") => FileMode::Symlink,
                Some("symlink-dir") => FileMode::SymlinkDir,
                Some("copy") => FileMode::Copy,
                Some("copy-tree") => FileMode::CopyTree,
                Some(other) => {
                    return Err(FileEntryError::UnsupportedMode {
                        value: other.to_string(),
                    });
                }
            }
        };

        Ok(Self {
            source,
            targets: resolved_targets,
            mode: resolved_mode,
        })
    }
}

/// Raw TOML projection of a `[[file]]` entry. `target` / `targets` are
/// captured separately so the XOR rule can be enforced post-parse.
#[derive(Debug, Deserialize)]
pub(super) struct RawFileEntry {
    pub(super) source: Utf8PathBuf,
    #[serde(default)]
    pub(super) target: Option<Utf8PathBuf>,
    #[serde(default)]
    pub(super) targets: Option<Vec<Utf8PathBuf>>,
    #[serde(default)]
    pub(super) mode: Option<String>,
}
