//! `patina.lock` is the committed statement of which remote commit every
//! machine applies.
//!
//! ```toml
//! version = 1
//!
//! [remotes.humanizer]
//! url = "https://github.com/blader/humanizer"
//! ref = "main"
//! rev = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"
//! updated_at = "2026-08-11T14:00:00Z"
//! ```
//!
//! The file lives beside the root `patina.toml` and is committed, so a remote
//! update flows to other machines as an ordinary repository change. `apply`
//! reads `rev` and nothing else, which is what keeps plan output identical
//! across machines and runs; `updated_at` exists solely for the update gate's
//! backdating check and is written only by `patina remote update`.
//!
//! Serialization is deterministic: entries in name order, a fixed field order,
//! and one canonical timestamp spelling. Re-writing an unchanged lockfile
//! therefore produces the same bytes, and never shows up as a spurious diff.
//!
//! See `docs/REMOTE_SOURCES.md` "The lockfile".

use super::RemoteError;
use super::RemoteRepr;
use crate::config::remote::RemoteName;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Filename of the lockfile, beside the root `patina.toml`.
pub const LOCKFILE_NAME: &str = "patina.lock";

/// The only lockfile layout this binary understands.
pub const LOCKFILE_VERSION: u32 = 1;

/// One remote's pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// The URL the rev was fetched from, recorded so a changed `[remote] url`
    /// is visible against the pin it produced.
    pub url: String,
    /// The tracked branch or tag, when the module names one.
    pub git_ref: Option<String>,
    /// The full commit SHA every machine materializes. The single field `apply`
    /// reads.
    pub rev: String,
    /// When the pin was last bumped, RFC 3339 in UTC. Read only by the update
    /// gate's backdating check.
    pub updated_at: String,
}

impl LockEntry {
    /// `updated_at` as Unix seconds, or `None` when it cannot be parsed.
    ///
    /// Parsing succeeds for every entry that came through [`Lockfile::parse`],
    /// which validates the field; the `Option` covers an entry constructed
    /// in-process.
    #[must_use = "the epoch is what the gate's backdating check compares against"]
    pub fn updated_at_epoch(&self) -> Option<i64> {
        self.updated_at
            .parse::<jiff::Timestamp>()
            .ok()
            .map(jiff::Timestamp::as_second)
    }
}

/// The parsed lockfile: one pin per declared remote, keyed by remote name.
///
/// [`RemoteName`] compares folded, so the map cannot hold one remote under two
/// spellings and every lookup answers to any spelling of the same name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockfile {
    remotes: BTreeMap<RemoteName, LockEntry>,
}

/// `<repo_root>/patina.lock`.
#[must_use = "the lockfile path is where pins are read from and written to"]
pub fn lockfile_path(repo_root: &Utf8Path) -> Utf8PathBuf {
    repo_root.join(LOCKFILE_NAME)
}

