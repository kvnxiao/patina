//! The committed apply record persisted in the `<ts>.COMMIT` sentinel.
//!
//! Earlier the commit sentinel was an empty marker file: its mere
//! presence beside a `<ts>.plan` told crash recovery the apply
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
//! - One [`ExpectedTarget`] per materialized object, in apply order. Each
//!   records the canonical absolute target path, the canonical source the
//!   target was materialized from, and — for content targets — the content hash
//!   `status` compares the live filesystem against:
//!   - a [`ExpectedTarget::Symlink`] records the canonical link target the
//!     symlink should point at; that link target is also the symlink's source.
//!   - a [`ExpectedTarget::Content`] records the canonical source path the
//!     bytes were copied or rendered from plus a 32-byte `blake3` hash of the
//!     bytes written (copy / render), so an external edit changes the hash and
//!     surfaces as drift.
//!
//! The content hash is `blake3` rather than a `std::hash` fingerprint so the
//! same hash serves the journal here and the drift cache, which
//! compares a freshly computed `blake3` of a target against this recorded
//! value. The record shares the journal's
//! [`FILE_MAJOR_VERSION`](super::FILE_MAJOR_VERSION); per the pre-release
//! no-bump policy the on-disk major is held at `1` and is
//! not bumped per breaking change until v1.0.

use super::Disposition;
use super::JournalError;
use super::plan::FILE_MAJOR_VERSION;
use crate::version_envelope;
use camino::Utf8Path;
use serde::Deserialize;
use serde::Serialize;

/// The expected state of one materialized target, recorded at commit so
/// `patina status` can classify the live filesystem against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ExpectedTarget {
    /// The target should be a symbolic link pointing at `link_target`
    /// (a canonical absolute path). For a symlink the `link_target` is also
    /// the source the target was materialized from, so [`Self::source`]
    /// returns it.
    Symlink {
        /// Canonical absolute target path of the symlink itself.
        target: String,
        /// Canonical absolute path the link is expected to point at. This is
        /// the canonical source for a symlink target.
        link_target: String,
        /// Index of the `[[file]]` entry that materialized this target.
        /// Targets sharing an entry index form one atomic rollback unit:
        /// a multi-target entry reverts every target as a unit
        /// or fails the whole entry.
        entry: u32,
        /// How this target was classified at plan time. Per-leaf
        /// for a tree target; recovery and rollback leave an
        /// `Unchanged` target in place.
        disposition: Disposition,
    },
    /// The target should be a regular file whose bytes hash to `hash`
    /// (copy or rendered-template output).
    Content {
        /// Canonical absolute target path of the file.
        target: String,
        /// Canonical absolute source path the bytes were copied or rendered
        /// from.
        source: String,
        /// 32-byte `blake3` hash of the expected bytes.
        hash: [u8; 32],
        /// Index of the `[[file]]` entry that materialized this target
        /// (see [`ExpectedTarget::Symlink::entry`]).
        entry: u32,
        /// How this target was classified at plan time. Per-leaf
        /// for a tree target; recovery and rollback leave an
        /// `Unchanged` target in place.
        disposition: Disposition,
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

    /// The canonical absolute source path this target was materialized from:
    /// the recorded link target for a symlink, or the copied /
    /// rendered source for a content target.
    #[must_use = "the source path maps the target back to its origin"]
    pub fn source(&self) -> &str {
        match self {
            Self::Symlink { link_target, .. } => link_target,
            Self::Content { source, .. } => source,
        }
    }

    /// The index of the `[[file]]` entry that materialized this target.
    /// Rollback groups targets by this index to honour per-entry atomicity.
    #[must_use = "the entry index groups targets into atomic rollback units"]
    pub fn entry(&self) -> u32 {
        match self {
            Self::Symlink { entry, .. } | Self::Content { entry, .. } => *entry,
        }
    }

    /// How this target was classified at plan time. Per-leaf for a
    /// tree target; recovery and rollback leave an `Unchanged`
    /// target in place.
    #[must_use = "the disposition decides whether rollback and recovery touch this target"]
    pub fn disposition(&self) -> Disposition {
        match self {
            Self::Symlink { disposition, .. } | Self::Content { disposition, .. } => *disposition,
        }
    }
}

/// Compute the 32-byte `blake3` content hash of a byte slice. Used both
/// when recording an apply and when probing the live file during status, so
/// the two agree byte-for-byte.
#[must_use = "the hash is compared to detect content drift"]
pub fn content_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
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
        Ok(version_envelope::encode_with_envelope(
            FILE_MAJOR_VERSION,
            &body,
        ))
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
        let body = version_envelope::decode_envelope(bytes, FILE_MAJOR_VERSION)?;
        postcard::from_bytes(body).map_err(JournalError::Decode)
    }
}

