//! Compact-UTC timestamp helper shared across the engine and CLI.
//!
//! Patina keys its journal `<ts>.plan` / `<ts>.COMMIT` files and backup
//! directories by a monotonic UTC timestamp formatted `YYYYMMDDTHHMMSSZ`.
//! Every CLI command that plans or journals an apply needs the same format
//! string. The helper lives here as a single shared definition, not a
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

/// Return the timestamp one second after `timestamp`, or `None` when
/// `timestamp` is not in the form [`current_timestamp`] writes or has no
/// successor in that form.
pub(crate) fn timestamp_after(timestamp: &str) -> Option<String> {
    let parsed = jiff::civil::DateTime::strptime(COMPACT_FORMAT, timestamp).ok()?;
    let next = parsed.checked_add(jiff::Span::new().seconds(1)).ok()?;
    Some(next.strftime(COMPACT_FORMAT).to_string())
}

/// Return the current timestamp once it is later than `after`.
///
/// While the clock reads `after`, sleep until the next second, for at most
/// about a second. When the clock reads a time before `after`, return the
/// timestamp one second after `after` instead, which is ahead of the clock.
/// Return `None` when `after` has no successor.
pub(crate) fn timestamp_later_than(after: &str) -> Option<String> {
    const POLL: std::time::Duration = std::time::Duration::from_millis(20);
    const MAX_POLLS: u32 = 60;
    let mut now = current_timestamp();
    for _ in 0..MAX_POLLS {
        if now.as_str() != after {
            break;
        }
        std::thread::sleep(POLL);
        now = current_timestamp();
    }
    if now.as_str() > after {
        Some(now)
    } else {
        timestamp_after(after)
    }
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
    fn timestamp_after_carries_into_the_next_day() {
        assert_eq!(
            timestamp_after("20261231T235959Z").as_deref(),
            Some("20270101T000000Z")
        );
    }

    #[test]
    fn timestamp_after_has_no_successor_for_the_last_representable_second() {
        assert_eq!(timestamp_after("99991231T235959Z"), None);
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

    #[test]
    fn timestamp_later_than_the_current_second_waits_for_the_clock() {
        let now = current_timestamp();

        let later = timestamp_later_than(&now).expect("a later timestamp");

        assert!(later > now, "{later} must follow {now}");
        assert!(
            later <= current_timestamp(),
            "{later} must not be ahead of the clock"
        );
    }

    #[test]
    fn timestamp_later_than_a_future_timestamp_is_one_second_after_it() {
        assert_eq!(
            timestamp_later_than("29990101T000000Z").as_deref(),
            Some("29990101T000001Z")
        );
    }
}
