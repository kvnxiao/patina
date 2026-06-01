//! The watcher's drift-notification ledger at `<state>/patina/drift.cache`
//! (REQ-007).
//!
//! When the watcher detects that a managed non-symlink target's bytes
//! diverge from the journal-recorded `blake3` hash, it records the
//! divergence here. The cache backs the per-target notification rate limit
//! (DEC-004), the `patina debug drift-cache` decode surface, and the
//! watcher's own metrics. It is deliberately **never** read by
//! `patina status`: status derives DRIFTED from SPEC-0001 REQ-018's own
//! live re-hash, so a file edited and then reverted reports CLEAN even
//! while this cache still holds the intervening edit.
//!
//! ## On-disk format
//!
//! The cache is laid out exactly like the journal's binary files — a
//! fixed-size [`version_envelope`] prefix followed by the
//! `postcard`-encoded body — but versions independently of the journal:
//! it carries its own [`DRIFT_CACHE_MAJOR_VERSION`], so a journal format
//! bump never forces a drift-cache bump (or vice versa). A binary refuses
//! any cache whose major exceeds its own, naming both versions.
//!
//! ```text
//! offset 0   offset 2
//! ┌────────┬─────────────────────────────────────┐
//! │ u16 LE │ postcard-encoded DriftCache body ... │
//! │ major  │                                      │
//! └────────┴─────────────────────────────────────┘
//! ```
//!
//! # Examples
//!
//! ```no_run
//! use camino::Utf8Path;
//! use patina_core::watch::drift_cache::{DriftCache, load_drift_cache_file, write_drift_cache};
//!
//! let cache = DriftCache::new("20260528T120000Z", Vec::new());
//! let path = Utf8Path::new("/var/state/patina/drift.cache");
//! write_drift_cache(path, &cache)?;
//! let loaded = load_drift_cache_file(path)?;
//! assert_eq!(loaded, cache);
//! # Ok::<(), patina_core::watch::drift_cache::DriftCacheError>(())
//! ```

use crate::journal::timestamp_to_rfc3339;
use crate::version_envelope;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

/// Current on-disk drift-cache format major version. This is the drift
/// cache's *own* version, intentionally separate from the journal's
/// [`FILE_MAJOR_VERSION`](crate::journal::FILE_MAJOR_VERSION): the two
/// formats version independently, so a journal-layout bump must never force
/// a drift-cache bump. Bump this only when the serialized [`DriftCache`]
/// layout changes incompatibly; older binaries then refuse the newer file
/// via the version envelope.
pub const DRIFT_CACHE_MAJOR_VERSION: u16 = 1;

/// One recorded drift event: a managed target whose live bytes diverged
/// from the journal-recorded expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DriftEntry {
    /// Canonical absolute path of the drifted target.
    pub target: Utf8PathBuf,
    /// 32-byte `blake3` hash the journal recorded for this target — the
    /// bytes Patina materialized. Directly comparable to the journal's
    /// recorded hash (REQ-029).
    pub expected_hash: [u8; 32],
    /// 32-byte `blake3` hash of the target's bytes when drift was detected.
    pub actual_hash: [u8; 32],
    /// Unix timestamp (seconds) when the watcher detected the divergence.
    /// Internal: it backs the per-target rate limit (DEC-004) and the
    /// human-rendered detection time in `patina debug drift-cache`, and is
    /// not otherwise surfaced to users.
    pub detected_at_unix: i64,
}

impl DriftEntry {
    /// Construct a drift entry from its target, the recorded and observed
    /// hashes, and the detection timestamp.
    #[must_use = "the entry must be placed in a DriftCache to be persisted"]
    pub fn new(
        target: impl Into<Utf8PathBuf>,
        expected_hash: [u8; 32],
        actual_hash: [u8; 32],
        detected_at_unix: i64,
    ) -> Self {
        Self {
            target: target.into(),
            expected_hash,
            actual_hash,
            detected_at_unix,
        }
    }
}

