//! The committed apply record persisted in the `<ts>.COMMIT` sentinel
//! (REQ-018, T-017).
//!
//! Before T-017 the commit sentinel was an empty marker file: its mere
//! presence beside a `<ts>.plan` told crash recovery (T-011) the apply
//! had committed. `patina status` needs more than presence — it must know
//! *what* the last committed apply materialized, and the expected state
//! of each target, so it can classify the live filesystem against it. The
//! plan file is deleted at commit, so the record cannot live there.
//!
//! This module makes the commit sentinel carry that payload. The sentinel
//! now holds a `postcard`-encoded [`ApplyRecord`] behind the same
//! fixed-size version envelope the plan file uses, so a future format
//! change is refused rather than mis-decoded. Recovery is unaffected: it
//! keys solely on the sentinel's *existence* beside an orphan plan and
//! never decodes its body.
//!
//! ## What is recorded
//!
//! - `last_apply` metadata: the apply timestamp (`at`, an RFC 3339 string
//!   derived from the journal `<ts>`), the `user`, and the `host`.
//! - One [`ExpectedTarget`] per materialized object, in apply order. Each pairs
//!   the canonical absolute target path with the fingerprint `status` compares
//!   the live filesystem against:
//!   - a [`ExpectedTarget::Symlink`] records the canonical link target the
//!     symlink should point at;
//!   - a [`ExpectedTarget::Content`] records a 64-bit fingerprint of the bytes
//!     written (copy / render), so an external edit changes the fingerprint and
//!     surfaces as drift.
//!
//! The fingerprint is a non-cryptographic `std::hash` digest: status only
//! needs to notice an *accidental* external edit, not resist an adversary
//! forging a collision, so a std-only hash keeps the dependency surface
//! flat (no new crate to clear through `deny.toml`).

use super::JournalError;
use super::plan::FILE_MAJOR_VERSION;
use camino::Utf8Path;
use serde::Deserialize;
use serde::Serialize;
use std::hash::Hash as _;
use std::hash::Hasher as _;

/// Width in bytes of the version envelope prefix (a little-endian `u16`),
/// shared with the plan-file layout in [`super::plan`].
const ENVELOPE_LEN: usize = core::mem::size_of::<u16>();

/// The expected state of one materialized target, recorded at commit so
/// `patina status` can classify the live filesystem against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ExpectedTarget {
    /// The target should be a symbolic link pointing at `link_target`
    /// (a canonical absolute path).
    Symlink {
        /// Canonical absolute target path of the symlink itself.
        target: String,
        /// Canonical absolute path the link is expected to point at.
        link_target: String,
        /// Index of the `[[file]]` entry that materialized this target.
        /// Targets sharing an entry index form one atomic rollback unit
        /// (REQ-019): a multi-target entry reverts every target as a unit
        /// or fails the whole entry.
        entry: u32,
    },
    /// The target should be a regular file whose bytes fingerprint to
    /// `fingerprint` (copy or rendered-template output).
    Content {
        /// Canonical absolute target path of the file.
        target: String,
        /// 64-bit non-cryptographic fingerprint of the expected bytes.
        fingerprint: u64,
        /// Index of the `[[file]]` entry that materialized this target
        /// (see [`ExpectedTarget::Symlink::entry`]).
        entry: u32,
    },
}

impl ExpectedTarget {
    /// The canonical absolute target path this expectation is for.
    #[must_use = "the target path is the key status classifies against"]
    pub fn target(&self) -> &str {
        match self {
            Self::Symlink { target, .. } | Self::Content { target, .. } => target,
        }
    }

    /// The index of the `[[file]]` entry that materialized this target.
    /// Rollback groups targets by this index to honour per-entry atomicity
    /// (REQ-019).
    #[must_use = "the entry index groups targets into atomic rollback units"]
    pub fn entry(&self) -> u32 {
        match self {
            Self::Symlink { entry, .. } | Self::Content { entry, .. } => *entry,
        }
    }
}

