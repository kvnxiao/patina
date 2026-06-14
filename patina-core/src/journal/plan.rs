//! The binary plan record and its version envelope.
//!
//! A plan file is laid out as a fixed-size version envelope followed by
//! the `postcard`-encoded [`Plan`] body:
//!
//! ```text
//! offset 0   offset 2
//! ┌────────┬───────────────────────────────┐
//! │ u16 LE │ postcard-encoded Plan body ... │
//! │ major  │                                │
//! └────────┴───────────────────────────────┘
//! ```
//!
//! The major version lives in the first two bytes so a reader can decide
//! whether it is able to decode the body **before** invoking the full
//! `postcard` decoder. A binary refuses any plan whose major version
//! exceeds its own compiled [`FILE_MAJOR_VERSION`], returning
//! [`JournalError::VersionMismatch`](super::JournalError::VersionMismatch)
//! rather than risk mis-decoding a future format.

use super::Disposition;
use super::JournalError;
use crate::version_envelope;
use serde::Deserialize;
use serde::Serialize;

/// Current on-disk plan format major version. Bump when the serialized
/// [`Plan`] layout changes incompatibly; older binaries then refuse the
/// newer file via the version envelope.
pub const FILE_MAJOR_VERSION: u16 = 1;

/// One planned filesystem operation. This is the minimal record the
/// journal needs in v1; the executor and recovery extend
/// the variant set with the inverse-operation data they require. The
/// representation is intentionally self-describing so a decoded plan can
/// be probed against the filesystem during recovery without re-reading
/// the source repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PlannedOperation {
    /// Create a symbolic link at `target` pointing back into the repo at
    /// `source` (a repo-relative path).
    Symlink {
        /// Repo-relative source the link points at.
        source: String,
        /// Absolute target path the link is created at.
        target: String,
        /// How this operation relates to the live filesystem.
        /// For a tree op this is the per-op aggregate.
        disposition: Disposition,
    },
    /// Render a template from `source` and write the output to `target`.
    Render {
        /// Repo-relative template source.
        source: String,
        /// Absolute target path the rendered output is written to.
        target: String,
        /// How this operation relates to the live filesystem.
        disposition: Disposition,
    },
    /// Copy bytes from `source` to `target` (used where a link is not
    /// appropriate).
    Copy {
        /// Repo-relative source file.
        source: String,
        /// Absolute target path the bytes are copied to.
        target: String,
        /// How this operation relates to the live filesystem.
        /// For a tree op this is the per-op aggregate.
        disposition: Disposition,
    },
}

impl PlannedOperation {
    /// Construct a [`PlannedOperation::Symlink`] from string-ish inputs and
    /// its classified [`Disposition`].
    #[must_use = "construct the operation to include it in a plan"]
    pub fn symlink(
        source: impl Into<String>,
        target: impl Into<String>,
        disposition: Disposition,
    ) -> Self {
        Self::Symlink {
            source: source.into(),
            target: target.into(),
            disposition,
        }
    }

    /// Construct a [`PlannedOperation::Render`] from string-ish inputs and
    /// its classified [`Disposition`].
    #[must_use = "construct the operation to include it in a plan"]
    pub fn render(
        source: impl Into<String>,
        target: impl Into<String>,
        disposition: Disposition,
    ) -> Self {
        Self::Render {
            source: source.into(),
            target: target.into(),
            disposition,
        }
    }

    /// Construct a [`PlannedOperation::Copy`] from string-ish inputs and its
    /// classified [`Disposition`].
    #[must_use = "construct the operation to include it in a plan"]
    pub fn copy(
        source: impl Into<String>,
        target: impl Into<String>,
        disposition: Disposition,
    ) -> Self {
        Self::Copy {
            source: source.into(),
            target: target.into(),
            disposition,
        }
    }

    /// How this operation relates to the live filesystem. For a
    /// tree op this is the per-op aggregate disposition.
    #[must_use = "the disposition decides whether the operation writes, and how recovery reverses it"]
    pub fn disposition(&self) -> Disposition {
        match self {
            Self::Symlink { disposition, .. }
            | Self::Render { disposition, .. }
            | Self::Copy { disposition, .. } => *disposition,
        }
    }
}

/// The full set of operations one `patina apply` will perform, recorded
/// durably before any mutation begins.
///
/// A plan is content-deterministic: the same source repository and the
/// same variable context produce a byte-identical encoded plan, modulo
/// the timestamp in the filename. The encoder
/// preserves operation order, which is also the order in which the
/// progress cursor records completions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    operations: Vec<PlannedOperation>,
}

impl Plan {
    /// Build a plan from an ordered list of operations.
    #[must_use = "a plan must be flushed via Journal::flush_plan_and_fsync to take effect"]
    pub fn new(operations: Vec<PlannedOperation>) -> Self {
        Self { operations }
    }

    /// The operations in execution order.
    #[must_use = "inspect the planned operations to drive execution or recovery"]
    pub fn operations(&self) -> &[PlannedOperation] {
        &self.operations
    }