/// The full drift-cache record: the journal timestamp this cache is bound
/// to plus one [`DriftEntry`] per detected divergence. The version envelope
/// is not a field — it is the fixed-size on-disk prefix
/// [`encode`](DriftCache::encode) prepends and [`decode`](DriftCache::decode)
/// strips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftCache {
    /// The journal `<ts>` whose committed apply this cache's expectations
    /// are measured against. When a new `patina apply` commits, the watcher
    /// rebinds the cache to the new timestamp.
    pub journal_ts: String,
    /// The recorded drift events, in detection order.
    pub entries: Vec<DriftEntry>,
}

impl DriftCache {
    /// Build a drift cache bound to `journal_ts` carrying `entries`.
    #[must_use = "the cache must be written with write_drift_cache to be persisted"]
    pub fn new(journal_ts: impl Into<String>, entries: Vec<DriftEntry>) -> Self {
        Self {
            journal_ts: journal_ts.into(),
            entries,
        }
    }

    /// Encode to the on-disk byte form: the little-endian `u16`
    /// [`DRIFT_CACHE_MAJOR_VERSION`] envelope followed by the
    /// `postcard`-encoded body.
    ///
    /// # Errors
    ///
    /// Returns [`DriftCacheError::Encode`] if `postcard` serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>, DriftCacheError> {
        let body = postcard::to_stdvec(self).map_err(DriftCacheError::Encode)?;
        Ok(version_envelope::encode_with_envelope(
            DRIFT_CACHE_MAJOR_VERSION,
            &body,
        ))
    }

    /// Decode a cache from its bytes, refusing any cache whose major version
    /// exceeds [`DRIFT_CACHE_MAJOR_VERSION`] (this binary's own drift-cache
    /// major, never the journal's).
    ///
    /// # Errors
    ///
    /// - [`DriftCacheError::Truncated`] if the envelope is missing.
    /// - [`DriftCacheError::VersionMismatch`] if the cache is from a newer
    ///   drift-cache format than this binary supports.
    /// - [`DriftCacheError::Decode`] if the body fails to deserialize.
    pub fn decode(bytes: &[u8]) -> Result<Self, DriftCacheError> {
        let body = version_envelope::decode_envelope(bytes, DRIFT_CACHE_MAJOR_VERSION)?;
        postcard::from_bytes(body).map_err(DriftCacheError::Decode)
    }
}

/// Errors raised while reading, writing, or decoding the drift cache.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DriftCacheError {
    /// The drift-cache file could not be read, written, or renamed. The
    /// wrapped `fs-err` error carries the offending path.
    #[error("drift-cache filesystem operation failed")]
    Filesystem(#[from] std::io::Error),

    /// The cache body could not be `postcard`-encoded.
    #[error("failed to encode drift cache to postcard: {0}")]
    Encode(postcard::Error),

    /// The cache body could not be `postcard`-decoded.
    #[error("failed to decode drift cache from postcard: {0}")]
    Decode(postcard::Error),

    /// The cache file was shorter than the fixed-size version envelope, so
    /// no major version could be read.
    #[error(
        "drift-cache file is truncated: {got} bytes, need at least {need} for the version envelope"
    )]
    Truncated {
        /// Bytes actually present in the file.
        got: usize,
        /// Bytes required to read the version envelope.
        need: usize,
    },

    /// The cache file declares a drift-cache major version newer than this
    /// binary understands. Refusing it is intentional: a forward-compatible
    /// decode would silently misread the cache.
    #[error(
        "drift-cache major version {found} is newer than supported version {supported}; \
         upgrade patina to read this drift cache"
    )]
    VersionMismatch {
        /// Major version read from the cache file's envelope.
        found: u16,
        /// Highest drift-cache major version this binary can decode.
        supported: u16,
    },
}

impl From<crate::version_envelope::EnvelopeError> for DriftCacheError {
    /// Map the shared envelope codec's failure arms onto the drift cache's
    /// own error vocabulary, mirroring the journal's mapping so the public
    /// error type does not leak `EnvelopeError` (REQ-007).
    fn from(err: crate::version_envelope::EnvelopeError) -> Self {
        match err {
            crate::version_envelope::EnvelopeError::Truncated { got, need } => {
                Self::Truncated { got, need }
            }
            crate::version_envelope::EnvelopeError::VersionMismatch { found, supported } => {
                Self::VersionMismatch { found, supported }
            }
        }
    }
}

