//! Compact-UTC timestamp helper shared across the engine and CLI.
//!
//! Patina keys its journal `<ts>.plan` / `<ts>.COMMIT` files and backup
//! directories by a monotonic UTC timestamp formatted `YYYYMMDDTHHMMSSZ`.
//! Both the CLI `apply` path and the watcher's re-apply need the
//! exact same format string, so the helper lives here — one definition the
//! whole workspace shares — rather than being duplicated per call site.
//!
//! The timestamp keys filenames only; it never appears in user-facing
//! output, so the deterministic-stdout guarantee is preserved.

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