/// Compute the 64-bit fingerprint of a byte slice with the std default
/// hasher. Used both when recording an apply and when probing the live
/// file during status, so the two agree byte-for-byte.
#[must_use = "the fingerprint is compared to detect content drift"]
pub fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The `last_apply` metadata block surfaced by `patina status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastApply {
    /// RFC 3339 timestamp of the apply, derived from the journal `<ts>`.
    pub at: String,
    /// User who ran the apply (`patina.user`).
    pub user: String,
    /// Host the apply ran on (`patina.hostname`).
    pub host: String,
}

/// The full record persisted in a committed apply's `<ts>.COMMIT`
/// sentinel: the `last_apply` metadata plus one [`ExpectedTarget`] per
/// materialized object, in apply order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyRecord {
    /// Metadata about who applied, when, and where.
    pub last_apply: LastApply,
    /// Per-target expected state, in apply order.
    pub targets: Vec<ExpectedTarget>,
}

impl ApplyRecord {
    /// Build a record from its metadata and per-target expectations.
    #[must_use = "an apply record must be written into the commit sentinel to take effect"]
    pub fn new(last_apply: LastApply, targets: Vec<ExpectedTarget>) -> Self {
        Self {
            last_apply,
            targets,
        }
    }

    /// Encode to the on-disk commit-sentinel byte form: the little-endian
    /// `u16` version envelope followed by the `postcard`-encoded body.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Encode`] if `postcard` serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>, JournalError> {
        let body = postcard::to_stdvec(self).map_err(JournalError::Encode)?;
        let mut bytes = Vec::with_capacity(ENVELOPE_LEN + body.len());
        bytes.extend_from_slice(&FILE_MAJOR_VERSION.to_le_bytes());
        bytes.extend_from_slice(&body);
        Ok(bytes)
    }

    /// Decode a record from a commit sentinel's bytes, refusing any record
    /// whose major version exceeds [`FILE_MAJOR_VERSION`].
    ///
    /// # Errors
    ///
    /// - [`JournalError::Truncated`] if the envelope is missing.
    /// - [`JournalError::VersionMismatch`] if the record is from a newer format
    ///   than this binary supports.
    /// - [`JournalError::Decode`] if the body fails to deserialize.
    pub fn decode(bytes: &[u8]) -> Result<Self, JournalError> {
        let envelope = bytes.get(..ENVELOPE_LEN).ok_or(JournalError::Truncated {
            got: bytes.len(),
            need: ENVELOPE_LEN,
        })?;
        let mut raw = [0u8; ENVELOPE_LEN];
        raw.copy_from_slice(envelope);
        let found = u16::from_le_bytes(raw);
        if found > FILE_MAJOR_VERSION {
            return Err(JournalError::VersionMismatch {
                found,
                supported: FILE_MAJOR_VERSION,
            });
        }
        let body = bytes.get(ENVELOPE_LEN..).ok_or(JournalError::Truncated {
            got: bytes.len(),
            need: ENVELOPE_LEN,
        })?;
        postcard::from_bytes(body).map_err(JournalError::Decode)
    }
}

/// Reformat a compact journal timestamp (`YYYYMMDDTHHMMSSZ`) as an RFC
/// 3339 string (`YYYY-MM-DDTHH:MM:SSZ`). Returns the input unchanged if it
/// does not match the compact 16-character shape, so a non-standard
/// timestamp is surfaced rather than silently mangled.
#[must_use = "the RFC 3339 timestamp is the `at` field status reports"]
pub fn timestamp_to_rfc3339(ts: &str) -> String {
    let bytes = ts.as_bytes();
    if bytes.len() != 16 || bytes.get(8) != Some(&b'T') || bytes.last() != Some(&b'Z') {
        return ts.to_owned();
    }
    // The verified shape is `YYYYMMDDTHHMMSSZ` (16 ASCII bytes): 8 date
    // bytes, the `T` at index 8, 6 time bytes, then the trailing `Z`. The
    // slices below are total against that shape.
    let (date, rest) = ts.split_at(8);
    // `rest` is `THHMMSSZ` (8 bytes); drop the leading `T`, then keep the
    // first 6 of the remaining `HHMMSSZ` to get `HHMMSS`. Using `split_at`
    // (char-boundary safe) rather than range indexing keeps the slicing
    // panic-free for the verified ASCII shape.
    let after_t = date_time_after_t(rest);
    let (time, _z) = after_t.split_at(6);
    let (year, monthday) = date.split_at(4);
    let (month, day) = monthday.split_at(2);
    let (hour, minsec) = time.split_at(2);
    let (minute, second) = minsec.split_at(2);
    format!("{year}-{month}-{day}T{hour}:{minute}:{second}Z")
}

