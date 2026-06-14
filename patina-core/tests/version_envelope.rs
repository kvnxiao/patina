//! Integration coverage for the shared version-envelope helper.
//! The helper is the single codec the journal plan file, the
//! committed apply record, and the drift cache frame their
//! `postcard` bodies with.

use patina_core::FILE_MAJOR_VERSION;
use patina_core::version_envelope::EnvelopeError;
use patina_core::version_envelope::decode_envelope;
use patina_core::version_envelope::encode_with_envelope;

/// Scenario 1: a body framed at major 2 and decoded at major 2 returns the
/// original body unchanged.
#[test]
fn encode_then_decode_returns_the_original_body() {
    let body = b"arbitrary postcard body bytes";
    let framed = encode_with_envelope(2, body);
    let decoded = decode_envelope(&framed, 2).expect("a major-2 reader decodes a major-2 buffer");
    assert_eq!(decoded, body);
}

/// Scenario 2: a buffer whose leading `u16` is `FILE_MAJOR_VERSION + 1` is
/// refused by a `FILE_MAJOR_VERSION` reader, naming both versions.
#[test]
fn newer_major_is_refused_naming_found_and_supported() {
    let framed = encode_with_envelope(FILE_MAJOR_VERSION + 1, b"future body");
    let err = decode_envelope(&framed, FILE_MAJOR_VERSION)
        .expect_err("a buffer one major past the reader must be refused");
    assert_eq!(
        err,
        EnvelopeError::VersionMismatch {
            found: FILE_MAJOR_VERSION + 1,
            supported: FILE_MAJOR_VERSION,
        }
    );
}
