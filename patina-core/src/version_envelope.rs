//! Shared version-envelope codec for Patina's `postcard`-encoded binary
//! files (REQ-007).
//!
//! Several on-disk formats — the journal plan file, the committed apply
//! record, and the SPEC-0003 drift cache — prefix their `postcard` body
//! with a fixed-size major-version envelope so a reader can decide whether
//! it can decode the body **before** invoking the full decoder. A file
//! whose major version exceeds the reader's is refused rather than
//! mis-decoded.
//!
//! ```text
//! offset 0   offset 2
//! ┌────────┬──────────────────────────┐
//! │ u16 LE │ postcard-encoded body ... │
//! │ major  │                           │
//! └────────┴──────────────────────────┘
//! ```
//!
//! Each format owns its own major-version constant and versions
//! independently; this module is only the format-agnostic prefix codec
//! they share.
//!
//! # Examples
//!
//! ```
//! use patina_core::version_envelope::{decode_envelope, encode_with_envelope};
//!
//! let framed = encode_with_envelope(2, b"body");
//! let body = decode_envelope(&framed, 2)?;
//! assert_eq!(body, b"body");
//! # Ok::<(), patina_core::version_envelope::EnvelopeError>(())
//! ```

use thiserror::Error;

/// Width in bytes of the version-envelope prefix (a little-endian `u16`).
pub const ENVELOPE_LEN: usize = core::mem::size_of::<u16>();

/// A failure decoding a version envelope. Models exactly the two arms a
/// reader can hit before the body decoder runs: too few bytes to hold the
/// envelope, or a major version the reader does not support.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvelopeError {
    /// The buffer was shorter than the fixed-size envelope, so no major
    /// version could be read.
    #[error("buffer is truncated: {got} bytes, need at least {need} for the version envelope")]
    Truncated {
        /// Bytes actually present in the buffer.
        got: usize,
        /// Bytes required to read the version envelope.
        need: usize,
    },

    /// The buffer declares a major version newer than the reader supports.
    /// Refusing it is intentional: a forward-compatible decode would
    /// silently misread the body.
    #[error(
        "format major version {found} is newer than supported version {supported}; \
         upgrade patina to read this file"
    )]
    VersionMismatch {
        /// Major version read from the buffer's envelope.
        found: u16,
        /// Highest major version the reader can decode.
        supported: u16,
    },
}

/// Prepend the little-endian `u16` `major` version envelope to `body`,
/// returning the framed bytes ready to write to disk.
///
/// # Examples
///
/// ```
/// use patina_core::version_envelope::{encode_with_envelope, ENVELOPE_LEN};
///
/// let framed = encode_with_envelope(1, b"abc");
/// assert_eq!(&framed[..ENVELOPE_LEN], &1u16.to_le_bytes());
/// assert_eq!(&framed[ENVELOPE_LEN..], b"abc");
/// ```
#[must_use = "the framed bytes are what gets written to disk"]
pub fn encode_with_envelope(major: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ENVELOPE_LEN + body.len());
    bytes.extend_from_slice(&major.to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

/// Split a framed buffer into its major version and the body tail in a
/// single bounds check. The fixed-size `[u8; ENVELOPE_LEN]` chunk makes the
/// body slice fall out of the same destructure, so neither the version read
/// nor the body extraction needs a second, panic-prone index.
fn split_envelope(bytes: &[u8]) -> Result<(u16, &[u8]), EnvelopeError> {
    let (envelope, body) =
        bytes
            .split_first_chunk::<ENVELOPE_LEN>()
            .ok_or(EnvelopeError::Truncated {
                got: bytes.len(),
                need: ENVELOPE_LEN,
            })?;
    Ok((u16::from_le_bytes(*envelope), body))
}

/// Read the major version from a framed buffer's envelope without touching
/// the body.
///
/// # Errors
///
/// Returns [`EnvelopeError::Truncated`] if `bytes` is shorter than
/// [`ENVELOPE_LEN`].
pub fn read_envelope_version(bytes: &[u8]) -> Result<u16, EnvelopeError> {
    let (found, _body) = split_envelope(bytes)?;
    Ok(found)
}

/// Strip and validate the version envelope, returning the post-envelope
/// body slice. Refuses any buffer whose major version exceeds
/// `supported_major`.
///
/// # Errors
///
/// - [`EnvelopeError::Truncated`] if the envelope is missing.
/// - [`EnvelopeError::VersionMismatch`] if the buffer is from a newer format
///   than `supported_major`.
///
/// # Examples
///
/// ```
/// use patina_core::version_envelope::{decode_envelope, encode_with_envelope, EnvelopeError};
///
/// let framed = encode_with_envelope(3, b"payload");
/// // A reader supporting major 3 reads the body back.
/// assert_eq!(decode_envelope(&framed, 3)?, b"payload");
/// // A reader supporting only major 2 refuses it.
/// assert_eq!(
///     decode_envelope(&framed, 2),
///     Err(EnvelopeError::VersionMismatch { found: 3, supported: 2 })
/// );
/// # Ok::<(), EnvelopeError>(())
/// ```
pub fn decode_envelope(bytes: &[u8], supported_major: u16) -> Result<&[u8], EnvelopeError> {
    let (found, body) = split_envelope(bytes)?;
    if found > supported_major {
        return Err(EnvelopeError::VersionMismatch {
            found,
            supported: supported_major,
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_the_body() {
        let body = b"the quick brown fox";
        let framed = encode_with_envelope(2, body);
        assert_eq!(decode_envelope(&framed, 2).expect("decode"), body);
    }

    #[test]
    fn envelope_is_the_literal_leading_le_u16() {
        let framed = encode_with_envelope(7, b"x");
        assert_eq!(
            framed.get(..ENVELOPE_LEN),
            Some(7u16.to_le_bytes().as_slice())
        );
    }

    #[test]
    fn empty_body_round_trips_to_empty_slice() {
        let framed = encode_with_envelope(1, b"");
        assert_eq!(framed.len(), ENVELOPE_LEN);
        assert_eq!(decode_envelope(&framed, 1).expect("decode"), b"");
    }

    #[test]
    fn newer_major_is_refused_naming_both_versions() {
        let framed = encode_with_envelope(3, b"body");
        assert_eq!(
            decode_envelope(&framed, 2),
            Err(EnvelopeError::VersionMismatch {
                found: 3,
                supported: 2,
            })
        );
    }

    #[test]
    fn equal_major_decodes() {
        let framed = encode_with_envelope(2, b"body");
        assert_eq!(decode_envelope(&framed, 2).expect("decode"), b"body");
    }

    #[test]
    fn older_major_decodes() {
        // A buffer from an older format (major below the reader's) decodes:
        // the reader understands every prior major.
        let framed = encode_with_envelope(1, b"body");
        assert_eq!(decode_envelope(&framed, 2).expect("decode"), b"body");
    }

    #[test]
    fn truncated_buffer_is_rejected_before_decode() {
        assert_eq!(
            decode_envelope(&[0u8], 2),
            Err(EnvelopeError::Truncated { got: 1, need: 2 })
        );
        assert_eq!(
            read_envelope_version(&[]),
            Err(EnvelopeError::Truncated { got: 0, need: 2 })
        );
    }

    #[test]
    fn read_envelope_version_returns_the_major_without_consuming_body() {
        let framed = encode_with_envelope(5, b"ignored body");
        assert_eq!(read_envelope_version(&framed).expect("version"), 5);
    }
}
