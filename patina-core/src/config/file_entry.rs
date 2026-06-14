//! Kind-typed `[[file]]` / `[[directory]]` table-array schema.
//!
//! Managed entries are declared under two kind-typed table-arrays. A
//! `[[file]]` describes a file source and accepts `mode = "symlink"` (the
//! default) or `mode = "copy"`, plus the implicit `.tmpl` template render.
//! A `[[directory]]` describes a directory source and accepts
//! `mode = "symlink"` (the default, an atomic whole-directory symlink),
//! `mode = "symlink-tree"` (one symbolic link per leaf file), or
//! `mode = "copy"` (a recursive directory copy). The collapsed mode names
//! mean "symlink/copy this thing" in both tables; the table supplies the
//! file/dir context, so the prior `symlink-dir` / `copy-tree` strings no
//! longer exist as accepted input.
//!
//! Both tables resolve to the same [`ManagedEntry`] carrying its
//! [`EntryKind`], its resolved executor [`FileMode`], a `source`, a
//! non-empty `targets` list, and an optional raw `when` expression. The
//! per-table `from_raw_*` constructors are the only way to build a
//! [`ManagedEntry`], and each validates that table's accepted-mode
//! allowlist before resolving to a [`FileMode`] — so a source-kind enum
//! can never pair with an illegal mode (the "illegal states
//! unrepresentable" bar). The parse-time rules surface as typed
//! [`FileEntryError`] variants whose `Display` impls satisfy the
//! substring contracts the tests assert.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;

/// Executor-facing materialization mode the apply engine dispatches on.
///
/// This is the resolved operation taxonomy, distinct from the collapsed
/// *user-facing* mode names (`symlink` / `symlink-tree` / `copy`) that
/// the table context disambiguates at parse time. The per-table
/// constructors on [`ManagedEntry`] map a collapsed user mode plus the
/// entry's [`EntryKind`] onto one of these variants, so an illegal
/// kind/mode pairing is never constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// Symbolic link to a single file (a `[[file]]` `symlink`, the
    /// default when `mode` is omitted).
    Symlink,
    /// Atomic symbolic link to a whole directory (a `[[directory]]`
    /// `symlink`, the default; the prior `symlink-dir` behavior).
    SymlinkDir,
    /// One symbolic link per leaf file of a directory source (a
    /// `[[directory]]` `symlink-tree`). The per-leaf executor lands
    /// later; this module only resolves the mode.
    SymlinkTree,
    /// Byte-for-byte copy of a single file (a `[[file]]` `copy`).
    Copy,
    /// Recursive byte-for-byte copy of a directory tree (a
    /// `[[directory]]` `copy`; the prior `copy-tree` behavior).
    CopyTree,
    /// MiniJinja-rendered output of a `.tmpl` source file (implicit;
    /// derived from the source's `.tmpl` suffix, file-only).
    TemplateRender,
}

/// Whether a managed entry was declared under `[[file]]` or
/// `[[directory]]`.
///
/// The kind is carried on the resolved [`ManagedEntry`] so the plan-time
/// source existence-and-kind check can validate the on-disk source
/// against the table it was declared under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Declared under a `[[file]]` table-array.
    File,
    /// Declared under a `[[directory]]` table-array.
    Directory,
}

/// A validated managed entry from either the `[[file]]` or
/// `[[directory]]` table-array.
///
/// Constructed only via the per-table `from_raw_file` /
/// `from_raw_directory` constructors, each of which enforces its table's
/// accepted-mode allowlist; the stored [`mode`](Self::mode) is therefore
/// always a [`FileMode`] legal for the [`kind`](Self::kind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedEntry {
    /// Which table-array declared this entry.
    pub kind: EntryKind,
    /// Resolved executor materialization mode.
    pub mode: FileMode,
    /// Source path relative to the module directory (e.g. `"zshrc"`).
    pub source: Utf8PathBuf,
    /// One or more target paths. Always non-empty after validation;
    /// single-target entries become a one-element vec internally so
    /// downstream consumers do not need to special-case the shape.
    pub targets: Vec<Utf8PathBuf>,
    /// Optional `when` predicate as raw expression source. Evaluation
    /// through `MiniJinja` lands later; this module only parses and
    /// carries it (mirrors [`HookEntry.when`](super::HookEntry::when)).
    pub when: Option<String>,
}

/// Backwards-compatible alias retained while downstream consumers still
/// refer to the pre-split `FileEntry` name. New code should use
/// [`ManagedEntry`].
pub type FileEntry = ManagedEntry;

