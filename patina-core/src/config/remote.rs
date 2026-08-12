//! The root manifest's `[[remote]]` registry.
//!
//! Every third-party git source a repository consumes is declared once, in the
//! root `patina.toml`, as a `[[remote]]` table: a URL, an optional ref, an
//! optional per-remote update-gate floor, and a name other manifests refer to
//! it by. A managed entry in any module selects one with `remote = "<name>"`;
//! an entry with no `remote` key resolves against its own module directory.
//! See `docs/REMOTE_SOURCES.md` "The remote registry".
//!
//! The root `[patina] remote_min_age` key carries the repository-wide default
//! for that same gate floor. Both durations are written in the `s` / `m` / `h`
//! / `d` shorthand [`parse_duration`] accepts.

use serde::Deserialize;
use std::time::Duration;

/// The shipped update-gate floor when neither the remote's own `min_age` nor
/// the root `[patina] remote_min_age` names one.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_hours(72);

/// A remote's name: the spelling its author wrote, and the folded key that
/// decides identity.
///
/// The name is not a label. It becomes a directory under the per-machine
/// cache, a table key in the committed lockfile, the `remote = "..."` an entry
/// selects it by, and the argument every `patina remote` verb takes. Two
/// spellings a case-insensitive filesystem cannot keep apart are therefore one
/// remote, and this type is where that rule lives: equality, ordering, and
/// hashing all run over [`RemoteName::key`], so a map keyed by this type
/// cannot hold one remote twice. [`Display`](std::fmt::Display) renders the
/// authored spelling, which is what messages and the lockfile keep.
#[derive(Debug, Clone)]
pub struct RemoteName {
    /// As authored, for messages and the lockfile table key.
    display: String,
    /// The folded identity, for comparison and for on-disk paths.
    key: String,
}

impl RemoteName {
    /// Validate `name` as a remote name.
    ///
    /// Surrounding whitespace is trimmed first, so an author's stray padding
    /// never reaches a path or a lockfile key.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteConfigError::IllegalName`] when the name is empty, is
    /// `.` or `..`, or carries a character outside the portable filename
    /// alphabet; [`RemoteConfigError::NonPortableName`] when it is a legal
    /// filename on Unix but not on Windows; and
    /// [`RemoteConfigError::ReservedName`] when it collides with one of
    /// Patina's own files in the cache directory.
    pub fn parse(name: &str) -> Result<Self, RemoteConfigError> {
        let display = name.trim().to_owned();
        let illegal = || RemoteConfigError::IllegalName {
            name: display.clone(),
        };
        if display.is_empty() || display == "." || display == ".." {
            return Err(illegal());
        }
        if !display
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(illegal());
        }
        if let Some(reason) = non_portable_reason(&display) {
            return Err(RemoteConfigError::NonPortableName {
                name: display,
                reason,
            });
        }
        let key = name_key(&display);
        if RESERVED_NAMES.contains(&key.as_str()) {
            return Err(RemoteConfigError::ReservedName { name: display });
        }
        Ok(Self { display, key })
    }

    /// The authored spelling.
    #[must_use = "the display spelling is what messages and the lockfile key carry"]
    pub fn as_str(&self) -> &str {
        &self.display
    }

    /// The folded identity key: what names a cache directory and what two
    /// spellings are compared under.
    #[must_use = "the key is the identity, not the display spelling"]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether `spelling` addresses this remote.
    ///
    /// For the raw strings that arrive from outside a validated manifest — an
    /// entry's `remote = "..."`, a `patina remote update <name>` argument.
    #[must_use = "the answer decides which declaration a raw spelling selects"]
    pub fn matches(&self, spelling: &str) -> bool {
        self.key == name_key(spelling)
    }
}

impl std::fmt::Display for RemoteName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display)
    }
}

