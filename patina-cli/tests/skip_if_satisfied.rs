#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixtures and asserted output; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Skip-if-satisfied execute behaviour (SPEC-0005, T-005 + T-006).
//!
//! ## Per-entry skip (T-005, REQ-003 / REQ-004; CHK-006 / CHK-007)
//!
//! A re-apply over a partially-drifted repo must leave the already-satisfied
//! (`Unchanged`) entry completely untouched — same inode/mtime, no backup
//! entry — while mutating the drifted entry and backing up its prior bytes
//! (CHK-006). The skipped entry must still be recorded in the commit so
//! `patina status` reports it `Clean` and a later reap never removes it
//! (CHK-007).
//!
//! ## Full-no-op short-circuit (T-006, REQ-007 / REQ-009; CHK-011 / CHK-013)
//!
//! When *every* entry is `Unchanged` and there is nothing to reap, the whole
//! apply is a full no-op: it writes no new journal record and creates no new
//! backup cycle, leaving the prior commit authoritative (CHK-011, REQ-007),
//! and it skips the diff-and-prompt confirmation entirely — presenting no
//! prompt and reading no stdin (CHK-013, REQ-009). The interactive prompt-skip
//! itself is unit-tested in `patina-cli/src/cmd/apply.rs` (the subprocess
//! fixture here pins stdin to a non-TTY); this suite covers the no-write
//! property over the real engine.
//!
//! ## Rollback fidelity for a mixed commit (T-008, REQ-006; CHK-010)
//!
//! A committed apply records one disposition per target. `patina rollback`
//! must honour those dispositions: an `Unchanged` target (which took no
//! backup) is left byte-for-byte in place, a `Create` target is deleted, and
//! an `Update` target is restored to its pre-apply bytes from the backup.

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use std::time::SystemTime;

/// The modification time of a regular file, as a `SystemTime`. Used to prove
/// an `Unchanged` target was not rewritten across a re-apply.
fn mtime(path: &Utf8Path) -> SystemTime {
    fs_err::symlink_metadata(path.as_std_path())
        .expect("stat target")
        .modified()
        .expect("mtime available")
}

/// Recursively collect the basenames of every regular file under `root`.
/// Returns an empty vector when `root` does not exist (no backup cycle was
/// written at all).
fn file_names_under(root: &Utf8Path) -> Vec<String> {
    let mut names = Vec::new();
    collect_names(root, &mut names);
    names
}

fn collect_names(dir: &Utf8Path, names: &mut Vec<String>) {
    let Ok(entries) = fs_err::read_dir(dir.as_std_path()) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("read backup dir entry");
        let path = Utf8PathBuf::from_path_buf(entry.path()).expect("utf8 backup path");
        let file_type = entry.file_type().expect("backup entry file type");
        if file_type.is_dir() {
            collect_names(&path, names);
        } else if let Some(name) = path.file_name() {
            names.push(name.to_owned());
        }
    }
}

