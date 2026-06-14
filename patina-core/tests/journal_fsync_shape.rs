#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for the plan journal.
//!
//! These tests drive the `patina_core::journal` module directly and prove
//! the load-bearing properties those scenarios depend on:
//!
//! - the single up-front plan fsync paired with a directory fsync, with no
//!   per-operation progress fsync (fsync shape);
//! - a flushed plan is durable before the first mutation, with no COMMIT
//!   sentinel until the run commits (crash-window state);
//! - a newer-version plan is refused with a typed error naming both versions
//!   (version-envelope scenario);
//! - committing deletes the plan and progress files, leaving only the COMMIT
//!   sentinel (cleanup scenario).

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::Disposition;
use patina_core::journal::ApplyRecord;
use patina_core::journal::COMMIT_SUFFIX;
use patina_core::journal::FILE_MAJOR_VERSION;
use patina_core::journal::Journal;
use patina_core::journal::JournalError;
use patina_core::journal::LastApply;
use patina_core::journal::PLAN_SUFFIX;
use patina_core::journal::PROGRESS_SUFFIX;
use patina_core::journal::Plan;
use patina_core::journal::PlannedOperation;
use patina_core::journal::Syncer;
use std::cell::RefCell;
use tempfile::TempDir;

/// A `Syncer` that issues the real fsyncs (so durability still holds for
/// the crash-window assertions) and records every `(kind, path)` it was
/// asked to sync, so the test can assert the exact fsync shape.
#[derive(Default)]
struct RecordingSyncer {
    calls: RefCell<Vec<(SyncKind, Utf8PathBuf)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncKind {
    File,
    Dir,
}

impl RecordingSyncer {
    fn calls(&self) -> Vec<(SyncKind, Utf8PathBuf)> {
        self.calls.borrow().clone()
    }

    /// How many file-fsyncs targeted a path ending in `suffix`.
    fn file_syncs_with_suffix(&self, suffix: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|(kind, path)| *kind == SyncKind::File && path.as_str().ends_with(suffix))
            .count()
    }
}

impl Syncer for RecordingSyncer {
    fn sync_file(&self, path: &Utf8Path) -> Result<(), std::io::Error> {
        self.calls
            .borrow_mut()
            .push((SyncKind::File, path.to_owned()));
        // Write handle (no truncation): Windows FlushFileBuffers needs
        // write access, mirroring OsSyncer.
        let file = fs_err::OpenOptions::new().write(true).open(path)?;
        file.sync_all()
    }

    fn sync_dir(&self, path: &Utf8Path) -> Result<(), std::io::Error> {
        self.calls
            .borrow_mut()
            .push((SyncKind::Dir, path.to_owned()));
        // Best-effort real dir fsync; on Windows opening a dir handle may
        // fail and that is fine (matches OsSyncer's platform handling).
        if let Ok(dir) = fs_err::File::open(path) {
            drop(dir.sync_all());
        }
        Ok(())
    }
}

fn journal_dir(temp: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(temp.path().join("journal")).expect("temp path must be UTF-8")
}

fn sample_record() -> ApplyRecord {
    ApplyRecord::new(
        LastApply {
            at: "2026-05-28T12:00:00Z".to_owned(),
            user: "u".to_owned(),
            host: "h".to_owned(),
        },
        Vec::new(),
    )
}

fn three_op_plan() -> Plan {
    Plan::new(vec![
        PlannedOperation::symlink("git/.gitconfig", "/home/u/.gitconfig", Disposition::Create),
        PlannedOperation::render("ssh/config.j2", "/home/u/.ssh/config", Disposition::Create),
        PlannedOperation::copy("bin/hello", "/home/u/.local/bin/hello", Disposition::Create),
    ])
}

// A three-operation apply records exactly one fsync on the plan
// file, one on the journal parent directory, one on the COMMIT sentinel,
// and zero per-operation fsyncs on the progress file.
#[test]
fn three_op_apply_fsyncs_plan_dir_commit_but_never_progress() {
    let temp = TempDir::new().expect("create tempdir");
    let dir = journal_dir(&temp);
    let syncer = RecordingSyncer::default();

    let plan = three_op_plan();
    let mut journal = Journal::flush_plan_and_fsync(&dir, "20260528T120000Z", &plan, &syncer)
        .expect("flush plan");

    // Drive the three progress records the way the executor loop will.
    for i in 0..plan.len() {
        journal
            .record_progress(u32::try_from(i).expect("index fits in u32"))
            .expect("record progress");
    }

    journal.commit(&sample_record(), &syncer).expect("commit");

    assert_eq!(
        syncer.file_syncs_with_suffix(PLAN_SUFFIX),
        1,
        "exactly one fsync on the plan file"
    );
    assert_eq!(
        syncer.file_syncs_with_suffix(COMMIT_SUFFIX),
        1,
        "exactly one fsync on the COMMIT sentinel"
    );
    assert_eq!(
        syncer.file_syncs_with_suffix(PROGRESS_SUFFIX),
        0,
        "zero per-operation fsyncs on the progress file"
    );

    let dir_syncs = syncer
        .calls()
        .into_iter()
        .filter(|(kind, _)| *kind == SyncKind::Dir)
        .count();
    assert_eq!(
        dir_syncs, 2,
        "the journal dir is fsync'd once after the plan and once after COMMIT"
    );

    // The progress file still exists during the run, but it must contain
    // three records and never have been fsync'd. After commit it is gone.
    assert!(
        !dir.join(format!("20260528T120000Z{PROGRESS_SUFFIX}"))
            .exists(),
        "progress file is deleted on commit"
    );
}

