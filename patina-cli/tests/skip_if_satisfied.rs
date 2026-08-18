//! Integration tests for `skip_if_satisfied`.
#![expect(
    clippy::expect_used,
    reason = "Integration tests use .expect() for fixtures and asserted output outside #[cfg(test)] modules; allow-expect-in-tests does not cover integration-crate roots."
)]

mod common;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use std::time::SystemTime;

fn mtime(path: &Utf8Path) -> SystemTime {
    fs_err::symlink_metadata(path.as_std_path())
        .expect("stat target")
        .modified()
        .expect("mtime available")
}

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

fn engine_canonical(target: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(
        dunce::canonicalize(target.as_std_path()).expect("canonicalize target"),
    )
    .expect("canonical target is utf8")
}

#[test]
fn fully_satisfied_reapply_writes_no_new_journal_or_backup() {
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

    assert_eq!(
        code(&f.apply(&["--yes"])),
        0,
        "first apply must succeed and converge the repo"
    );

    let journal_dir = f.state_root().join("journal");
    let backups_dir = f.state_root().join("backups");
    let journal_before = entry_names(&journal_dir);
    let backups_before = entry_names(&backups_dir);

    let commit_path = sole_commit_file(&journal_dir);
    let commit_bytes_before =
        fs_err::read(commit_path.as_std_path()).expect("read commit bytes before");
    let commit_mtime_before = mtime(&commit_path);

    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "the no-op re-apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

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
fn fully_satisfied_apply_without_yes_skips_prompt_and_reports_up_to_date() {
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
fn unchanged_entry_is_not_rewritten_or_backed_up_while_drift_is() {
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

    fs_err::write(&b_out, b"b-drifted").expect("drift b_out");

    let second = f.apply(&["--yes"]);
    assert_eq!(
        code(&second),
        0,
        "second apply must succeed; stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

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

    assert_eq!(
        fs_err::read(b_out.as_std_path()).expect("read b_out"),
        b"b-bytes",
        "the drifted target is re-materialized to the source"
    );

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

    let cycle_ts = cycle.file_name().expect("cycle has a timestamp name");
    let backed_up = patina_core::journal::mirror_backup_path(
        &backups_root,
        cycle_ts,
        &engine_canonical(&b_out),
    );
    assert_eq!(
        fs_err::read(backed_up.as_std_path()).expect("read backed-up bytes"),
        b"b-drifted",
        "the backup must hold the prior (drifted) bytes, not the new source"
    );
}

#[test]
fn copy_tree_re_apply_restores_drift_and_backs_up_the_tree_as_a_unit() {
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

    fs_err::write(&one, b"tampered").expect("drift one.txt");

    assert_eq!(code(&f.apply(&["--yes"])), 0, "re-apply succeeds");

    assert_eq!(
        fs_err::read(one.as_std_path()).expect("read one"),
        b"one",
        "the drifted leaf is re-materialized to the source"
    );

    let backups_root = f.state_root().join("backups");
    let cycle = latest_backup_cycle(&backups_root).expect("a backup cycle for the drifted tree");
    let cycle_ts = cycle.file_name().expect("cycle has a timestamp name");
    let backed_up_one =
        patina_core::journal::mirror_backup_path(&backups_root, cycle_ts, &engine_canonical(&one));
    assert_eq!(
        fs_err::read(backed_up_one.as_std_path()).expect("read backed-up leaf"),
        b"tampered",
        "the whole-tree backup must capture the drifted leaf's prior bytes"
    );
}

#[test]
fn rollback_leaves_unchanged_deletes_create_and_restores_update() {
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

    fs_err::write(&unchanged_out, b"unchanged-bytes").expect("pre-stage unchanged_out");
    fs_err::write(&update_out, b"update-pre-apply").expect("pre-stage update_out");

    let apply = f.apply(&["--yes"]);
    assert_eq!(
        code(&apply),
        0,
        "apply must succeed and converge; stderr: {}",
        String::from_utf8_lossy(&apply.stderr)
    );

    assert_eq!(
        fs_err::read(create_out.as_std_path()).expect("read create_out post-apply"),
        b"create-bytes",
        "the Create target must be materialized by the apply"
    );

    let rollback = f.run(&["rollback", "--yes"], &[]);
    assert_eq!(
        code(&rollback),
        0,
        "rollback must succeed; stderr: {}",
        String::from_utf8_lossy(&rollback.stderr)
    );

    assert_eq!(
        fs_err::read(unchanged_out.as_std_path()).expect("read unchanged_out post-rollback"),
        b"unchanged-bytes",
        "the Unchanged target must be left byte-for-byte in place"
    );

    assert!(
        !create_out.as_std_path().exists(),
        "the Create target must be deleted by rollback"
    );

    assert_eq!(
        fs_err::read(update_out.as_std_path()).expect("read update_out post-rollback"),
        b"update-pre-apply",
        "the Update target must be restored to its pre-apply bytes"
    );
}

#[test]
fn unchanged_entry_is_recorded_clean_and_survives_reap() {
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

    fs_err::write(&drift_out, b"tampered").expect("drift drift_out");
    assert_eq!(code(&f.apply(&["--yes"])), 0, "re-apply succeeds");

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

    assert_eq!(code(&f.apply(&["--yes"])), 0, "third apply succeeds");
    assert_eq!(
        fs_err::read(keep_out.as_std_path()).expect("read keep_out"),
        b"keep",
        "the Unchanged target must survive a later apply's reap phase"
    );
}

fn plan_rows(stdout: &[u8]) -> Vec<serde_json::Value> {
    let doc: serde_json::Value =
        serde_json::from_slice(stdout).expect("apply --json stdout must be one JSON document");
    for key in ["repo_root", "profile", "plan", "result"] {
        assert!(
            doc.get(key).is_some(),
            "the --json envelope must carry the standard `{key}` field; got: {doc}"
        );
    }
    doc.get("plan")
        .and_then(serde_json::Value::as_array)
        .expect("the envelope must carry a `plan` array")
        .clone()
}

fn state_for(rows: &[serde_json::Value], basename: &str) -> String {
    rows.iter()
        .find(|row| {
            row.get("target")
                .and_then(serde_json::Value::as_str)
                .and_then(|t| Utf8Path::new(t).file_name())
                == Some(basename)
        })
        .and_then(|row| row.get("state"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .expect("a plan row whose target basename matches, carrying a string `state`")
}

#[test]
fn json_plan_entries_carry_their_disposition_state() {
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
    let update_out = f.home.join("update_out");

    fs_err::write(&unchanged_out, b"unchanged-bytes").expect("pre-stage unchanged_out");
    fs_err::write(&update_out, b"update-pre-apply").expect("pre-stage update_out");

    let first = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&first),
        0,
        "the mixed --json apply must succeed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let rows = plan_rows(&first.stdout);
    assert_eq!(
        state_for(&rows, "unchanged_out"),
        "unchanged",
        "the already-satisfied target must report state `unchanged`; rows: {rows:?}"
    );
    assert_eq!(
        state_for(&rows, "create_out"),
        "create",
        "the absent target must report state `create`; rows: {rows:?}"
    );
    assert_eq!(
        state_for(&rows, "update_out"),
        "update",
        "the drifted target must report state `update`; rows: {rows:?}"
    );

    let satisfied = f.apply(&["--json", "--yes"]);
    assert_eq!(code(&satisfied), 0, "the satisfying apply must succeed");
    let again = f.apply(&["--json", "--yes"]);
    assert_eq!(code(&again), 0, "the repeat apply must succeed");
    assert_eq!(
        satisfied.stdout,
        again.stdout,
        "two --json applies over the same live state must be byte-identical;\nfirst:  {}\nsecond: {}",
        String::from_utf8_lossy(&satisfied.stdout),
        String::from_utf8_lossy(&again.stdout),
    );
}

#[test]
fn fully_satisfied_json_emits_standard_envelope_all_unchanged() {
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

    assert_eq!(
        code(&f.apply(&["--json", "--yes"])),
        0,
        "priming apply must converge the repo"
    );

    let out = f.apply(&["--json", "--yes"]);
    assert_eq!(
        code(&out),
        0,
        "the fully-satisfied --json apply must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rows = plan_rows(&out.stdout);
    assert!(
        !rows.is_empty(),
        "the standard envelope still lists every managed entry, even on a no-op"
    );
    for row in &rows {
        assert_eq!(
            row.get("state").and_then(serde_json::Value::as_str),
            Some("unchanged"),
            "every entry of a fully-satisfied plan must be `unchanged` (zero change counts); row: {row}"
        );
    }
}