/// The sorted basenames of the immediate entries under `dir` (files and
/// subdirectories), or an empty vector when `dir` does not exist. Used to
/// snapshot the journal and backups directories across a re-apply so a no-op
/// run can be shown to add nothing (REQ-007).
fn entry_names(dir: &Utf8Path) -> Vec<String> {
    let Ok(entries) = fs_err::read_dir(dir.as_std_path()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| {
            e.expect("read dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The newest timestamped backup-cycle directory under `<state>/backups`, or
/// `None` when no backup cycle exists.
fn latest_backup_cycle(backups_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let entries = fs_err::read_dir(backups_root.as_std_path()).ok()?;
    let mut cycles: Vec<Utf8PathBuf> = entries
        .filter_map(|e| {
            let e = e.expect("read backups root entry");
            let path = Utf8PathBuf::from_path_buf(e.path()).expect("utf8 cycle path");
            e.file_type()
                .expect("cycle file type")
                .is_dir()
                .then_some(path)
        })
        .collect();
    cycles.sort();
    cycles.pop()
}

/// The single committed journal record `<ts>.COMMIT` under `journal_dir`,
/// or a panic if there is not exactly one. A no-op re-apply must overwrite
/// none of it.
fn sole_commit_file(journal_dir: &Utf8Path) -> Utf8PathBuf {
    let mut commits: Vec<Utf8PathBuf> = fs_err::read_dir(journal_dir.as_std_path())
        .expect("read journal dir")
        .map(|e| {
            Utf8PathBuf::from_path_buf(e.expect("journal entry").path()).expect("utf8 journal path")
        })
        .filter(|p| p.extension() == Some("COMMIT"))
        .collect();
    assert_eq!(
        commits.len(),
        1,
        "expected exactly one committed record, found {commits:?}"
    );
    commits.pop().expect("one commit file")
}

#[test]
fn fully_satisfied_reapply_writes_no_new_journal_or_backup(/* CHK-011, REQ-007 */) {
    // After a converging first apply, a second apply over the unchanged source
    // is a full no-op: it must add no new `*.plan` / `*.COMMIT` to the journal
    // directory, must not rewrite the existing `<ts>.COMMIT`, and must create
    // no new backup-cycle directory. The prior commit stays the single
    // authoritative record.
    //
    // A basename-set compare alone is NOT enough: `current_timestamp()` is
    // second-resolution, so two back-to-back applies usually share a `<ts>`,
    // and a full write cycle would overwrite `<ts>.COMMIT` in place — leaving
    // the basename set identical. The collision-proof signal is the commit
    // file's own identity (mtime + bytes): a write cycle changes at least one,
    // so disabling the no-op short-circuit turns this test red regardless of
    // whether the two applies land in the same wall-clock second.
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[file]]
source = "a_src"
target = "~/a_out"
mode = "copy"

[[file]]
source = "rc.tmpl"
target = "~/.rc"
"#,
    );
    fs_err::write(m.join("a_src"), b"a-bytes").expect("write a_src");
    fs_err::write(m.join("rc.tmpl"), b"export EDITOR=vim\n").expect("write rc.tmpl");

    // First apply converges the repo and writes the authoritative commit.
    assert_eq!(
        code(&f.apply(&["--yes"])),
        0,
        "first apply must succeed and converge the repo"
    );

    let journal_dir = f.state_root().join("journal");
    let backups_dir = f.state_root().join("backups");
    let journal_before = entry_names(&journal_dir);
    let backups_before = entry_names(&backups_dir);

    // Capture the authoritative commit's collision-proof identity: its bytes
    // and its mtime. A full plan→commit write cycle would rewrite this file
    // (changing the bytes and/or the mtime) even when the new timestamp
    // collides with the old basename.
    let commit_path = sole_commit_file(&journal_dir);
    let commit_bytes_before =
        fs_err::read(commit_path.as_std_path()).expect("read commit bytes before");
    let commit_mtime_before = mtime(&commit_path);

    // Second apply over the unchanged source is the full no-op.
    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "the no-op re-apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // The journal directory gained no `*.plan` / `*.COMMIT` entry, and the
    // backups directory gained no new cycle (REQ-007). Comparing the full
    // entry set — not just counts — also catches a stray `.progress` file.
    // (Necessary but, on its own, collision-blind; the identity asserts below
    // are what make removing the feature turn this test red.)
    assert_eq!(
        entry_names(&journal_dir),
        journal_before,
        "a no-op re-apply must add no journal entry (no new .plan / .COMMIT)"
    );
    assert_eq!(
        entry_names(&backups_dir),
        backups_before,
        "a no-op re-apply must add no new backup cycle"
    );

    // The collision-proof core: the sole `<ts>.COMMIT` was neither rewritten
    // nor replaced. Its path, bytes, and mtime are all unchanged, so a full
    // write cycle (which deletes/rewrites the commit) cannot pass even when
    // the second apply lands in the same wall-clock second.
    let commit_path_after = sole_commit_file(&journal_dir);
    assert_eq!(
        commit_path_after, commit_path,
        "the no-op re-apply must not replace the committed record with a new one"
    );
    assert_eq!(
        fs_err::read(commit_path_after.as_std_path()).expect("read commit bytes after"),
        commit_bytes_before,
        "the no-op re-apply must not rewrite the committed record's bytes"
    );
    assert_eq!(
        mtime(&commit_path_after),
        commit_mtime_before,
        "the no-op re-apply must not touch the committed record's mtime"
    );
}

#[test]
fn fully_satisfied_apply_without_yes_skips_prompt_and_reports_up_to_date(/* CHK-013, REQ-009 */) {
    // A fully-satisfied repo applied WITHOUT `--yes` must short-circuit before
    // the diff-and-prompt branch: it prints the deterministic up-to-date line
    // and completes exit 0 without reading stdin and without rendering a diff.
    // The no-op branch precedes the `(yes, tty)` prompt decision in the human
    // path, so neither the prompt nor a stdin read is ever reached — feeding a
    // decline answer on stdin therefore changes nothing.
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[file]]
source = "a_src"
target = "~/a_out"
mode = "copy"
"#,
    );
    fs_err::write(m.join("a_src"), b"a-bytes").expect("write a_src");

    assert_eq!(
        code(&f.apply(&["--yes"])),
        0,
        "first apply must succeed and converge the repo"
    );

    // Re-apply without `--yes`. If the no-op short-circuit did NOT fire, the
    // human path would either preview the diff (non-interactive) or prompt —
    // both of which omit the up-to-date line and emit a diff body. Asserting
    // the up-to-date line is present and no `Apply?` prompt text reached
    // stderr proves the prompt/stdin branch was skipped entirely.
    let out = f.apply(&[]);
    assert_eq!(
        code(&out),
        0,
        "the no-op apply must complete exit 0 without a prompt; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("Already up to date"),
        "a no-op apply must print the up-to-date line, got stdout: {stdout}"
    );
    assert!(
        !stderr.contains("Apply?"),
        "a no-op apply must not emit the confirmation prompt, got stderr: {stderr}"
    );
}