impl Lockfile {
    /// Read the lockfile at `path`.
    ///
    /// An absent file is an empty lockfile, not an error: a repository with no
    /// remote-backed modules never grows one, and the first `patina remote
    /// update` creates it.
    ///
    /// # Errors
    ///
    /// Returns a [`RemoteError`] when the file cannot be read, is not valid
    /// TOML, declares an unsupported `version`, or carries a malformed `rev` or
    /// `updated_at`.
    pub fn load(path: &Utf8Path) -> Result<Self, RemoteError> {
        let text = match fs_err::read_to_string(path.as_std_path()) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(RemoteRepr::LockfileIo {
                    path: path.to_path_buf(),
                    source,
                }
                .into());
            }
        };
        Self::parse(&text).map_err(|err| err.with_lockfile_path(path))
    }

    /// Parse a lockfile from an in-memory string.
    ///
    /// # Errors
    ///
    /// See [`Lockfile::load`].
    pub fn parse(text: &str) -> Result<Self, RemoteError> {
        let raw: RawLockfile = toml::from_str(text).map_err(|source| RemoteRepr::LockfileToml {
            path: Utf8PathBuf::from(LOCKFILE_NAME),
            source: Box::new(source),
        })?;
        if raw.version != LOCKFILE_VERSION {
            return Err(RemoteRepr::LockfileVersion {
                found: raw.version,
                supported: LOCKFILE_VERSION,
            }
            .into());
        }

        let mut remotes = BTreeMap::new();
        for (name, entry) in raw.remotes {
            if !super::git::is_full_sha(&entry.rev) {
                return Err(RemoteRepr::LockfileRev {
                    name,
                    value: entry.rev,
                }
                .into());
            }
            if entry.updated_at.parse::<jiff::Timestamp>().is_err() {
                return Err(RemoteRepr::LockfileTimestamp {
                    name,
                    value: entry.updated_at,
                }
                .into());
            }
            let key = RemoteName::parse(&name).map_err(|source| RemoteRepr::LockfileName {
                name: name.clone(),
                source,
            })?;
            let replaced = remotes.insert(
                key,
                LockEntry {
                    url: entry.url,
                    git_ref: entry.git_ref,
                    rev: entry.rev,
                    updated_at: entry.updated_at,
                },
            );
            // Patina's own writer replaces rather than joins, so two keys that
            // fold together mean a hand-edited file or a merge nobody finished
            // resolving. Guessing which pin wins would apply a different commit
            // than the machine that wrote the file.
            if replaced.is_some() {
                return Err(RemoteRepr::LockfileDuplicate { name }.into());
            }
        }
        Ok(Self { remotes })
    }

    /// Write the lockfile to `path`.
    ///
    /// Atomic, because this file is the only record of which commit each
    /// remote is pinned to: a direct write killed between the truncate and the
    /// last byte would leave neither the old pins nor the new ones, and every
    /// remote-backed entry would then fail to plan.
    ///
    /// # Errors
    ///
    /// Returns a [`RemoteError`] when the write fails.
    pub fn save(&self, path: &Utf8Path) -> Result<(), RemoteError> {
        crate::fsx::write_atomic(path, self.render().as_bytes()).map_err(|source| {
            RemoteRepr::LockfileIo {
                path: path.to_path_buf(),
                source,
            }
            .into()
        })
    }

    /// Render the lockfile as TOML.
    ///
    /// Entries come out in folded-name order with a fixed field order, so two
    /// renders of the same pins are byte-identical and a pin bump shows up as a
    /// one-entry diff. Each key keeps the spelling its author declared.
    #[must_use = "the rendered document is what gets committed"]
    pub fn render(&self) -> String {
        let mut out = format!("version = {LOCKFILE_VERSION}\n");
        for (name, entry) in &self.remotes {
            out.push_str("\n[remotes.");
            out.push_str(&toml_key(name.as_str()));
            out.push_str("]\n");
            push_field(&mut out, "url", &entry.url);
            if let Some(git_ref) = &entry.git_ref {
                push_field(&mut out, "ref", git_ref);
            }
            push_field(&mut out, "rev", &entry.rev);
            push_field(&mut out, "updated_at", &entry.updated_at);
        }
        out
    }

    /// The pin for `name`, if any.
    #[must_use = "the pin is the rev apply materializes"]
    pub fn get(&self, name: &RemoteName) -> Option<&LockEntry> {
        self.remotes.get(name)
    }

    /// Record (or replace) the pin for `name`.
    pub fn insert(&mut self, name: RemoteName, entry: LockEntry) {
        self.remotes.insert(name, entry);
    }

    /// Drop every pin `declared` does not name, returning the dropped names in
    /// order.
    ///
    /// The lockfile is a statement about the root manifest's declarations, so a
    /// pin whose declaration was deleted is stale by definition: it would keep
    /// its checkout alive in the cache and reappear in `patina remote` output
    /// forever. Nothing else depends on it: an entry naming a remote that is
    /// not declared fails at plan time, before this runs.
    pub fn retain_declared<'a>(
        &mut self,
        declared: impl IntoIterator<Item = &'a RemoteName>,
    ) -> Vec<RemoteName> {
        let declared: BTreeSet<&RemoteName> = declared.into_iter().collect();
        let stale: Vec<RemoteName> = self
            .remotes
            .keys()
            .filter(|name| !declared.contains(name))
            .cloned()
            .collect();
        for name in &stale {
            self.remotes.remove(name);
        }
        stale
    }

    /// Every pin, in folded-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&RemoteName, &LockEntry)> {
        self.remotes.iter()
    }

    /// Whether any pin is recorded.
    #[must_use = "an empty lockfile means no remote has ever been pinned"]
    pub fn is_empty(&self) -> bool {
        self.remotes.is_empty()
    }
}

/// Append one `key = "value"` line.
fn push_field(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(&toml_string(value));
    out.push('\n');
}

/// The `\uXXXX` escape for a control character TOML has no short escape for.
fn control_escape(ch: char) -> String {
    format!("\\u{:04X}", u32::from(ch))
}

/// Quote `value` as a TOML basic string.
///
/// Hand-written so the rendered bytes are a pure function of the input rather
/// than of a serializer's formatting choices, because the determinism contract
/// is on the exact bytes. The escape set is TOML's: backslash, quote, and the
/// control characters, which get their short escapes where TOML defines one and
/// a `\uXXXX` otherwise.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => out.push_str(&control_escape(c)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render `name` as a TOML table key: bare when it is a bare-key, quoted
/// otherwise. A remote name may contain `.`, which a bare key cannot.
fn toml_key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if bare {
        name.to_owned()
    } else {
        toml_string(name)
    }
}