impl PartialEq for RemoteName {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for RemoteName {}

impl PartialOrd for RemoteName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RemoteName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

impl std::hash::Hash for RemoteName {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

/// One validated `[[remote]]` declaration from the root manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    /// What entries select this remote by, and the key its pin, its cache
    /// directory, and every `patina remote` verb are addressed under. Written
    /// explicitly or derived from the URL by [`derive_name`].
    pub name: RemoteName,
    /// The git URL to fetch from, passed to `git` verbatim so existing SSH
    /// agents and credential helpers apply untouched.
    pub url: String,
    /// The branch or tag whose tip `patina remote update` proposes. `None`
    /// means the remote's default branch.
    pub git_ref: Option<String>,
    /// Per-remote override of the update gate's age floor. `None` defers to
    /// the root `[patina] remote_min_age`, then to [`DEFAULT_MIN_AGE`].
    pub min_age: Option<Duration>,
}

/// Parse-time failures from the `[[remote]]` registry and the
/// `[patina] remote_min_age` key.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteConfigError {
    /// A `[[remote]]` table declared an empty `url`.
    #[error("[[remote]] declares an empty `url`; a remote needs a git URL to fetch")]
    EmptyUrl,

    /// No legal name could be taken from a `[[remote]]`'s URL and the table
    /// declared none.
    #[error(
        "cannot derive a name from the remote url `{url}`; add `name = \"...\"` to its \
         [[remote]] table"
    )]
    UnderivableName {
        /// The URL nothing legal could be taken from.
        url: String,
    },

    /// A `name` (written or derived) is not usable as a cache directory and a
    /// lockfile key.
    #[error(
        "the remote name `{name}` is not usable; a name may contain only letters, digits, \
         `.`, `_`, and `-`, and may not be `.` or `..`"
    )]
    IllegalName {
        /// The offending name.
        name: String,
    },

    /// A `name` (written or derived) is a legal filename on Unix but not on
    /// Windows.
    #[error(
        "the remote name `{name}` is not a portable directory name: {reason}. A manifest must \
         mean the same thing on macOS, Linux, and Windows, so such a name is refused on every \
         platform rather than only where it breaks"
    )]
    NonPortableName {
        /// The offending name.
        name: String,
        /// What Windows does with it.
        reason: &'static str,
    },

    /// A `name` (written or derived) collides with one of Patina's own files
    /// in the cache directory.
    #[error(
        "the remote name `{name}` is reserved; `notice`, `pending`, and `last_check` name \
         Patina's own files beside the per-remote cache directories"
    )]
    ReservedName {
        /// The offending name.
        name: String,
    },

    /// Two `[[remote]]` tables claim one name.
    #[error(
        "two [[remote]] tables declare the name `{name}`; a remote name addresses one pin, \
         one cache directory, and one `patina remote` target, so it must be unique (names \
         are compared ignoring case and Unicode normalization)"
    )]
    DuplicateName {
        /// The name declared twice.
        name: String,
    },

    /// A module manifest carries a `[remote]` table; remotes are declared only
    /// in the root manifest.
    #[error(
        "a module manifest declares a `[remote]` table; declare the remote once in the root \
         patina.toml as a `[[remote]]` table, then point each entry at it with \
         `remote = \"<name>\"`"
    )]
    ModuleRemoteTable,

    /// A `url` or `ref` began with `-`, which git would parse as an option
    /// rather than a positional argument.
    #[error(
        "[[remote]] `{key}` may not begin with `-` (`{value}`); git would read a leading dash \
         as an option, so a manifest could smuggle flags like `--upload-pack` into a fetch"
    )]
    LeadingDash {
        /// The key that carried the offending value (`url` or `ref`).
        key: &'static str,
        /// The offending value as written.
        value: String,
    },

    /// A duration string did not match the accepted shorthand.
    #[error(
        "invalid duration `{value}` for `{key}`; write a whole number followed by one of \
         `s`, `m`, `h`, `d` (for example `0s`, `30m`, `72h`, `7d`)"
    )]
    InvalidDuration {
        /// The key the duration was declared under (`min_age`).
        key: &'static str,
        /// The offending value as written.
        value: String,
    },
}