#[test]
fn unchanged_entry_is_not_rewritten_or_backed_up_while_drift_is(/* CHK-006 */) {
    // Two `copy` entries. After the first apply both targets match their
    // source. We then drift exactly one (`b`) and re-apply: `a` must be left
    // byte-for-byte with its original mtime and contribute no backup entry,
    // while `b` is rewritten and its prior (drifted) bytes are backed up.
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[file]]
source = "a_src"
target = "~/a_out"
mode = "copy"

[[file]]
source = "b_src"
target = "~/b_out"
mode = "copy"
"#,
    );
    fs_err::write(m.join("a_src"), b"a-bytes").expect("write a_src");
    fs_err::write(m.join("b_src"), b"b-bytes").expect("write b_src");

    let first = f.apply(&["--yes"]);
    assert_eq!(
        code(&first),
        0,
        "first apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let a_out = f.home.join("a_out");
    let b_out = f.home.join("b_out");
    let a_mtime_before = mtime(&a_out);

    // Drift only `b_out` so the second apply classifies `a` Unchanged and `b`
    // Update.
    fs_err::write(&b_out, b"b-drifted").expect("drift b_out");

    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "second apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // The Unchanged entry's mtime is preserved: it was neither removed nor
    // rewritten (REQ-003).
    assert_eq!(
        mtime(&a_out),
        a_mtime_before,
        "the Unchanged target must not be rewritten across the re-apply"
    );
    assert_eq!(
        fs_err::read(a_out.as_std_path()).expect("read a_out"),
        b"a-bytes",
        "the Unchanged target keeps its bytes"
    );

    // The drifted entry is updated back to the source bytes.
    assert_eq!(
        fs_err::read(b_out.as_std_path()).expect("read b_out"),
        b"b-bytes",
        "the drifted target is re-materialized to the source"
    );

    // The second run's backup cycle holds the drifted target's prior bytes but
    // no entry for the Unchanged target (REQ-003).
    let backups_root = f.state_root().join("backups");
    let cycle = latest_backup_cycle(&backups_root).expect("a backup cycle for the Update");
    let names = file_names_under(&cycle);
    assert!(
        names.iter().any(|n| n == "b_out"),
        "the drifted target's prior bytes must be backed up; found {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "a_out"),
        "the Unchanged target must produce no backup entry; found {names:?}"
    );

    // The backed-up bytes are the pre-overwrite (drifted) bytes, located at
    // the same mirrored path crash recovery reads. Reusing the production
    // mapping keeps the assertion exact across platforms rather than guessing
    // the on-disk layout.
    let cycle_ts = cycle.file_name().expect("cycle has a timestamp name");
    let backed_up = patina_core::journal::mirror_backup_path(&backups_root, cycle_ts, &b_out);
    assert_eq!(
        fs_err::read(backed_up.as_std_path()).expect("read backed-up bytes"),
        b"b-drifted",
        "the backup must hold the prior (drifted) bytes, not the new source"
    );
}