    /// Number of operations in the plan.
    #[must_use = "the operation count bounds the progress cursor"]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Whether the plan contains no operations.
    #[must_use = "an empty plan still writes a journal entry"]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Encode the plan to its on-disk byte form: the little-endian `u16`
    /// version envelope followed by the `postcard`-encoded body.
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

    /// Read the major version from a plan file's envelope without
    /// invoking the full `postcard` decoder.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Truncated`] if `bytes` is shorter than the
    /// envelope.
    pub fn read_envelope_version(bytes: &[u8]) -> Result<u16, JournalError> {
        Ok(version_envelope::read_envelope_version(bytes)?)
    }

    /// Decode a plan from its on-disk byte form, refusing any plan whose
    /// major version exceeds [`FILE_MAJOR_VERSION`].
    ///
    /// # Errors
    ///
    /// - [`JournalError::Truncated`] if the envelope is missing.
    /// - [`JournalError::VersionMismatch`] if the plan is from a newer format
    ///   than this binary supports.
    /// - [`JournalError::Decode`] if the body fails to deserialize.
    pub fn decode(bytes: &[u8]) -> Result<Self, JournalError> {
        let body = version_envelope::decode_envelope(bytes, FILE_MAJOR_VERSION)?;
        postcard::from_bytes(body).map_err(JournalError::Decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Plan {
        // One Create, one Update, one Unchanged target so the
        // round-trip below proves every disposition variant survives the
        // envelope, not just whichever one a single-op fixture happened to use.
        Plan::new(vec![
            PlannedOperation::symlink("a", "/x/a", Disposition::Create),
            PlannedOperation::render("b.j2", "/x/b", Disposition::Update),
            PlannedOperation::copy("c", "/x/c", Disposition::Unchanged),
        ])
    }

    #[test]
    fn encode_decode_round_trips() {
        let plan = sample();
        let bytes = plan.encode().expect("encode");
        assert_eq!(Plan::decode(&bytes).expect("decode"), plan);
    }

    #[test]
    fn per_op_dispositions_round_trip() {
        // A plan with one Create, one Update, and one Unchanged op must
        // decode with each op's disposition unchanged. `PartialEq` on the
        // whole plan above already gates this, but asserting the per-op
        // dispositions directly pins the field rather than the aggregate
        // equality, so a field dropped from the wire is caught here.
        let plan = sample();
        let decoded = Plan::decode(&plan.encode().expect("encode")).expect("decode");
        let got: Vec<Disposition> = decoded
            .operations()
            .iter()
            .map(PlannedOperation::disposition)
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
    fn envelope_carries_current_major_at_offset_zero() {
        let bytes = sample().encode().expect("encode");
        assert_eq!(
            Plan::read_envelope_version(&bytes).expect("read envelope"),
            FILE_MAJOR_VERSION
        );
        // The envelope is the literal first two little-endian bytes.
        assert_eq!(
            bytes.get(..2),
            Some(FILE_MAJOR_VERSION.to_le_bytes().as_slice())
        );
    }

    #[test]
    fn truncated_buffer_is_rejected_before_decode() {
        let err = Plan::decode(&[0u8]).expect_err("one byte cannot hold the envelope");
        assert!(matches!(err, JournalError::Truncated { got: 1, need: 2 }));
    }

    #[test]
    fn current_major_decodes_but_newer_major_is_refused() {
        let mut bytes = sample().encode().expect("encode");
        // Equal-to-current decodes.
        Plan::decode(&bytes).expect("current major version decodes");
        // One past current is refused.
        bytes
            .get_mut(..2)
            .expect("encoded plan has a 2-byte envelope")
            .copy_from_slice(&(FILE_MAJOR_VERSION + 1).to_le_bytes());
        assert!(matches!(
            Plan::decode(&bytes),
            Err(JournalError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn on_disk_major_is_held_at_one() {
        // The pre-release on-disk format major is 1 and
        // does not bump per breaking change until v1.0. A regression that
        // bumped it (e.g. back to 2) would make older binaries refuse files
        // this binary writes, so the value is pinned by this assertion.
        assert_eq!(FILE_MAJOR_VERSION, 1);
    }

    #[test]
    fn envelope_major_byte_reads_as_one() {
        let bytes = sample().encode().expect("encode");
        assert_eq!(
            Plan::read_envelope_version(&bytes).expect("read envelope"),
            1
        );
    }

    #[test]
    fn major_two_buffer_is_refused_not_misdecoded() {
        // A buffer prefixed with major 2 wrapping an otherwise valid plan
        // body must fail with a version mismatch rather than decode, since
        // `decode_envelope` refuses `found > supported` and the supported
        // major is now 1.
        let mut bytes = sample().encode().expect("encode");
        bytes
            .get_mut(..2)
            .expect("encoded plan has a 2-byte envelope")
            .copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            Plan::decode(&bytes),
            Err(JournalError::VersionMismatch {
                found: 2,
                supported: 1
            })
        ));
    }

    #[test]
    fn empty_plan_is_empty_and_round_trips() {
        let plan = Plan::new(vec![]);
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
        let bytes = plan.encode().expect("encode");
        assert_eq!(Plan::decode(&bytes).expect("decode"), plan);
    }
}