/// Raw TOML projection of the lockfile.
#[derive(Debug, Deserialize)]
struct RawLockfile {
    /// Layout version. A value other than [`LOCKFILE_VERSION`] is refused
    /// rather than guessed at.
    version: u32,
    /// One table per declared remote.
    #[serde(default)]
    remotes: BTreeMap<String, RawLockEntry>,
}

/// Raw TOML projection of one `[remotes.<name>]` table.
#[derive(Debug, Deserialize)]
struct RawLockEntry {
    url: String,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    rev: String,
    updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rev: &str) -> LockEntry {
        LockEntry {
            url: "https://github.com/blader/humanizer".to_owned(),
            git_ref: Some("main".to_owned()),
            rev: rev.to_owned(),
            updated_at: "2026-08-11T14:00:00Z".to_owned(),
        }
    }

    const REV: &str = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";

    /// A validated name, for the fixtures that key pins by one.
    fn name(spelling: &str) -> RemoteName {
        RemoteName::parse(spelling).expect("a legal remote name")
    }

    #[test]
    fn a_rendered_lockfile_round_trips_through_parse() {
        let mut lock = Lockfile::default();
        lock.insert(name("humanizer"), entry(REV));
        let parsed = Lockfile::parse(&lock.render()).expect("the rendered document parses");
        assert_eq!(parsed, lock);
    }

    #[test]
    fn a_pin_answers_to_a_name_respelled_in_case() {
        // The registry compares declarations ignoring case, so a pin written
        // under one spelling must keep answering after a case-only respell of
        // its declaration, for reads, replacement, and the stale sweep alike.
        let mut lock = Lockfile::default();
        lock.insert(name("Humanizer"), entry(REV));

        assert!(
            lock.get(&name("humanizer")).is_some(),
            "a folded get must hit"
        );
        assert!(
            lock.retain_declared([&name("humanizer")]).is_empty(),
            "a case-respelled declaration must not read its own pin as stale"
        );

        lock.insert(name("humanizer"), entry(&"b".repeat(40)));
        assert_eq!(
            lock.iter().count(),
            1,
            "an insert under a respelling must replace, not join"
        );
        assert_eq!(
            lock.get(&name("Humanizer")).expect("still answers").rev,
            "b".repeat(40)
        );
    }

    #[test]
    fn rendering_is_byte_identical_across_calls_and_insertion_orders() {
        // The determinism contract: the committed bytes must be a function of
        // the pins, not of the order they happened to be inserted in.
        let mut first = Lockfile::default();
        first.insert(name("zsh"), entry(REV));
        first.insert(name("humanizer"), entry(REV));
        let mut second = Lockfile::default();
        second.insert(name("humanizer"), entry(REV));
        second.insert(name("zsh"), entry(REV));

        assert_eq!(first.render(), first.render(), "two renders must agree");
        assert_eq!(
            first.render(),
            second.render(),
            "insertion order must not reach the bytes"
        );
        assert!(
            first.render().find("[remotes.humanizer]") < first.render().find("[remotes.zsh]"),
            "entries must be emitted in remote-name order:\n{}",
            first.render()
        );
    }

    #[test]
    fn retaining_the_declared_set_drops_the_rest_and_names_them() {
        let mut lock = Lockfile::default();
        lock.insert(name("humanizer"), entry(REV));
        lock.insert(name("zsh"), entry(REV));
        assert_eq!(
            lock.retain_declared([&name("zsh")]),
            vec![name("humanizer")]
        );
        assert!(lock.get(&name("humanizer")).is_none());
        assert!(
            lock.get(&name("zsh")).is_some(),
            "a declared remote's pin must survive"
        );
    }

    #[test]
    fn retaining_a_superset_drops_nothing() {
        // A declaration with no pin yet is the ordinary state before the first
        // `patina remote update`; it must not make the sweep report anything.
        let mut lock = Lockfile::default();
        lock.insert(name("zsh"), entry(REV));
        assert!(
            lock.retain_declared([&name("zsh"), &name("humanizer")])
                .is_empty()
        );
        assert!(lock.get(&name("zsh")).is_some());
    }

    #[test]
    fn an_empty_lockfile_renders_only_its_version() {
        // Derived from the constant rather than re-typed, so this cannot pass by
        // someone editing both sites in step.
        assert_eq!(
            Lockfile::default().render(),
            format!("version = {LOCKFILE_VERSION}\n")
        );
    }

    #[test]
    fn a_remote_without_a_ref_omits_the_field_and_round_trips() {
        let mut lock = Lockfile::default();
        lock.insert(
            name("humanizer"),
            LockEntry {
                git_ref: None,
                ..entry(REV)
            },
        );
        let rendered = lock.render();
        assert!(
            !rendered.contains("ref ="),
            "an absent ref must be omitted, not written empty:\n{rendered}"
        );
        assert_eq!(Lockfile::parse(&rendered).expect("re-parse"), lock);
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let err = Lockfile::parse("version = 2\n").expect_err("a newer layout must be refused");
        assert!(
            err.to_string().contains("version 2"),
            "the message must name the version found, got: {err}"
        );
    }

    #[test]
    fn a_missing_version_is_refused() {
        Lockfile::parse("[remotes.humanizer]\nurl = \"u\"\nrev = \"x\"\nupdated_at = \"t\"\n")
            .expect_err("a lockfile with no version is not a version-1 lockfile");
    }

    #[test]
    fn a_short_rev_is_refused() {
        // `apply` materializes `rev` verbatim as a cache directory name, so an
        // abbreviated SHA would silently produce a different cache key than the
        // machine that wrote it.
        let err = Lockfile::parse(
            "version = 1\n\n[remotes.humanizer]\nurl = \"u\"\nrev = \"a1b2c3d\"\n\
             updated_at = \"2026-08-11T14:00:00Z\"\n",
        )
        .expect_err("an abbreviated rev must be refused");
        assert!(
            err.to_string().contains("humanizer"),
            "the message must name the offending remote, got: {err}"
        );
    }

    #[test]
    fn two_keys_that_fold_together_are_refused() {
        // Patina's own writer replaces a pin rather than joining a second one,
        // so this is a hand-edit or an unfinished merge. Picking one would
        // apply a different commit than the machine that wrote the file.
        let err = Lockfile::parse(&format!(
            "version = 1\n\n[remotes.humanizer]\nurl = \"u\"\nrev = \"{REV}\"\n\
             updated_at = \"2026-08-11T14:00:00Z\"\n\n\
             [remotes.Humanizer]\nurl = \"u\"\nrev = \"{}\"\n\
             updated_at = \"2026-08-11T14:00:00Z\"\n",
            "b".repeat(40)
        ))
        .expect_err("two entries for one remote must be refused");
        assert!(
            err.to_string().to_lowercase().contains("humanizer"),
            "the message must name the remote claimed twice, got: {err}"
        );
    }

    #[test]
    fn a_key_that_is_not_a_usable_remote_name_is_refused() {
        let err = Lockfile::parse(&format!(
            "version = 1\n\n[remotes.\"../escape\"]\nurl = \"u\"\nrev = \"{REV}\"\n\
             updated_at = \"2026-08-11T14:00:00Z\"\n"
        ))
        .expect_err("a key that could never match a declaration must be refused");
        assert!(
            err.to_string().contains("../escape"),
            "the message must name the offending key, got: {err}"
        );
    }

    #[test]
    fn a_malformed_updated_at_is_refused() {
        let err = Lockfile::parse(&format!(
            "version = 1\n\n[remotes.humanizer]\nurl = \"u\"\nrev = \"{REV}\"\n\
             updated_at = \"last tuesday\"\n"
        ))
        .expect_err("a non-RFC-3339 timestamp must be refused");
        assert!(err.to_string().contains("updated_at"));
    }

    #[test]
    fn updated_at_parses_to_the_epoch_the_gate_compares() {
        assert_eq!(
            entry(REV).updated_at_epoch(),
            Some(1_786_456_800),
            "2026-08-11T14:00:00Z is 1786456800 seconds after the epoch"
        );
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_lockfile() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let lock = Lockfile::load(&lockfile_path(root)).expect("an absent lockfile is empty");
        assert!(lock.is_empty());
    }

    #[test]
    fn save_then_load_preserves_every_field() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let path = lockfile_path(root);
        let mut lock = Lockfile::default();
        lock.insert(name("humanizer"), entry(REV));
        lock.save(&path).expect("write the lockfile");
        assert_eq!(Lockfile::load(&path).expect("read it back"), lock);
    }

    #[test]
    fn a_url_needing_escapes_survives_the_round_trip() {
        let mut lock = Lockfile::default();
        lock.insert(
            name("odd"),
            LockEntry {
                url: "ssh://host/a\\b\"c".to_owned(),
                ..entry(REV)
            },
        );
        let parsed = Lockfile::parse(&lock.render()).expect("escaped url re-parses");
        assert_eq!(parsed, lock);
    }

    #[test]
    fn a_remote_name_that_is_not_a_bare_key_is_quoted() {
        let mut lock = Lockfile::default();
        lock.insert(name("my.remote"), entry(REV));
        let rendered = lock.render();
        assert!(
            rendered.contains("[remotes.\"my.remote\"]"),
            "a dotted name must be quoted or it would nest tables:\n{rendered}"
        );
        assert_eq!(Lockfile::parse(&rendered).expect("re-parse"), lock);
    }
}