impl RemoteSpec {
    /// Validate a raw `[[remote]]` table.
    fn from_raw(raw: RawRemote) -> Result<Self, RemoteConfigError> {
        let url = raw.url.trim().to_owned();
        if url.is_empty() {
            return Err(RemoteConfigError::EmptyUrl);
        }
        reject_leading_dash("url", &url)?;
        let git_ref = raw
            .git_ref
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        if let Some(git_ref) = &git_ref {
            reject_leading_dash("ref", git_ref)?;
        }
        let min_age = raw
            .min_age
            .as_deref()
            .map(|value| parse_duration("min_age", value))
            .transpose()?;
        let name = match raw.name {
            Some(written) => RemoteName::parse(&written)?,
            None => derive_name(&url)?,
        };
        Ok(Self {
            name,
            url,
            git_ref,
            min_age,
        })
    }
}

/// Take a remote's name from its URL: the last path segment, without a
/// trailing `.git`.
///
/// One rule covers every spelling a git URL comes in, because they all end in
/// the segment a user would name the remote after:
/// `https://github.com/blader/humanizer.git`, the scp-like
/// `git@github.com:blader/humanizer`, a trailing slash on either, and a local
/// path. A URL whose last segment is not a legal name — a bare host, a query
/// string — has nothing obvious to take, so it is refused and the author writes
/// `name` instead of getting a surprising one.
///
/// # Errors
///
/// Returns [`RemoteConfigError::UnderivableName`] when no legal name remains.
/// A segment that is shaped like a name but is refused for what it would
/// collide with keeps its own error, so the author is told the real reason
/// rather than being sent to write an explicit `name` that would fail too.
pub fn derive_name(url: &str) -> Result<RemoteName, RemoteConfigError> {
    let trimmed = url.trim().trim_end_matches(['/', '\\']);
    // `:` separates host from path in the scp-like form, which has no `//`.
    let segment = trimmed.rsplit(['/', '\\', ':']).next().unwrap_or(trimmed);
    let name = segment.strip_suffix(".git").unwrap_or(segment);
    RemoteName::parse(name).map_err(|err| match err {
        RemoteConfigError::IllegalName { .. } => RemoteConfigError::UnderivableName {
            url: url.trim().to_owned(),
        },
        other => other,
    })
}

/// The names of Patina's own files beside the per-remote cache directories
/// under `<state>/remotes/`, in folded form. A remote so named would fight its
/// own metadata over one path.
const RESERVED_NAMES: [&str; 3] = ["notice", "pending", "last_check"];

/// The identity key of a remote-name spelling.
///
/// [`RemoteName`] carries this for every validated name; the bare function is
/// for the spellings that never went through validation — a directory name the
/// cache sweep read off disk, a raw selector from a manifest or a command line.
#[must_use = "the key is the identity every name comparison uses"]
pub fn name_key(name: &str) -> String {
    crate::caseless::fold(name)
}

/// Why Windows would not treat `name` as an ordinary directory name, or `None`
/// when it would.
///
/// The name becomes a directory under the per-machine cache, so a name Windows
/// resolves elsewhere is not a naming-style preference: `notice.` would land on
/// Patina's own notice file, and `CON` is a device rather than a path at all.
fn non_portable_reason(name: &str) -> Option<&'static str> {
    if name.ends_with('.') {
        return Some("Windows strips a trailing dot, so this would name a different directory");
    }
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    let folded = stem.to_ascii_lowercase();
    let device = matches!(folded.as_str(), "con" | "prn" | "aux" | "nul")
        || matches!(
            folded.strip_suffix(|c: char| c.is_ascii_digit()),
            Some("com" | "lpt")
        );
    device.then_some(
        "Windows reserves CON, PRN, AUX, NUL, COM0-9, and LPT0-9 as device names, with or \
         without an extension",
    )
}

