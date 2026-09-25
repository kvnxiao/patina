//! Read wall-clock time for record metadata and remote update checks.

/// Return the current UTC time formatted `YYYYMMDDTHHMMSSZ`.
///
/// # Examples
///
/// ```
/// let ts = patina_core::clock::current_timestamp();
/// assert_eq!(ts.len(), 16);
/// assert_eq!(ts.as_bytes()[8], b'T');
/// assert!(ts.ends_with('Z'));
/// ```
pub fn current_timestamp() -> String {
    jiff::Timestamp::now().strftime(COMPACT_FORMAT).to_string()
}

const COMPACT_FORMAT: &str = "%Y%m%dT%H%M%SZ";

/// Whether `text` is a timestamp exactly as [`current_timestamp`] writes it.
/// Timestamps in that form compare chronologically as strings.
pub(crate) fn is_timestamp(text: &str) -> bool {
    jiff::civil::DateTime::strptime(COMPACT_FORMAT, text)
        .is_ok_and(|parsed| parsed.strftime(COMPACT_FORMAT).to_string() == text)
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
pub fn current_epoch_seconds() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// The current time as an RFC 3339 UTC timestamp, the form `patina.lock`
/// records in `updated_at`.
pub fn current_rfc3339() -> String {
    crate::journal::timestamp_to_rfc3339(&current_timestamp())
}

/// Render Unix seconds as an RFC 3339 UTC instant, falling back to the raw
/// integer for a value outside the representable range.
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

    #[test]
    fn is_timestamp_rejects_a_name_that_only_starts_like_one() {
        assert!(is_timestamp("20260528T120000Z"));
        assert!(!is_timestamp("20260528T120000"));
        assert!(!is_timestamp(""));
        assert!(!is_timestamp("rollback-stage-20260528T120000Z-0"));
    }

    #[test]
    fn is_timestamp_rejects_an_unpadded_spelling() {
        assert!(!is_timestamp("2026528T120000Z"));
        assert!(!is_timestamp("20260528T12000Z"));
    }
}