#[test]
fn copy_tree_re_apply_restores_drift_and_backs_up_the_tree_as_a_unit() {
    // DEC-007 tree path through the real engine: a `copy-tree` with three
    // leaves, one drifted out of band, re-applies to restore the drifted leaf.
    // The whole target directory is backed up as a unit (today's
    // `backup_before_overwrite(<dir>)` model), so every leaf's prior bytes —
    // including the clean ones — land in the backup cycle. (The per-leaf
    // write-skip itself — that a clean leaf's link/file is not re-created — is
    // asserted exactly by the `copy_tree` / `tree_symlink` executor unit
    // tests, which observe that an unselected leaf is never written.)
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[directory]]
source = "tree_src"
target = "~/tree_out"
mode = "copy"
"#,
    );
    let src = m.join("tree_src");
    fs_err::create_dir_all(&src).expect("mkdir tree_src");
    fs_err::write(src.join("one.txt"), b"one").expect("write one");
    fs_err::write(src.join("two.txt"), b"two").expect("write two");
    fs_err::write(src.join("three.txt"), b"three").expect("write three");

    assert_eq!(code(&f.apply(&["--yes"])), 0, "first apply succeeds");

    let out = f.home.join("tree_out");
    let one = out.join("one.txt");

    // Drift exactly one leaf out of band.
    fs_err::write(&one, b"tampered").expect("drift one.txt");

    assert_eq!(code(&f.apply(&["--yes"])), 0, "re-apply succeeds");

    // The drifted leaf is restored to the source bytes.
    assert_eq!(
        fs_err::read(one.as_std_path()).expect("read one"),
        b"one",
        "the drifted leaf is re-materialized to the source"
    );

    // The re-apply backed up the target directory as a unit, so the backup
    // cycle holds the drifted leaf's prior bytes. The pre-drift bytes are the
    // ground truth recovery/rollback restores from.
    let backups_root = f.state_root().join("backups");
    let cycle = latest_backup_cycle(&backups_root).expect("a backup cycle for the drifted tree");
    let cycle_ts = cycle.file_name().expect("cycle has a timestamp name");
    let backed_up_one = patina_core::journal::mirror_backup_path(&backups_root, cycle_ts, &one);
    assert_eq!(
        fs_err::read(backed_up_one.as_std_path()).expect("read backed-up leaf"),
        b"tampered",
        "the whole-tree backup must capture the drifted leaf's prior bytes"
    );
}

#[test]
fn rollback_leaves_unchanged_deletes_create_and_restores_update(/* CHK-010, REQ-006 */) {
    // One committed apply produces all three dispositions in a single commit,
    // then `patina rollback` must honour each:
    //   - `unchanged`: the target already matched its source before the apply, so
    //     the apply took no backup; rollback must leave it byte-for-byte in place
    //     (NOT delete it as a no-backup fresh creation would be).
    //   - `create`: the target was absent before the apply; rollback deletes it.
    //   - `update`: the target existed with different bytes before the apply, so
    //     the apply backed it up; rollback restores the pre-apply bytes.
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[file]]
source = "unchanged_src"
target = "~/unchanged_out"
mode = "copy"

[[file]]
source = "create_src"
target = "~/create_out"
mode = "copy"