// Immediately after `flush_plan_and_fsync` returns and before
// the first mutation, the journal dir holds exactly one `<ts>.plan` file
// and no COMMIT sentinel. (The progress file exists but may be empty.)
#[test]
fn after_flush_plan_exists_with_no_commit_sentinel() {
    let temp = TempDir::new().expect("create tempdir");
    let dir = journal_dir(&temp);
    let syncer = RecordingSyncer::default();

    let plan = Plan::new(vec![PlannedOperation::symlink(
        "git/.gitconfig",
        "/home/u/.gitconfig",
        Disposition::Create,
    )]);
    // Simulate the crash window: flush, then drop the handle without
    // recording any progress or committing — as if SIGKILL'd here.
    let handle = Journal::flush_plan_and_fsync(&dir, "20260528T130000Z", &plan, &syncer)
        .expect("flush plan");
    drop(handle);

    let plan_files: Vec<_> = fs_err::read_dir(&dir)
        .expect("read journal dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(PLAN_SUFFIX))
        .collect();
    assert_eq!(
        plan_files,
        vec!["20260528T130000Z.plan".to_owned()],
        "exactly one .plan file after flush"
    );

    assert!(
        !dir.join(format!("20260528T130000Z{COMMIT_SUFFIX}"))
            .exists(),
        "no COMMIT sentinel exists in the crash window before commit"
    );

    // The plan bytes on disk decode back to the same plan — proving the
    // flush is durable, not a partial write.
    let bytes = fs_err::read(dir.join("20260528T130000Z.plan")).expect("read plan");
    assert_eq!(
        Plan::decode(&bytes).expect("decode plan"),
        plan,
        "the flushed plan round-trips through the on-disk bytes"
    );
}

// Version-envelope scenario: a plan whose envelope u16 is
// u16::MAX is refused on a binary compiled for major version 1, with a
// typed error naming both versions.
#[test]
fn newer_major_version_is_refused_with_both_versions_named() {
    // Build a byte buffer with a poisoned envelope: u16::MAX followed by
    // an otherwise-valid postcard body.
    let plan = Plan::new(vec![PlannedOperation::symlink(
        "a",
        "/b",
        Disposition::Create,
    )]);
    let mut bytes = plan.encode().expect("encode plan");
    // Overwrite the 2-byte little-endian version envelope at offset 0.
    let envelope = bytes
        .get_mut(..2)
        .expect("encoded plan has a 2-byte envelope");
    envelope.copy_from_slice(&u16::MAX.to_le_bytes());

    let err = Plan::decode(&bytes).expect_err("decode must refuse a newer major version");
    assert!(
        matches!(
            err,
            JournalError::VersionMismatch {
                found,
                supported,
            } if found == u16::MAX && supported == FILE_MAJOR_VERSION
        ),
        "expected VersionMismatch naming u16::MAX vs the compiled major, got {err:?}"
    );

    let rendered = JournalError::VersionMismatch {
        found: u16::MAX,
        supported: FILE_MAJOR_VERSION,
    }
    .to_string();
    assert!(
        rendered.contains(&u16::MAX.to_string())
            && rendered.contains(&FILE_MAJOR_VERSION.to_string()),
        "Display names both the found and supported versions: {rendered}"
    );
}

// Cleanup scenario: after a successful commit, the prior run's
// .plan and .progress are gone and only the COMMIT sentinel remains. A
// subsequent flush adds new plan files alongside the surviving sentinel.
#[test]
fn commit_deletes_plan_and_progress_leaving_only_commit_sentinel() {
    let temp = TempDir::new().expect("create tempdir");
    let dir = journal_dir(&temp);
    let syncer = RecordingSyncer::default();

    let plan = three_op_plan();
    let mut journal = Journal::flush_plan_and_fsync(&dir, "20260528T140000Z", &plan, &syncer)
        .expect("flush plan");
    for i in 0..plan.len() {
        journal
            .record_progress(u32::try_from(i).expect("index fits in u32"))
            .expect("record progress");
    }
    journal.commit(&sample_record(), &syncer).expect("commit");

    assert!(
        !dir.join(format!("20260528T140000Z{PLAN_SUFFIX}")).exists(),
        "plan file deleted after commit"
    );
    assert!(
        !dir.join(format!("20260528T140000Z{PROGRESS_SUFFIX}"))
            .exists(),
        "progress file deleted after commit"
    );
    assert!(
        dir.join(format!("20260528T140000Z{COMMIT_SUFFIX}"))
            .exists(),
        "COMMIT sentinel survives"
    );

    // A subsequent apply writes a fresh plan beside the surviving sentinel.
    let next = Journal::flush_plan_and_fsync(&dir, "20260528T150000Z", &plan, &syncer)
        .expect("flush second plan");
    assert!(
        dir.join(format!("20260528T150000Z{PLAN_SUFFIX}")).exists(),
        "new plan written alongside the old COMMIT sentinel"
    );
    assert!(
        dir.join(format!("20260528T140000Z{COMMIT_SUFFIX}"))
            .exists(),
        "the prior COMMIT sentinel is untouched by the new flush"
    );
    drop(next);
}

// The encoded plan is byte-identical for the same
// operations (the timestamp lives only in the filename, not the body).
#[test]
fn same_plan_encodes_to_identical_bytes() {
    let a = three_op_plan().encode().expect("encode a");
    let b = three_op_plan().encode().expect("encode b");
    assert_eq!(a, b, "identical plans encode to identical bytes");
}