/// Parse-time failures from the `[[file]]` / `[[directory]]`
/// table-array rules.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileEntryError {
    /// Both `target` and `targets` were declared on the same entry.
    /// Exactly one must be set.
    #[error("entry declares both `target` and `targets`; exactly one must be set")]
    TargetAndTargets,

    /// Neither `target` nor `targets` was declared. Exactly one must
    /// be set.
    #[error("entry is missing both `target` and `targets`; exactly one must be set")]
    TargetMissing,

    /// `targets = []` was declared; the array must be non-empty.
    #[error("entry declares `targets = []`; the array must be non-empty")]
    TargetsEmpty,

    /// A `[[file]]` `mode` was set to a value outside the accepted
    /// allowlist. The accepted `[[file]]` modes are listed so the
    /// substring contract holds.
    #[error(
        "[[file]] entry declares unsupported mode `{value}`; the accepted `[[file]]` modes are `symlink`, `copy`"
    )]
    UnsupportedFileMode {
        /// The offending mode string.
        value: String,
    },

    /// A `[[directory]]` `mode` was set to a value outside the accepted
    /// allowlist. The accepted `[[directory]]` modes are listed so the
    /// substring contract holds.
    #[error(
        "[[directory]] entry declares unsupported mode `{value}`; the accepted `[[directory]]` modes are `symlink`, `symlink-tree`, `copy`"
    )]
    UnsupportedDirectoryMode {
        /// The offending mode string.
        value: String,
    },

    /// A `[[file]]` `.tmpl` source declared an explicit `mode`. The
    /// implicit template render is never declared.
    #[error(
        "[[file]] entry source `{source_path}` has the `.tmpl` suffix and declares `mode = \"{mode}\"`; the implicit-template rule forbids declaring any `mode` on a `.tmpl` source"
    )]
    ImplicitTemplateModeDeclared {
        /// The `.tmpl` source path.
        source_path: String,
        /// The offending declared mode string.
        mode: String,
    },

    /// A `[[directory]]` source carried the `.tmpl` suffix. Template
    /// render is file-only.
    #[error(
        "[[directory]] entry source `{source_path}` has the `.tmpl` suffix; template render is file-only and not valid for a `[[directory]]`"
    )]
    DirectoryTemplateSource {
        /// The offending `.tmpl` directory source path.
        source_path: String,
    },
}

impl ManagedEntry {
    /// Build a `[[file]]`-kind [`ManagedEntry`] from a raw deserialized
    /// [`RawEntry`], applying the `[[file]]` parse-time rules.
    pub(super) fn from_raw_file(raw: RawEntry) -> Result<Self, FileEntryError> {
        let RawEntry {
            source,
            target,
            targets,
            mode,
            when,
        } = raw;

        let resolved_targets = resolve_targets(target, targets)?;

        // The implicit-template rule is checked before the mode allowlist
        // so a `.tmpl` source plus `mode = "..."` surfaces
        // ImplicitTemplateModeDeclared rather than an UnsupportedFileMode
        // false-positive.
        let is_tmpl = has_tmpl_suffix(&source);
        if is_tmpl && let Some(declared) = mode.as_deref() {
            return Err(FileEntryError::ImplicitTemplateModeDeclared {
                source_path: source.to_string(),
                mode: declared.to_string(),
            });
        }

        let resolved_mode = if is_tmpl {
            FileMode::TemplateRender
        } else {
            match mode.as_deref() {
                None | Some("symlink") => FileMode::Symlink,
                Some("copy") => FileMode::Copy,
                Some(other) => {
                    return Err(FileEntryError::UnsupportedFileMode {
                        value: other.to_string(),
                    });
                }
            }
        };

        Ok(Self {
            kind: EntryKind::File,
            mode: resolved_mode,
            source,
            targets: resolved_targets,
            when,
        })
    }

    /// Build a `[[directory]]`-kind [`ManagedEntry`] from a raw
    /// deserialized [`RawEntry`], applying the `[[directory]]`
    /// parse-time rules.
    pub(super) fn from_raw_directory(raw: RawEntry) -> Result<Self, FileEntryError> {
        let RawEntry {
            source,
            target,
            targets,
            mode,
            when,
        } = raw;

        let resolved_targets = resolve_targets(target, targets)?;

        // Template render is file-only: a `.tmpl` directory source is
        // rejected outright.
        if has_tmpl_suffix(&source) {
            return Err(FileEntryError::DirectoryTemplateSource {
                source_path: source.to_string(),
            });
        }

        let resolved_mode = match mode.as_deref() {
            None | Some("symlink") => FileMode::SymlinkDir,
            Some("symlink-tree") => FileMode::SymlinkTree,
            Some("copy") => FileMode::CopyTree,
            Some(other) => {
                return Err(FileEntryError::UnsupportedDirectoryMode {
                    value: other.to_string(),
                });
            }
        };

        Ok(Self {
            kind: EntryKind::Directory,
            mode: resolved_mode,
            source,
            targets: resolved_targets,
            when,
        })
    }
}

/// Apply the exactly-one-of `target` / `targets` rule and the
/// non-empty-`targets` rule, shared by both tables.
fn resolve_targets(
    target: Option<Utf8PathBuf>,
    targets: Option<Vec<Utf8PathBuf>>,
) -> Result<Vec<Utf8PathBuf>, FileEntryError> {
    match (target, targets) {
        (Some(_), Some(_)) => Err(FileEntryError::TargetAndTargets),
        (None, None) => Err(FileEntryError::TargetMissing),
        (Some(single), None) => Ok(vec![single]),
        (None, Some(many)) => {
            if many.is_empty() {
                Err(FileEntryError::TargetsEmpty)
            } else {
                Ok(many)
            }
        }
    }
}

/// Whether `source`'s filename ends in a `.tmpl` suffix (case-insensitive).
fn has_tmpl_suffix(source: &Utf8Path) -> bool {
    source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tmpl"))
}

/// Raw TOML projection of a `[[file]]` or `[[directory]]` entry.
/// `target` / `targets` are captured separately so the XOR rule can be
/// enforced post-parse; the per-table `from_raw_*` constructors resolve
/// `mode` against that table's allowlist.
#[derive(Debug, Deserialize)]
pub(super) struct RawEntry {
    pub(super) source: Utf8PathBuf,
    #[serde(default)]
    pub(super) target: Option<Utf8PathBuf>,
    #[serde(default)]
    pub(super) targets: Option<Vec<Utf8PathBuf>>,
    #[serde(default)]
    pub(super) mode: Option<String>,
    #[serde(default)]
    pub(super) when: Option<String>,
}