/// Drop the leading `T` from the `THHMMSSZ` time portion, returning
/// `HHMMSSZ`. Split-based so it cannot panic on a non-char-boundary.
fn date_time_after_t(rest: &str) -> &str {
    rest.split_at(1).1
}

/// Whether `path` currently resolves to a symbolic link, reading its
/// link target. Returns `Some(link_target)` when the path is a symlink,
/// `None` when it is absent or not a link.
#[must_use = "the read link target is compared to the recorded expectation"]
pub fn read_symlink_target(path: &Utf8Path) -> Option<String> {
    let meta = fs_err::symlink_metadata(path).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let raw = fs_err::read_link(path).ok()?;
    let utf8 = camino::Utf8PathBuf::from_path_buf(raw).ok()?;
    Some(utf8.into_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> ApplyRecord {
        ApplyRecord::new(
            LastApply {
                at: "2026-05-28T12:00:00Z".to_owned(),
                user: "u".to_owned(),
                host: "h".to_owned(),
            },
            vec![
                ExpectedTarget::Symlink {
                    target: "/home/u/.zshrc".to_owned(),
                    link_target: "/repo/zsh/zshrc".to_owned(),
                    entry: 0,
                },
                ExpectedTarget::Content {
                    target: "/home/u/.gitconfig".to_owned(),
                    fingerprint: fingerprint_bytes(b"payload"),
                    entry: 1,
                },
            ],
        )
    }

    #[test]
    fn encode_decode_round_trips() {
        let r = record();
        let bytes = r.encode().expect("encode");
        assert_eq!(ApplyRecord::decode(&bytes).expect("decode"), r);
    }

    #[test]
    fn newer_major_is_refused() {
        let mut bytes = record().encode().expect("encode");
        bytes
            .get_mut(..2)
            .expect("envelope")
            .copy_from_slice(&(FILE_MAJOR_VERSION + 1).to_le_bytes());
        assert!(matches!(
            ApplyRecord::decode(&bytes),
            Err(JournalError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn empty_buffer_is_truncated_not_misdecoded() {
        assert!(matches!(
            ApplyRecord::decode(&[]),
            Err(JournalError::Truncated { got: 0, need: 2 })
        ));
    }

    #[test]
    fn fingerprint_changes_when_bytes_change() {
        assert_ne!(fingerprint_bytes(b"a"), fingerprint_bytes(b"b"));
        assert_eq!(fingerprint_bytes(b"same"), fingerprint_bytes(b"same"));
    }

    #[test]
    fn compact_timestamp_becomes_rfc3339() {
        assert_eq!(
            timestamp_to_rfc3339("20260528T120000Z"),
            "2026-05-28T12:00:00Z"
        );
    }

    #[test]
    fn non_compact_timestamp_passes_through_unchanged() {
        assert_eq!(timestamp_to_rfc3339("not-a-ts"), "not-a-ts");
        // A 16-char string lacking the T/Z markers is left alone.
        assert_eq!(timestamp_to_rfc3339("2026052812000099"), "2026052812000099");
    }

    #[test]
    fn target_accessor_returns_the_path_for_each_variant() {
        let sym = ExpectedTarget::Symlink {
            target: "/t/s".to_owned(),
            link_target: "/r/s".to_owned(),
            entry: 0,
        };
        let content = ExpectedTarget::Content {
            target: "/t/c".to_owned(),
            fingerprint: 0,
            entry: 3,
        };
        assert_eq!(sym.target(), "/t/s");
        assert_eq!(content.target(), "/t/c");
        assert_eq!(sym.entry(), 0);
        assert_eq!(content.entry(), 3);
    }
}
