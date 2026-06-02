//! Integration coverage for the drift-cache format (SPEC-0003 T-004,
//! REQ-007): round-trip through the atomic write, newer-major refusal,
//! independence from the journal's major version, and the rename-based
//! write guarantee — all against the crate's public API surface.

use camino::Utf8Path;
use patina_core::DRIFT_CACHE_MAJOR_VERSION;
use patina_core::content_hash;
use patina_core::watch::drift_cache::DriftCache;
use patina_core::watch::drift_cache::DriftCacheError;
use patina_core::watch::drift_cache::DriftEntry;
use patina_core::watch::drift_cache::load_drift_cache_file;
use patina_core::watch::drift_cache::write_drift_cache;
use tempfile::TempDir;

fn sample() -> DriftCache {
    DriftCache::new(
        "20260528T120000Z",
        vec![DriftEntry::new(
            "/home/u/.gitconfig",
            content_hash(b"H1"),
            content_hash(b"H2"),
            1_716_897_600,
        )],
    )
}

#[test]
fn write_then_load_round_trips_through_the_public_api() {
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let path = dir.join("drift.cache");
    let cache = sample();

    write_drift_cache(&path, &cache).expect("write drift cache");
    let loaded = load_drift_cache_file(&path).expect("load drift cache");

    assert_eq!(loaded, cache);
    assert_eq!(loaded.journal_ts, "20260528T120000Z");
    let entry = loaded.entries.first().expect("one entry");
    assert_eq!(entry.expected_hash, content_hash(b"H1"));
    assert_eq!(entry.actual_hash, content_hash(b"H2"));
}

#[test]
fn newer_major_load_is_refused_naming_both_versions() {
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let path = dir.join("drift.cache");

    let mut bytes = sample().encode().expect("encode");
    bytes
        .get_mut(..2)
        .expect("envelope")
        .copy_from_slice(&(DRIFT_CACHE_MAJOR_VERSION + 1).to_le_bytes());
    fs_err::write(&path, bytes).expect("write tampered cache");

    let err = load_drift_cache_file(&path).expect_err("newer major must error");
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
fn drift_cache_major_is_independent_of_the_journal_major() {
    // The two formats version separately (REQ-007): a regression that made
    // the drift cache validate against the journal's major would break the
    // moment the two diverge. This pins that a cache encoded at the
    // drift-cache major carries that major and decodes against it,
    // independent of the journal's `FILE_MAJOR_VERSION` (the two may
    // currently coincide at 1 pre-release, so an inequality assertion would
    // gate the constants' values rather than the coupling behaviour).
    let cache = sample();
    let bytes = cache.encode().expect("encode");
    assert_eq!(
        bytes.get(..2),
        Some(DRIFT_CACHE_MAJOR_VERSION.to_le_bytes().as_slice()),
        "the encoded envelope carries the drift-cache major, not the journal's"
    );
    assert_eq!(DriftCache::decode(&bytes).expect("decode"), cache);
}

#[test]
fn atomic_write_replaces_via_rename_leaving_no_staging_tempfile() {
    let temp = TempDir::new().expect("tempdir");
    let dir = Utf8Path::from_path(temp.path()).expect("utf8 tempdir");
    let path = dir.join("drift.cache");

    write_drift_cache(&path, &DriftCache::new("20260101T000000Z", Vec::new()))
        .expect("write first");
    let second = sample();
    write_drift_cache(&path, &second).expect("write second");

    // No leftover staging tempfile beside the destination after a
    // successful rename, and the destination holds the second cache in full
    // (the rename never truncated the destination in place).
    assert!(
        !dir.join("drift.cache.tmp").exists(),
        "the staging tempfile must be renamed away, not left behind"
    );
    assert_eq!(load_drift_cache_file(&path).expect("load"), second);
}
