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
//! [`ManagedEntry`]. Each validates that table's accepted-mode allowlist
//! before resolving to a [`FileMode`]. A source-kind enum can therefore never
//! pair with an illegal mode, which is the "illegal states unrepresentable"
//! bar. The parse-time rules surface as typed
//! [`FileEntryError`] variants whose `Display` impls satisfy the
//! substring contracts the tests assert.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;

/// Executor-facing materialization mode the apply engine dispatches on.
///
/// Represent the resolved operation taxonomy, distinct from the collapsed
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
    /// Optional `when` predicate as raw expression source. Parse and carry it;
    /// do not evaluate it through
    /// `MiniJinja` (mirrors [`HookEntry.when`](super::HookEntry::when)).
    pub when: Option<String>,
    /// The root-declared remote this entry's `source` is relative to, or
    /// `None` for a source relative to the module's own directory. The name is
    /// resolved against the registry at plan time; this module only carries it.
    pub remote: Option<String>,
    /// Gitignore-syntax patterns filtering this entry's source walk, in
    /// declaration order. Always empty except on the two tree modes; the
    /// constructors reject the key elsewhere. Compiled into a matcher by
    /// [`crate::ignore_rules::build`], which appends these after the
    /// repo-wide list so a pattern here wins a conflict.
    pub ignore: Vec<String>,
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

    /// A target path contains an ASCII control character.
    ///
    /// Patina prints one path per line, in tab-separated columns, across
    /// `status`, the apply diff, the Defender listing, and the `debug journal`
    /// dump. A tab would open a column that row never closes, and a newline
    /// would split one row into two, letting a crafted filename forge a row
    /// that reads as Patina's own. The whole control range goes together rather
    /// than drawing a line mid-range that each renderer would have to
    /// re-justify. The same rule covers sources, as
    /// [`SourceControlCharacter`](FileEntryError::SourceControlCharacter).
    #[error(
        "target `{}` contains the control character U+{:04X}; a target path must not contain an ASCII control character",
        .target.escape_debug(),
        .codepoint
    )]
    TargetControlCharacter {
        /// The target exactly as authored. Rendered through `escape_debug` in
        /// the message, so reporting the path cannot itself corrupt the report.
        target: String,
        /// The scalar value of the first offending character. A control
        /// character is invisible in a manifest, so the message has to name
        /// which one is at fault.
        codepoint: u32,
    },

    /// A source path contains an ASCII control character. The rationale is the
    /// one on [`TargetControlCharacter`](FileEntryError::TargetControlCharacter).
    /// A source is authored in the same manifest and rendered in the same
    /// line-oriented output, so the two are refused alike.
    #[error(
        "entry source `{}` contains the control character U+{:04X}; a source path must not contain an ASCII control character",
        .source_path.escape_debug(),
        .codepoint
    )]
    SourceControlCharacter {
        /// The source exactly as authored, `escape_debug`-rendered in the
        /// message for the same reason the target is.
        source_path: String,
        /// The scalar value of the first offending character.
        codepoint: u32,
    },

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

    /// A `[[file]]` entry declared `ignore`. Only the two tree modes walk a
    /// source tree, so there is nothing for a single file's list to filter.
    #[error(
        "[[file]] entry source `{source_path}` declares `ignore`; the key is accepted only on a \
         [[directory]] with `mode = \"symlink-tree\"` or `mode = \"copy\"`, the two modes that \
         enumerate a source tree one leaf at a time"
    )]
    FileIgnoreDeclared {
        /// The source path of the entry that declared it.
        source_path: String,
    },

    /// A whole-directory `symlink` `[[directory]]` entry declared `ignore`.
    ///
    /// Accepting it silently would be worse than refusing: the author would
    /// believe the listed paths are filtered while the single link keeps
    /// exposing the whole directory. The message names the mode that does
    /// filter, because reaching for `ignore` here means wanting per-leaf links.
    #[error(
        "[[directory]] entry source `{source_path}` declares `ignore` with the whole-directory \
         `symlink` mode; that mode creates one link and exposes the directory through it, so no \
         ignore list can change what appears at the target. Use `mode = \"symlink-tree\"` to \
         materialize one link per leaf and filter them"
    )]
    DirectorySymlinkIgnoreDeclared {
        /// The source path of the entry that declared it.
        source_path: String,
    },

    /// An entry declared `remote = ""`. Omitting the key is how an entry
    /// stays local; a blank one is a typo whose silent fallback would resolve
    /// the source against the wrong tree.
    #[error(
        "entry source `{source_path}` declares an empty `remote`; name a `[[remote]]` from the \
         root patina.toml, or drop the key to resolve the source inside this module"
    )]
    EmptyRemote {
        /// The source path of the entry that declared it.
        source_path: String,
    },
}

