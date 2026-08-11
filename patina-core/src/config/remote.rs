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

/// One validated `[[remote]]` declaration from the root manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    /// What entries select this remote by, and the key its pin, its cache
    /// directory, and every `patina remote` verb are addressed under. Written
    /// explicitly or derived from the URL by [`derive_name`].
    pub name: String,
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

    /// A module manifest carries a `[remote]` table, which is where remotes
    /// were declared before the root registry existed.
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
        let name = match raw.name.map(|name| name.trim().to_owned()) {
            Some(written) => {
                if !is_legal_name(&written) {
                    return Err(RemoteConfigError::IllegalName { name: written });
                }
                written
            }
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
pub fn derive_name(url: &str) -> Result<String, RemoteConfigError> {
    let trimmed = url.trim().trim_end_matches(['/', '\\']);
    // `:` separates host from path in the scp-like form, which has no `//`.
    let segment = trimmed.rsplit(['/', '\\', ':']).next().unwrap_or(trimmed);
    let name = segment.strip_suffix(".git").unwrap_or(segment);
    if !is_legal_name(name) {
        return Err(RemoteConfigError::UnderivableName {
            url: url.trim().to_owned(),
        });
    }
    Ok(name.to_owned())
}

/// Whether `name` may address a remote.
///
/// The name is not merely a label: it becomes a directory name under the
/// per-machine cache and a table key in the committed lockfile. Restricting it
/// to a portable filename alphabet — and refusing the two directory names that
/// traverse — keeps a manifest from steering a checkout outside the cache.
fn is_legal_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
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
        for input in [
            // no unit
            "72",    // unknown unit
            "72w",   // compound
            "1h30m", // fractional
            "1.5h",  // signed
            "-1h",   // unit only
            "h",     // empty
            "",      // internal space
            "72 h",  // uppercase unit is a distinct (unaccepted) spelling
            "72H",
        ] {
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
                derive_name(url).expect("a name is derivable"),
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
        assert_eq!(spec.name, "agents");
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