[[file]]
source = "update_src"
target = "~/update_out"
mode = "copy"
"#,
    );
    fs_err::write(m.join("unchanged_src"), b"unchanged-bytes").expect("write unchanged_src");
    fs_err::write(m.join("create_src"), b"create-bytes").expect("write create_src");
    fs_err::write(m.join("update_src"), b"update-bytes").expect("write update_src");

    let unchanged_out = f.home.join("unchanged_out");
    let create_out = f.home.join("create_out");
    let update_out = f.home.join("update_out");

    // Pre-stage the live filesystem so the single apply classifies one of each:
    //   - unchanged_out already holds the source bytes → Unchanged (no backup).
    //   - update_out exists with different bytes → Update (backed up).
    //   - create_out is absent → Create.
    fs_err::write(&unchanged_out, b"unchanged-bytes").expect("pre-stage unchanged_out");
    fs_err::write(&update_out, b"update-pre-apply").expect("pre-stage update_out");

    let apply = f.apply(&["--yes"]);
    assert_eq!(
        code(&apply),
        0,
        "apply must succeed and converge; stderr: {}",
        String::from_utf8_lossy(&apply.stderr)
    );

    // Sanity: after the apply, all three targets hold the source bytes.
    assert_eq!(
        fs_err::read(create_out.as_std_path()).expect("read create_out post-apply"),
        b"create-bytes",
        "the Create target must be materialized by the apply"
    );

    // `--yes` is required: a non-interactive `rollback` without it only
    // previews and performs no mutation (it would exit 0 having changed
    // nothing, masking the behaviour under test).
    let rollback = f.run(&["rollback", "--yes"], &[]);
    assert_eq!(
        code(&rollback),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rollback.stderr)
    );

    // The Unchanged target is left byte-for-byte in place. Had rollback used
    // the naive no-backup → delete rule, this target would be gone.
    assert_eq!(
        fs_err::read(unchanged_out.as_std_path()).expect("read unchanged_out post-rollback"),
        b"unchanged-bytes",
        "the Unchanged target must be left byte-for-byte in place"
    );

    // The Create target is deleted.
    assert!(
        !create_out.as_std_path().exists(),
        "the Create target must be deleted by rollback"
    );

    // The Update target is restored to its pre-apply bytes.
    assert_eq!(
        fs_err::read(update_out.as_std_path()).expect("read update_out post-rollback"),
        b"update-pre-apply",
        "the Update target must be restored to its pre-apply bytes"
    );
}

#[test]
fn unchanged_entry_is_recorded_clean_and_survives_reap(/* CHK-007 */) {
    // After a re-apply where one entry is Update and another stays Unchanged,
    // `patina status` must report the Unchanged entry's target `Clean` and
    // present in the output, and a subsequent apply must not reap it.
    let f = Fixture::new();
    let m = f.module(
        "m",
        r#"
[[file]]
source = "keep_src"
target = "~/keep_out"
mode = "copy"

[[file]]
source = "drift_src"
target = "~/drift_out"
mode = "copy"
"#,
    );
    fs_err::write(m.join("keep_src"), b"keep").expect("write keep_src");
    fs_err::write(m.join("drift_src"), b"drift").expect("write drift_src");

    assert_eq!(code(&f.apply(&["--yes"])), 0, "first apply succeeds");

    let keep_out = f.home.join("keep_out");
    let drift_out = f.home.join("drift_out");

    // Drift only `drift_out`, then re-apply (keep_out classifies Unchanged).
    fs_err::write(&drift_out, b"tampered").expect("drift drift_out");
    assert_eq!(code(&f.apply(&["--yes"])), 0, "re-apply succeeds");

    // `patina status` reports the Unchanged target Clean and present.
    let status = f.run(&["status"], &[]);
    assert_eq!(
        code(&status),
        0,
        "status must succeed; stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("keep_out"),
        "the Unchanged target must appear in status output: {stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("clean"),
        "the Unchanged target must be reported Clean: {stdout}"
    );

    // A subsequent apply must not reap the Unchanged target: it is still on
    // disk afterward with its bytes intact.
    assert_eq!(code(&f.apply(&["--yes"])), 0, "third apply succeeds");
    assert_eq!(
        fs_err::read(keep_out.as_std_path()).expect("read keep_out"),
        b"keep",
        "the Unchanged target must survive a later apply's reap phase"
    );
}