/// Reformat a compact journal timestamp (`YYYYMMDDTHHMMSSZ`) as an RFC
/// 3339 string (`YYYY-MM-DDTHH:MM:SSZ`). Returns the input unchanged if it
/// does not match the compact shape, so a non-standard timestamp is
/// surfaced rather than silently mangled.
#[must_use = "the RFC 3339 timestamp is the `at` field status reports"]
pub fn timestamp_to_rfc3339(ts: &str) -> String {
    // The compact form is produced by `clock::current_timestamp` via jiff's
    // `strftime`, so jiff round-trips it: parse it back as a civil datetime
    // (the trailing `Z` is matched as a literal — no timezone math) and
    // re-emit it hyphenated. An input that does not match the compact shape
    // fails to parse and is returned unchanged.
    jiff::civil::DateTime::strptime("%Y%m%dT%H%M%SZ", ts).map_or_else(
        |_| ts.to_owned(),
        |dt| dt.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
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
                    disposition: Disposition::Create,
                },
                ExpectedTarget::Content {
                    target: "/home/u/.gitconfig".to_owned(),
                    source: "/repo/git/gitconfig".to_owned(),
                    hash: content_hash(b"payload"),
                    entry: 1,
                    disposition: Disposition::Update,
                },
                // Third target so the round-trip below exercises all three
                // disposition variants, including an `Unchanged`
                // target that recovery and rollback must leave in place.
                ExpectedTarget::Content {
                    target: "/home/u/.vimrc".to_owned(),
                    source: "/repo/vim/vimrc".to_owned(),
                    hash: content_hash(b"clean"),
                    entry: 2,
                    disposition: Disposition::Unchanged,
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
    fn per_leaf_dispositions_round_trip_at_major_one() {
        // A record with one Create, one Update, and one Unchanged target
        // must decode with each target's disposition unchanged, and the
        // envelope major byte must be 1. Whole-record `PartialEq` above
        // gates equality; this pins the per-target dispositions and the
        // major byte directly so a dropped field or a bumped major is caught.
        let r = record();
        let bytes = r.encode().expect("encode");
        assert_eq!(
            version_envelope::read_envelope_version(&bytes).expect("read envelope"),
            1
        );
        let decoded = ApplyRecord::decode(&bytes).expect("decode");
        let got: Vec<Disposition> = decoded
            .targets
            .iter()
            .map(ExpectedTarget::disposition)
            .collect();
        assert_eq!(
            got,
            vec![
                Disposition::Create,
                Disposition::Update,
                Disposition::Unchanged
            ]
        );
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
    fn envelope_major_byte_reads_as_one() {
        let bytes = record().encode().expect("encode");
        assert_eq!(
            version_envelope::read_envelope_version(&bytes).expect("read envelope"),
            1
        );
    }

    #[test]
    fn major_two_buffer_is_refused_not_misdecoded() {
        // An ApplyRecord buffer prefixed with major 2 must fail to decode
        // now that the supported major is 1.
        let mut bytes = record().encode().expect("encode");
        bytes
            .get_mut(..2)
            .expect("envelope")
            .copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            ApplyRecord::decode(&bytes),
            Err(JournalError::VersionMismatch {
                found: 2,
                supported: 1
            })
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
    fn content_hash_changes_when_bytes_change() {
        assert_ne!(content_hash(b"a"), content_hash(b"b"));
        assert_eq!(content_hash(b"same"), content_hash(b"same"));
    }

    #[test]
    fn content_hash_is_blake3_of_the_bytes() {
        // Pin the helper to the canonical blake3 digest so a silent swap to
        // a different hash function is caught; the journal hash must match
        // the drift cache.
        assert_eq!(
            content_hash(b"payload"),
            *blake3::hash(b"payload").as_bytes()
        );
    }

    #[test]
    fn source_accessor_returns_origin_for_each_variant() {
        let sym = ExpectedTarget::Symlink {
            target: "/t/s".to_owned(),
            link_target: "/r/s".to_owned(),
            entry: 0,
            disposition: Disposition::Create,
        };
        let content = ExpectedTarget::Content {
            target: "/t/c".to_owned(),
            source: "/r/c".to_owned(),
            hash: [0u8; 32],
            entry: 3,
            disposition: Disposition::Update,
        };
        assert_eq!(sym.source(), "/r/s");
        assert_eq!(content.source(), "/r/c");
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
            disposition: Disposition::Unchanged,
        };
        let content = ExpectedTarget::Content {
            target: "/t/c".to_owned(),
            source: "/r/c".to_owned(),
            hash: [0u8; 32],
            entry: 3,
            disposition: Disposition::Update,
        };
        assert_eq!(sym.target(), "/t/s");
        assert_eq!(content.target(), "/t/c");
        assert_eq!(sym.entry(), 0);
        assert_eq!(content.entry(), 3);
        // The disposition accessor reads the per-variant field, so a swap
        // of the two arms would surface here.
        assert_eq!(sym.disposition(), Disposition::Unchanged);
        assert_eq!(content.disposition(), Disposition::Update);
    }
}