/// Filename suffix for the sibling tempfile the atomic write stages bytes
/// in before the rename.
const TEMP_SUFFIX: &str = ".tmp";

/// Write `cache` to `path` atomically: encode to bytes, write them to a
/// sibling `<path>.tmp`, then rename it over `path`. A concurrent reader
/// (e.g. `patina debug drift-cache`) therefore observes either the previous
/// complete cache or the new complete cache, never a half-written file
/// (REQ-007 `<done-when>`).
///
/// The rename is the atomic point: POSIX `rename(2)` and Windows
/// `MoveFileEx` both replace the destination as a single operation, so the
/// destination is never observed truncated mid-write.
///
/// # Errors
///
/// Returns [`DriftCacheError::Encode`] if the cache cannot be serialized, or
/// [`DriftCacheError::Filesystem`] if the tempfile write or the rename
/// fails.
pub fn write_drift_cache(
    path: impl AsRef<Utf8Path>,
    cache: &DriftCache,
) -> Result<(), DriftCacheError> {
    let path = path.as_ref();
    let bytes = cache.encode()?;
    let temp_path = sibling_temp_path(path);
    fs_err::write(&temp_path, &bytes)?;
    fs_err::rename(&temp_path, path)?;
    Ok(())
}

/// The sibling tempfile path the atomic write stages into: the destination
/// filename with [`TEMP_SUFFIX`] appended, so it lands in the same directory
/// (and thus the same filesystem) and the rename is atomic rather than a
/// cross-device copy.
fn sibling_temp_path(path: &Utf8Path) -> Utf8PathBuf {
    let mut temp = path.to_owned();
    let name = match path.file_name() {
        Some(name) => format!("{name}{TEMP_SUFFIX}"),
        // A path with no filename component cannot be a real cache path;
        // fall back to a fixed tempfile name so the write still has a
        // sibling to stage into rather than panicking.
        None => TEMP_SUFFIX.trim_start_matches('.').to_owned(),
    };
    temp.set_file_name(name);
    temp
}

/// Read and decode the drift cache at `path`, parallel to the journal's
/// [`load_plan_file`](crate::journal::load_plan_file).
///
/// # Errors
///
/// - [`DriftCacheError::Filesystem`] if the file is missing or unreadable.
/// - [`DriftCacheError::Truncated`] / [`DriftCacheError::VersionMismatch`] /
///   [`DriftCacheError::Decode`] if the bytes fail to decode, including a
///   [`DriftCacheError::VersionMismatch`] for a cache written by a newer
///   binary.
pub fn load_drift_cache_file(path: impl AsRef<Utf8Path>) -> Result<DriftCache, DriftCacheError> {
    let bytes = fs_err::read(path.as_ref())?;
    DriftCache::decode(&bytes)
}

/// Render a decoded [`DriftCache`] to a human-readable string, parallel to
/// the journal's [`render_plan`](crate::journal::render_plan): a header line
/// carrying the drift-cache version and the bound journal timestamp,
/// followed by one block per entry naming the target path, both hashes, and
/// the human-rendered detection time.
///
/// The output is for a human reading `patina debug drift-cache`; it is
/// deliberately **not** a stable, machine-parsed format.
#[must_use = "the rendered cache is the debug command's stdout payload"]
pub fn render_drift_cache(cache: &DriftCache) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    // `write!`/`writeln!` into a `String` is infallible (the `fmt::Error`
    // path is for IO sinks), so discarding the `Result` introduces no panic
    // path — no `expect` in production.
    ignore_fmt(writeln!(
        out,
        "drift-cache version: {DRIFT_CACHE_MAJOR_VERSION}"
    ));
    ignore_fmt(writeln!(out, "journal timestamp: {}", cache.journal_ts));
    ignore_fmt(writeln!(out, "entries: {}", cache.entries.len()));
    for (index, entry) in cache.entries.iter().enumerate() {
        ignore_fmt(writeln!(out, "[{index}] {}", entry.target));
        ignore_fmt(writeln!(
            out,
            "    expected_hash: {}",
            hex_encode(&entry.expected_hash)
        ));
        ignore_fmt(writeln!(
            out,
            "    actual_hash:   {}",
            hex_encode(&entry.actual_hash)
        ));
        ignore_fmt(writeln!(
            out,
            "    detected_at:   {}",
            render_detected_at(entry.detected_at_unix)
        ));
    }
    out
}

