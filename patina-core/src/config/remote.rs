//! The `[remote]` module table and the `[remotes]` root table.
//!
//! A module carrying a `[remote]` table is remote-backed: every entry source
//! in it resolves against a cached checkout of another repository instead of
//! the module directory. The table itself is small — a URL, an optional ref,
//! and an optional per-remote update-gate floor — because everything else
//! about the module (entries, hooks, variables) stays exactly as it is for a
//! local module. See `docs/REMOTE_SOURCES.md` "Remote-backed modules".
//!
//! The root manifest's `[remotes]` table carries the global default for that
//! same gate floor. Both durations are written in the `s` / `m` / `h` / `d`
//! shorthand [`parse_duration`] accepts.

use serde::Deserialize;
use std::time::Duration;

/// The shipped update-gate floor when neither the module's `[remote]` table
/// nor the root `[remotes]` table names one.
pub const DEFAULT_MIN_AGE: Duration = Duration::from_hours(72);

/// A module's validated `[remote]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    /// The git URL to fetch from, passed to `git` verbatim so existing SSH
    /// agents and credential helpers apply untouched.
    pub url: String,
    /// The branch or tag whose tip `patina remote update` proposes. `None`
    /// means the remote's default branch.
    pub git_ref: Option<String>,
    /// Per-remote override of the update gate's age floor. `None` defers to
    /// the root `[remotes] min_age`, then to [`DEFAULT_MIN_AGE`].
    pub min_age: Option<Duration>,
}

/// Parse-time failures from the `[remote]` / `[remotes]` tables.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteConfigError {
    /// A `[remote]` table declared an empty `url`.
    #[error("[remote] declares an empty `url`; a remote-backed module needs a git URL to fetch")]
    EmptyUrl,

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
    /// Validate a raw `[remote]` table.
    fn from_raw(raw: RawRemote) -> Result<Self, RemoteConfigError> {
        let url = raw.url.trim().to_owned();
        if url.is_empty() {
            return Err(RemoteConfigError::EmptyUrl);
        }
        let min_age = raw
            .min_age
            .as_deref()
            .map(|value| parse_duration("min_age", value))
            .transpose()?;
        Ok(Self {
            url,
            git_ref: raw.git_ref.filter(|r| !r.trim().is_empty()),
            min_age,
        })
    }
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
    // parse failure is a count too large for `u64` — an invalid duration too.
    let count: u64 = digits.parse().map_err(|_overflow| invalid())?;
    count
        .checked_mul(per_unit)
        .map(Duration::from_secs)
        .ok_or_else(invalid)
}

/// Raw TOML projection of a module's `[remote]` table.
#[derive(Debug, Deserialize)]
pub(super) struct RawRemote {
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

/// Raw TOML projection of the root manifest's `[remotes]` table.
#[derive(Debug, Default, Deserialize)]
pub(super) struct RawRemotes {
    /// Global update-gate floor, in the [`parse_duration`] shorthand.
    #[serde(default)]
    pub(super) min_age: Option<String>,
}

impl RawRemotes {
    /// Validate the global floor, if one is declared.
    pub(super) fn validate(self) -> Result<Option<Duration>, RemoteConfigError> {
        self.min_age
            .as_deref()
            .map(|value| parse_duration("min_age", value))
            .transpose()
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

    #[test]
    fn a_remote_table_needs_a_non_empty_url() {
        let raw = RawRemote {
            url: "   ".to_owned(),
            git_ref: None,
            min_age: None,
        };
        assert!(matches!(
            raw.validate().expect_err("blank url"),
            RemoteConfigError::EmptyUrl
        ));
    }

    #[test]
    fn a_blank_ref_reads_as_the_default_branch() {
        // An author who writes `ref = ""` means "no opinion", not "a branch
        // whose name is the empty string" — the latter would be handed to git
        // verbatim and fail obscurely later.
        let raw = RawRemote {
            url: "https://example.invalid/r".to_owned(),
            git_ref: Some("  ".to_owned()),
            min_age: None,
        };
        let spec = raw.validate().expect("valid remote");
        assert_eq!(spec.git_ref, None);
    }
}
