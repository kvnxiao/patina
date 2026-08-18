//! Integration tests for journal fsync shape.

#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

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
        let file = fs_err::OpenOptions::new().write(true).open(path)?;
        file.sync_all()
    }

    fn sync_dir(&self, path: &Utf8Path) -> Result<(), std::io::Error> {
        self.calls
            .borrow_mut()
            .push((SyncKind::Dir, path.to_owned()));
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

#[test]
fn three_op_apply_fsyncs_plan_dir_commit_but_never_progress() {
    let temp = TempDir::new().expect("create tempdir");
    let dir = journal_dir(&temp);
    let syncer = RecordingSyncer::default();

    let plan = three_op_plan();
    let mut journal = Journal::flush_plan_and_fsync(&dir, "20260528T120000Z", &plan, &syncer)
        .expect("flush plan");

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

    assert!(
        !dir.join(format!("20260528T120000Z{PROGRESS_SUFFIX}"))
            .exists(),
        "progress file is deleted on commit"
    );
}

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

    let bytes = fs_err::read(dir.join("20260528T130000Z.plan")).expect("read plan");
    assert_eq!(
        Plan::decode(&bytes).expect("decode plan"),
        plan,
        "the flushed plan round-trips through the on-disk bytes"
    );
}

#[test]
fn newer_major_version_is_refused_with_both_versions_named() {
    let plan = Plan::new(vec![PlannedOperation::symlink(
        "a",
        "/b",
        Disposition::Create,
    )]);
    let mut bytes = plan.encode().expect("encode plan");
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

#[test]
fn same_plan_encodes_to_identical_bytes() {
    let a = three_op_plan().encode().expect("encode a");
    let b = three_op_plan().encode().expect("encode b");
    assert_eq!(a, b, "identical plans encode to identical bytes");
}