/// Whether a `.tmpl` suffix on an entry's source triggers the implicit
/// template render.
///
/// A local source takes the render. A source inside a remote
/// checkout never does: third-party bytes full of `{{ }}` would either explode
/// under strict-undefined rendering or, worse, render. Under
/// [`TemplatePolicy::Never`] a `.tmpl` suffix is just part of a filename: the
/// entry materializes as plain bytes under its declared mode, and a `.tmpl`
/// directory source is an ordinary directory name rather than an error. The
/// policy is per entry, so a module's own `.tmpl` still renders beside a
/// remote-sourced one that does not. See `docs/REMOTE_SOURCES.md`
/// "Trust boundaries".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplatePolicy {
    /// A `.tmpl` source renders, and declaring an explicit `mode` beside it is
    /// an error.
    Implicit,
    /// A `.tmpl` suffix carries no meaning.
    Never,
}

impl TemplatePolicy {
    /// The policy for an entry that does (or does not) select a remote.
    fn for_source(remote: Option<&str>) -> Self {
        if remote.is_some() {
            Self::Never
        } else {
            Self::Implicit
        }
    }
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
            remote,
            ignore,
        } = raw;

        reject_source_control_characters(&source)?;
        if !ignore.is_empty() {
            return Err(FileEntryError::FileIgnoreDeclared {
                source_path: source.to_string(),
            });
        }
        let remote = resolve_remote(remote, &source)?;
        let policy = TemplatePolicy::for_source(remote.as_deref());
        let resolved_targets = resolve_targets(target, targets)?;

        // The implicit-template rule is checked before the mode allowlist
        // so a `.tmpl` source plus `mode = "..."` surfaces
        // ImplicitTemplateModeDeclared rather than an UnsupportedFileMode
        // false-positive.
        let is_tmpl = policy == TemplatePolicy::Implicit && has_tmpl_suffix(&source);
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
            remote,
            ignore: Vec::new(),
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
            remote,
            ignore,
        } = raw;

        reject_source_control_characters(&source)?;
        let remote = resolve_remote(remote, &source)?;
        let policy = TemplatePolicy::for_source(remote.as_deref());
        let resolved_targets = resolve_targets(target, targets)?;

        // Template render is file-only: a `.tmpl` directory source is
        // rejected outright. Under `Never` the suffix means nothing at all, so
        // a remote directory that happens to be named `*.tmpl` is fine.
        if policy == TemplatePolicy::Implicit && has_tmpl_suffix(&source) {
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

        // Only the tree modes walk the source, so only they have leaves to
        // filter. Checked after the mode resolves so an unsupported mode plus
        // an `ignore` key reports the mode, the more basic error.
        if !ignore.is_empty()
            && !matches!(resolved_mode, FileMode::SymlinkTree | FileMode::CopyTree)
        {
            return Err(FileEntryError::DirectorySymlinkIgnoreDeclared {
                source_path: source.to_string(),
            });
        }

        Ok(Self {
            kind: EntryKind::Directory,
            mode: resolved_mode,
            source,
            targets: resolved_targets,
            when,
            remote,
            ignore,
        })
    }
}

/// Normalize a declared `remote`, refusing a blank one.
fn resolve_remote(
    remote: Option<String>,
    source: &Utf8Path,
) -> Result<Option<String>, FileEntryError> {
    let Some(name) = remote else {
        return Ok(None);
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(FileEntryError::EmptyRemote {
            source_path: source.to_string(),
        });
    }
    Ok(Some(name.to_owned()))
}

/// Apply the exactly-one-of `target` / `targets` rule, the
/// non-empty-`targets` rule, and the no-control-character rule, shared by both
/// tables.
fn resolve_targets(
    target: Option<Utf8PathBuf>,
    targets: Option<Vec<Utf8PathBuf>>,
) -> Result<Vec<Utf8PathBuf>, FileEntryError> {
    let resolved = match (target, targets) {
        (Some(_), Some(_)) => return Err(FileEntryError::TargetAndTargets),
        (None, None) => return Err(FileEntryError::TargetMissing),
        (Some(single), None) => vec![single],
        (None, Some(many)) if many.is_empty() => return Err(FileEntryError::TargetsEmpty),
        (None, Some(many)) => many,
    };
    for target in &resolved {
        if let Some(codepoint) = first_control_character(target) {
            return Err(FileEntryError::TargetControlCharacter {
                target: target.to_string(),
                codepoint,
            });
        }
    }
    Ok(resolved)
}

/// Refuse a source path carrying an ASCII control character.
///
/// Runs before every other source rule, so no later message quotes an
/// unprintable source back at the user.
fn reject_source_control_characters(source: &Utf8Path) -> Result<(), FileEntryError> {
    match first_control_character(source) {
        Some(codepoint) => Err(FileEntryError::SourceControlCharacter {
            source_path: source.to_string(),
            codepoint,
        }),
        None => Ok(()),
    }
}

/// The scalar value of the first ASCII control character in `path`, if it has
/// one.
///
/// Reads the path as authored, before tilde expansion, so a rejection quotes it
/// exactly as the manifest spells it.
fn first_control_character(path: &Utf8Path) -> Option<u32> {
    path.as_str()
        .chars()
        .find(char::is_ascii_control)
        .map(|c| c as u32)
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
    #[serde(default)]
    pub(super) remote: Option<String>,
    #[serde(default)]
    pub(super) ignore: Vec<String>,
}