/// Discard an infallible `String` formatting result, mirroring
/// `journal/render.rs::ignore_fmt`: it documents that the discard is
/// deliberate without a bare `let _`.
fn ignore_fmt(_result: std::fmt::Result) {}

/// Lower-case hex-encode a 32-byte hash for the human-readable view. Shared
/// with the drift handler ([`crate::watch::drift`]), which logs the same hex
/// form in its `drift` event so the log and `patina debug drift-cache` agree.
pub(super) fn hex_encode(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        ignore_fmt(write!(out, "{byte:02x}"));
    }
    out
}

/// Render an internal `detected_at_unix` (seconds since the Unix epoch) as a
/// human-readable RFC 3339 string for the debug view. Falls back to the raw
/// integer for a timestamp outside `jiff`'s representable range, so the
/// value is surfaced rather than dropped.
fn render_detected_at(detected_at_unix: i64) -> String {
    jiff::Timestamp::from_second(detected_at_unix)
        .map_or_else(|_| detected_at_unix.to_string(), |ts| ts.to_string())
}

/// Reformat a compact journal timestamp the way the journal renderer does,
/// re-exported so a future caller rendering the bound `journal_ts` in RFC
/// 3339 form has the same helper the journal uses rather than a parallel
/// copy.
#[must_use = "the RFC 3339 timestamp is the human-readable journal binding"]
pub fn journal_ts_rfc3339(journal_ts: &str) -> String {
    timestamp_to_rfc3339(journal_ts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::FILE_MAJOR_VERSION;
    use tempfile::TempDir;

    fn sample() -> DriftCache {
        DriftCache::new(
            "20260528T120000Z",
            vec![DriftEntry::new(
                Utf8PathBuf::from("/home/u/.gitconfig"),
                crate::journal::content_hash(b"H1"),
                crate::journal::content_hash(b"H2"),
                1_716_897_600,
            )],
        )
    }

    fn temp_dir() -> (TempDir, Utf8PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path())
            .expect("utf8 tempdir")
            .to_owned();
        (temp, dir)
    }

    #[test]
    fn write_then_load_round_trips_including_ts_and_both_hashes() {
        let (_temp, dir) = temp_dir();
        let path = dir.join("drift.cache");
        let cache = sample();

        write_drift_cache(&path, &cache).expect("write");
        let loaded = load_drift_cache_file(&path).expect("load");

        assert_eq!(loaded, cache);
        assert_eq!(loaded.journal_ts, "20260528T120000Z");
        let entry = loaded.entries.first().expect("one entry");
        assert_eq!(entry.expected_hash, crate::journal::content_hash(b"H1"));
        assert_eq!(entry.actual_hash, crate::journal::content_hash(b"H2"));
        assert_ne!(entry.expected_hash, entry.actual_hash);
    }

    #[test]
    fn newer_major_is_refused_naming_both_versions() {
        let mut bytes = sample().encode().expect("encode");
        bytes
            .get_mut(..2)
            .expect("envelope")
            .copy_from_slice(&(DRIFT_CACHE_MAJOR_VERSION + 1).to_le_bytes());

        let err = DriftCache::decode(&bytes).expect_err("newer major must error");
        assert!(
            matches!(
                &err,
                DriftCacheError::VersionMismatch { found, supported }
                    if *found == DRIFT_CACHE_MAJOR_VERSION + 1
                        && *supported == DRIFT_CACHE_MAJOR_VERSION
            ),
            "expected a VersionMismatch naming both majors, got {err:?}"
        );
    }

    #[test]
    fn decode_uses_its_own_major_not_the_journals() {
        // A cache encoded at the drift-cache major must decode here even
        // when the journal's own major differs, proving the drift cache
        // does not validate against FILE_MAJOR_VERSION. The two constants
        // are independent: were `decode` to check the journal's major, a
        // cache at major 1 would be refused once the journal moved past it.
        assert_ne!(
            DRIFT_CACHE_MAJOR_VERSION, FILE_MAJOR_VERSION,
            "the two formats version separately; this test pins that they \
             currently differ so a shared-major regression is caught"
        );
        let cache = sample();
        let bytes = cache.encode().expect("encode");
        assert_eq!(
            version_envelope::read_envelope_version(&bytes).expect("version"),
            DRIFT_CACHE_MAJOR_VERSION,
            "the encoded envelope must carry the drift-cache major, not the journal's"
        );
        assert_eq!(DriftCache::decode(&bytes).expect("decode"), cache);
    }

    #[test]
    fn empty_buffer_is_truncated_not_misdecoded() {
        assert!(matches!(
            DriftCache::decode(&[]),
            Err(DriftCacheError::Truncated { got: 0, need: 2 })
        ));
    }

    #[test]
    fn write_lands_via_rename_leaving_no_tempfile_and_no_inplace_truncation() {
        let (_temp, dir) = temp_dir();
        let path = dir.join("drift.cache");

        // Seed a prior complete cache, then overwrite it. If the write
        // truncated the destination in place, an interrupted reader could
        // see a short file; the rename guarantees the destination is only
        // ever the old or the new complete bytes.
        let first = DriftCache::new("20260101T000000Z", Vec::new());
        write_drift_cache(&path, &first).expect("write first");
        let second = sample();
        write_drift_cache(&path, &second).expect("write second");

        // The sibling tempfile must not linger after a successful rename.
        let temp_path = sibling_temp_path(&path);
        assert!(
            !temp_path.exists(),
            "the staging tempfile {temp_path} must be renamed away, not left behind"
        );
        // The final bytes at the destination are the second cache in full.
        assert_eq!(load_drift_cache_file(&path).expect("load"), second);
    }

    #[test]
    fn sibling_temp_path_stays_in_the_same_directory() {
        let temp = sibling_temp_path(Utf8Path::new("/var/state/patina/drift.cache"));
        assert_eq!(temp.parent(), Some(Utf8Path::new("/var/state/patina")));
        assert_eq!(temp.file_name(), Some("drift.cache.tmp"));
    }

    #[test]
    fn load_missing_path_is_a_filesystem_error() {
        let err = load_drift_cache_file(Utf8Path::new("/no/such/drift.cache"))
            .expect_err("missing path must error");
        assert!(matches!(err, DriftCacheError::Filesystem(_)), "{err:?}");
    }

    #[test]
    fn render_names_version_journal_ts_target_and_both_hashes() {
        let text = render_drift_cache(&sample());
        assert!(
            text.contains(&format!("version: {DRIFT_CACHE_MAJOR_VERSION}")),
            "{text}"
        );
        assert!(text.contains("20260528T120000Z"), "{text}");
        assert!(text.contains(".gitconfig"), "{text}");
        assert!(
            text.contains(&hex_encode(&crate::journal::content_hash(b"H1"))),
            "{text}"
        );
        assert!(
            text.contains(&hex_encode(&crate::journal::content_hash(b"H2"))),
            "{text}"
        );
    }

    #[test]
    fn render_detected_at_is_human_readable_for_a_real_timestamp() {
        // 1_716_897_600 is 2024-05-28T12:00:00Z.
        assert_eq!(render_detected_at(1_716_897_600), "2024-05-28T12:00:00Z");
    }

    #[test]
    fn journal_ts_rfc3339_matches_the_journal_renderer() {
        assert_eq!(
            journal_ts_rfc3339("20260528T120000Z"),
            timestamp_to_rfc3339("20260528T120000Z")
        );
    }
}
