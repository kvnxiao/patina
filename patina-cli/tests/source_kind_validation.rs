//! A managed entry's declared kind is
//! validated against its source's on-disk kind at plan time, before the
//! advisory lock, the journal flush, or any mutation.
//!
//! Each test drives `PATINA_REPO=<tempdir> patina apply --yes` over a
//! fixture repo whose module declares a `[[file]]` or `[[directory]]` entry,
//! and asserts that:
//!
//! - a `[[file]]` pointing at a directory source exits 1, names the source and
//!   `[[directory]]`, and writes no journal artifact;
//! - a `[[directory]]` pointing at a file source directs the author to
//!   `[[file]]`;
//! - a `when`-true entry whose source is absent exits 1 with a missing-source
//!   error and no journal artifact;
//! - a `when`-false entry whose source is absent and wrong-shaped on this OS
//!   exits 0 with no kind / missing-source error, because step (3) never runs
//!   on a gated-off entry.

mod common;

use common::Fixture;
use common::code;

/// The OS family string the engine's `patina.os` built-in resolves to on
/// this host. Matches `current_os_family` in `conditional_entries.rs`:
/// `std::env::consts::OS` is exactly the value the engine normalizes to on
/// the three supported platforms, so a `when` built from it is
/// deterministically true here.
fn current_os_family() -> &'static str {
    std::env::consts::OS
}

/// Assert that the apply wrote no `*.plan` or `*.COMMIT` journal file for the
/// run — the plan-time-failure guarantee that a mismatched entry mutates
/// nothing. The journal directory is `<state>/patina/journal`; it
/// may not exist at all on a plan-phase failure, which is itself proof that
/// nothing was flushed.
fn assert_no_journal_artifacts(f: &Fixture) {
    let journal = f.state_root().join("journal");
    let Ok(entries) = fs_err::read_dir(&journal) else {
        // No journal dir → nothing was ever flushed. That satisfies the
        // "no plan/COMMIT for the run" contract.
        return;
    };
    let artifacts: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            let path = std::path::Path::new(name);
            path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("plan") || ext.eq_ignore_ascii_case("COMMIT")
            })
        })
        .collect();
    assert!(
        artifacts.is_empty(),
        "a plan-time source-kind failure must write no journal plan/COMMIT, found: {artifacts:?}"
    );
}

#[test]
fn file_entry_with_directory_source_fails_and_directs_to_directory_table() {
    // A `[[file]]` whose source is a directory exits 1, stderr names
    // the source (`confdir`) and the `[[directory]]` table, and no journal
    // plan/COMMIT is written.
    let f = Fixture::new();
    let module = f.module(
        "shell",
        "[[file]]\nsource = \"confdir\"\ntarget = \"~/.config/app\"\n",
    );
    fs_err::create_dir(module.join("confdir")).expect("create directory source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "a `[[file]]` pointing at a directory source must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("confdir"),
        "stderr must name the offending source `confdir`, got: {stderr}"
    );
    assert!(
        stderr.contains("[[directory]]"),
        "stderr must direct the author to the `[[directory]]` table, got: {stderr}"
    );
    assert!(
        !f.home.join(".config/app").exists(),
        "a mismatched entry must materialize no target"
    );
    assert_no_journal_artifacts(&f);
}

#[test]
fn directory_entry_with_file_source_fails_and_directs_to_file_table() {
    // A `[[directory]]` whose source is a regular file exits 1 and
    // stderr directs the author to the `[[file]]` table.
    let f = Fixture::new();
    let module = f.module(
        "git",
        "[[directory]]\nsource = \"gitconfig\"\ntarget = \"~/.config/git\"\n",
    );
    fs_err::write(module.join("gitconfig"), "[user]\n").expect("write file source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "a `[[directory]]` pointing at a file source must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[[file]]"),
        "stderr must direct the author to the `[[file]]` table, got: {stderr}"
    );
    assert_no_journal_artifacts(&f);
}

#[test]
fn when_true_entry_with_absent_source_fails_as_source_not_found() {
    // A `[[file]]` with `source = "ghost"` and no `when` (so it is
    // not gated off) whose source is absent exits 1, stderr names `ghost` as
    // a missing source, and no journal plan/COMMIT is written.
    let f = Fixture::new();
    f.module(
        "shell",
        "[[file]]\nsource = \"ghost\"\ntarget = \"~/.ghostrc\"\n",
    );
    // Deliberately do not create `ghost` on disk.

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        1,
        "an entry whose source is absent must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ghost"),
        "stderr must name `ghost` as the missing source, got: {stderr}"
    );
    assert!(
        !f.home.join(".ghostrc").exists(),
        "a missing-source entry must materialize no target"
    );
    assert_no_journal_artifacts(&f);
}

#[test]
fn when_false_entry_with_absent_wrong_kind_source_is_not_validated() {
    // A `[[directory]]` entry gated off on this OS
    // (`when = "patina.os == 'definitely-not-this-os'"`) with an absent
    // source exits 0 with no missing-source or kind error — step (3) never
    // runs on a `when`-false entry (the ordering guarantee).
    let f = Fixture::new();
    f.module(
        "wm",
        "[[directory]]\nsource = \"only-on-other-os\"\ntarget = \"~/.config/wm\"\n\
         when = \"patina.os == 'definitely-not-this-os'\"\n",
    );
    // Deliberately do not create `only-on-other-os`: a gated-off entry must
    // never be canonicalized or kind-checked.

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a `when`-false entry with an absent source must not fail the apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("does not exist") && !stderr.contains("[[file]]"),
        "a gated-off entry must raise no missing-source or kind error, got: {stderr}"
    );
    assert!(
        !f.home.join(".config/wm").exists(),
        "a `when`-false entry must materialize no target"
    );
}

#[test]
fn when_true_entry_with_present_source_does_apply() {
    // A control alongside the wrong-OS case: an entry whose `when` is true on
    // this host and whose source exists with the matching kind applies
    // cleanly. Guards against `validate_source_kind` rejecting a valid entry.
    let f = Fixture::new();
    let when = format!("patina.os == '{}'", current_os_family());
    let module = f.module(
        "shell",
        &format!(
            "[[file]]\nsource = \"zshrc\"\ntarget = \"~/.zshrc\"\nmode = \"copy\"\nwhen = \"{when}\"\n"
        ),
    );
    fs_err::write(module.join("zshrc"), "export EDITOR=vim\n").expect("write source");

    let out = f.apply(&["--yes"]);

    assert_eq!(
        code(&out),
        0,
        "a `when`-true entry with a matching-kind present source must apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.home.join(".zshrc").exists(),
        "the entry's target must be materialized"
    );
}
