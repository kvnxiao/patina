//! Compact-UTC timestamp helper shared across the engine and CLI.
//!
//! Patina keys its journal `<ts>.plan` / `<ts>.COMMIT` files and backup
//! directories by a monotonic UTC timestamp formatted `YYYYMMDDTHHMMSSZ`.
//! Both the CLI `apply` path and the watcher's re-apply need the exact same
//! format string. The helper lives here as a single shared definition, not a
//! copy at each call site.
//!
//! The timestamp keys filenames only. It never appears in user-facing
//! output, so the deterministic-stdout guarantee holds.

/// A monotonic UTC timestamp keying a run's journal and backup files,
/// formatted `YYYYMMDDTHHMMSSZ`.
///
/// # Examples
///
/// ```
/// let ts = patina_core::clock::current_timestamp();
/// // YYYYMMDDTHHMMSSZ: 16 chars, a `T` separator at offset 8, ending in `Z`.
/// assert_eq!(ts.len(), 16);
/// assert_eq!(ts.as_bytes()[8], b'T');
/// assert!(ts.ends_with('Z'));
/// ```
#[must_use = "the timestamp keys journal and backup filenames; use it"]
pub fn current_timestamp() -> String {
    jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

/// The current time as Unix seconds.
///
/// The remote update gate compares a candidate commit's committer time against
/// "now" and against a lockfile timestamp, all in Unix seconds. Reading the
/// clock here, rather than at each comparison site, keeps every time read in
/// this one module. The gate's own logic therefore stays a pure function of
/// its inputs, and is unit-testable without touching the clock.
///
/// # Examples
///
/// ```
/// // Comfortably after 2020-01-01 and before 2100-01-01.
/// let now = patina_core::clock::current_epoch_seconds();
/// assert!((1_577_836_800..4_102_444_800).contains(&now));
/// ```
#[must_use = "the update gate compares committer times against the epoch"]
pub fn current_epoch_seconds() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// The current time as an RFC 3339 UTC timestamp, the form `patina.lock`
/// records in `updated_at`.
#[must_use = "the timestamp is written into the lockfile entry"]
pub fn current_rfc3339() -> String {
    crate::journal::timestamp_to_rfc3339(&current_timestamp())
}

/// Render Unix seconds as an RFC 3339 UTC instant, falling back to the raw
/// integer for a value outside the representable range.
///
/// The one spelling Patina renders an epoch in, so the cooldown message, the
/// watch drift report, and the lockfile timestamps can never disagree on
/// format.
///
/// # Examples
///
/// ```
/// // 2026-08-11T14:00:00Z, the epoch the remote-update tests pin.
/// assert_eq!(
///     patina_core::clock::epoch_to_rfc3339(1_786_456_800),
///     "2026-08-11T14:00:00Z"
/// );
/// ```
#[must_use = "the rendered instant is user-facing output"]
pub fn epoch_to_rfc3339(epoch: i64) -> String {
    jiff::Timestamp::from_second(epoch).map_or_else(
        |_out_of_range| epoch.to_string(),
        |ts| ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_compact_utc() {
        let ts = current_timestamp();
        // YYYYMMDDTHHMMSSZ is 16 chars; ends in Z, has the T separator.
        assert_eq!(ts.len(), 16, "timestamp {ts} should be 16 chars");
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.as_bytes().get(8), Some(&b'T'));
    }
}