/// Reject a `url` or `ref` that begins with `-`.
///
/// Every git call passes these as bare positionals, so a value like
/// `--upload-pack=...` would be parsed as an option and could run an arbitrary
/// program. A real URL or ref never starts with a dash, so refusing one closes
/// the injection without a false positive.
fn reject_leading_dash(key: &'static str, value: &str) -> Result<(), RemoteConfigError> {
    if value.starts_with('-') {
        return Err(RemoteConfigError::LeadingDash {
            key,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Parse a `<whole number><unit>` duration, where the unit is one of `s`, `m`,
/// `h`, or `d`.
///
/// Hand-rolled rather than delegated to a dependency: the accepted grammar is
/// four suffixes over an integer, and it is the whole surface Patina exposes.
/// Compound forms (`1h30m`), fractions (`1.5h`), signs, and a bare number with
/// no unit are all rejected, so a typo surfaces as an error instead of a
/// silently different window.
///
/// # Errors
///
/// Returns [`RemoteConfigError::InvalidDuration`] for anything outside that
/// grammar, including a value whose seconds count overflows [`u64`].
pub fn parse_duration(key: &'static str, value: &str) -> Result<Duration, RemoteConfigError> {
    let invalid = || RemoteConfigError::InvalidDuration {
        key,
        value: value.to_owned(),
    };
    let trimmed = value.trim();
    let (digits, unit) = trimmed
        .split_at_checked(trimmed.len().saturating_sub(1))
        .ok_or_else(invalid)?;
    let per_unit: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(invalid()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    // Digits-only and non-empty is already established, so the only remaining
    // parse failure is a count too large for `u64`, which is also invalid.
    let count: u64 = digits.parse().map_err(|_overflow| invalid())?;
    count
        .checked_mul(per_unit)
        .map(Duration::from_secs)
        .ok_or_else(invalid)
}

/// Raw TOML projection of one root `[[remote]]` table.
#[derive(Debug, Deserialize)]
pub(super) struct RawRemote {
    /// What entries select this remote by. Omitted means
    /// [`derive_name`] takes one from the URL.
    #[serde(default)]
    pub(super) name: Option<String>,
    /// The git URL to fetch from.
    pub(super) url: String,
    /// The tracked branch or tag. `ref` is a Rust keyword, so the field is
    /// renamed rather than raw-identified.
    #[serde(default, rename = "ref")]
    pub(super) git_ref: Option<String>,
    /// Per-remote update-gate floor, in the [`parse_duration`] shorthand.
    #[serde(default)]
    pub(super) min_age: Option<String>,
}

impl RawRemote {
    /// Validate this table into a [`RemoteSpec`].
    pub(super) fn validate(self) -> Result<RemoteSpec, RemoteConfigError> {
        RemoteSpec::from_raw(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_unit_suffix_scales_correctly() {
        for (input, expected_secs) in [
            ("0s", 0),
            ("45s", 45),
            ("30m", 30 * 60),
            ("72h", 72 * 60 * 60),
            ("7d", 7 * 24 * 60 * 60),
        ] {
            let parsed = parse_duration("min_age", input).expect("valid duration");
            assert_eq!(
                parsed.as_secs(),
                expected_secs,
                "`{input}` must parse to {expected_secs}s"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(
            parse_duration("min_age", "  72h ")
                .expect("padded duration")
                .as_secs(),
            72 * 60 * 60
        );
    }

    #[test]
    fn malformed_durations_are_rejected() {
        for input in ["72", "72w", "1h30m", "1.5h", "-1h", "h", "", "72 h", "72H"] {
            let err =
                parse_duration("min_age", input).expect_err(&format!("`{input}` must be rejected"));
            assert!(
                matches!(
                    err,
                    RemoteConfigError::InvalidDuration { key: "min_age", .. }
                ),
                "`{input}` must fail as an invalid duration, got {err:?}"
            );
        }
    }

    #[test]
    fn an_overflowing_duration_is_rejected_rather_than_wrapping() {
        let err = parse_duration("min_age", &format!("{}d", u64::MAX))
            .expect_err("an overflowing day count must be rejected");
        assert!(matches!(err, RemoteConfigError::InvalidDuration { .. }));
    }

    /// A raw table over the given url and ref, with no name and no `min_age`.
    fn raw(url: &str, git_ref: Option<&str>) -> RawRemote {
        RawRemote {
            name: None,
            url: url.to_owned(),
            git_ref: git_ref.map(str::to_owned),
            min_age: None,
        }
    }

    #[test]
    fn a_remote_table_needs_a_non_empty_url() {
        assert!(matches!(
            raw("   ", None).validate().expect_err("blank url"),
            RemoteConfigError::EmptyUrl
        ));
    }

    #[test]
    fn a_url_beginning_with_a_dash_is_rejected() {
        assert!(matches!(
            raw("--upload-pack=touch /tmp/pwn", Some("main"))
                .validate()
                .expect_err("a dash-led url must be rejected"),
            RemoteConfigError::LeadingDash { key: "url", .. }
        ));
    }

    #[test]
    fn a_ref_beginning_with_a_dash_is_rejected() {
        assert!(matches!(
            raw("https://example.invalid/r", Some("--output=/etc/cron.d/x"))
                .validate()
                .expect_err("a dash-led ref must be rejected"),
            RemoteConfigError::LeadingDash { key: "ref", .. }
        ));
    }

    #[test]
    fn a_name_colliding_with_a_cache_metadata_file_is_rejected() {
        // `<state>/remotes/` holds the `notice`, `pending`, and `last_check`
        // files beside the per-remote directories, so a remote by any of those
        // names (in any case) would fight Patina's own metadata over one path.
        for name in ["notice", "pending", "last_check", "Notice", "PENDING"] {
            let err = RawRemote {
                name: Some(name.to_owned()),
                url: "https://example.invalid/r".to_owned(),
                git_ref: None,
                min_age: None,
            }
            .validate()
            .expect_err("a reserved name must be rejected");
            assert!(
                matches!(err, RemoteConfigError::ReservedName { .. }),
                "`{name}` must be reserved, got: {err}"
            );
        }
    }

    #[test]
    fn a_reserved_name_derived_from_a_url_is_rejected() {
        raw("https://example.invalid/owner/notice", None)
            .validate()
            .expect_err("a derived reserved name must be rejected the same way");
    }

    #[test]
    fn a_blank_ref_reads_as_the_default_branch() {
        // An author who writes `ref = ""` means "no opinion", not "a branch
        // whose name is the empty string": the latter would be handed to git
        // verbatim and fail obscurely later.
        let spec = raw("https://example.invalid/r", Some("  "))
            .validate()
            .expect("valid remote");
        assert_eq!(spec.git_ref, None);
    }

    #[test]
    fn a_ref_is_stored_trimmed() {
        // The stored value reaches git as a bare argv element and is committed
        // to patina.lock, so padding is not merely cosmetic.
        let spec = raw("https://example.invalid/r", Some("  main\n"))
            .validate()
            .expect("valid remote");
        assert_eq!(spec.git_ref.as_deref(), Some("main"));
    }

    #[test]
    fn a_name_is_taken_from_the_last_segment_of_every_url_spelling() {
        for url in [
            "https://github.com/blader/humanizer",
            "https://github.com/blader/humanizer.git",
            "https://github.com/blader/humanizer/",
            "git@github.com:blader/humanizer",
            "git@github.com:blader/humanizer.git",
            "git@github.com:humanizer.git",
            "ssh://git@github.com:22/blader/humanizer.git",
            "/srv/mirrors/humanizer.git",
            "C:\\mirrors\\humanizer",
        ] {
            assert_eq!(
                derive_name(url).expect("a name is derivable").as_str(),
                "humanizer",
                "`{url}` must name the remote `humanizer`"
            );
        }
    }

    #[test]
    fn a_url_with_no_usable_last_segment_asks_for_an_explicit_name() {
        for url in [
            "https://example.invalid/repo?ref=main",
            "https://example.invalid/.git",
            "https://example.invalid/../",
            "https://example.invalid/\u{540d}\u{524d}",
        ] {
            let err = derive_name(url).expect_err("no legal name is takeable");
            assert!(
                matches!(err, RemoteConfigError::UnderivableName { .. }),
                "`{url}` must ask for an explicit name, got {err:?}"
            );
            assert!(
                err.to_string().contains("name = "),
                "the message must say which key to write, got: {err}"
            );
        }
    }

    #[test]
    fn a_written_name_overrides_the_derived_one() {
        let spec = RawRemote {
            name: Some("  agents  ".to_owned()),
            ..raw("https://github.com/blader/humanizer.git", None)
        }
        .validate()
        .expect("valid remote");
        assert_eq!(spec.name.as_str(), "agents");
    }

    #[test]
    fn identity_is_the_folded_key_while_display_keeps_the_authored_spelling() {
        // The whole point of the type: a map keyed by it cannot hold one remote
        // twice, yet a message still shows the name as its author wrote it.
        let written = RemoteName::parse("Humanizer").expect("a legal name");
        let respelled = RemoteName::parse("humanizer").expect("a legal name");

        assert_eq!(written, respelled, "a case-only respelling is one remote");
        assert_eq!(written.key(), respelled.key());
        assert_eq!(written.as_str(), "Humanizer", "the spelling is preserved");
        assert_eq!(written.to_string(), "Humanizer");
        assert!(written.matches("HUMANIZER"), "a raw selector folds too");

        let set: std::collections::BTreeSet<RemoteName> =
            [written, respelled].into_iter().collect();
        assert_eq!(set.len(), 1, "two spellings must occupy one slot");
    }

    #[test]
    fn both_normal_forms_of_one_name_are_refused_together() {
        // `café` precomposed against `e` plus a combining acute. Both are
        // outside the legal alphabet; what must not happen is one being
        // accepted and the other not, which would make a manifest's legality
        // depend on the editor that saved it.
        RemoteName::parse("caf\u{e9}").expect_err("a precomposed non-ASCII name is refused");
        RemoteName::parse("cafe\u{301}").expect_err("its decomposed spelling is refused too");
    }

    #[test]
    fn a_dos_device_name_is_refused_on_every_platform() {
        // These are devices rather than paths on Windows, with or without an
        // extension, so a manifest carrying one would apply on Linux and fail
        // on Windows.
        for name in [
            "con", "CON", "PRN", "aux", "NUL", "com1", "LPT9", "con.git", "nul.d",
        ] {
            let err = RemoteName::parse(name).expect_err("a device name must be refused");
            assert!(
                matches!(err, RemoteConfigError::NonPortableName { .. }),
                "`{name}` must be refused as non-portable, got {err:?}"
            );
        }
    }

    #[test]
    fn a_name_windows_would_resolve_elsewhere_is_refused() {
        // Windows strips a trailing dot, so `notice.` would land on Patina's
        // own notice file and `humanizer.` on a directory of another name.
        for name in ["notice.", "humanizer.", "a.."] {
            let err = RemoteName::parse(name).expect_err("a trailing dot must be refused");
            assert!(
                matches!(err, RemoteConfigError::NonPortableName { .. }),
                "`{name}` must be refused as non-portable, got {err:?}"
            );
        }
    }

    #[test]
    fn names_that_merely_contain_a_device_word_stay_legal() {
        // The rule is about the stem Windows resolves, not about the substring:
        // refusing these would reject ordinary repository names.
        for name in ["console", "com", "lpt", "my-con", "conf.d", "aux-tools"] {
            let parsed = RemoteName::parse(name);
            assert!(
                parsed.is_ok(),
                "`{name}` must stay legal, got: {:?}",
                parsed.err()
            );
        }
    }

    #[test]
    fn a_written_name_that_could_escape_the_cache_is_rejected() {
        // The name becomes a directory under `<state>/remotes/`, so a separator
        // or a traversal component would steer a checkout out of the cache.
        for name in ["..", ".", "", "a/b", "a\\b", "a b", "sk!lls"] {
            let err = RawRemote {
                name: Some(name.to_owned()),
                ..raw("https://example.invalid/r", None)
            }
            .validate()
            .expect_err("an unusable name must be rejected");
            assert!(
                matches!(err, RemoteConfigError::IllegalName { .. }),
                "`{name}` must be refused as a name, got {err:?}"
            );
        }
    }
}
