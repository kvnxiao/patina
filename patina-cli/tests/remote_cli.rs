#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! The `patina remote` command group, end to end.
//!
//! Origins are throwaway git repositories on the local filesystem, so the suite
//! exercises the real `git` plumbing with no network. Fixtures set
//! `min_age = "0s"` where a test needs a bump to be eligible immediately; the
//! gate's own arithmetic is unit-tested in `patina_core::remote::gate`.
//!
//! See `docs/REMOTE_SOURCES.md` "Commands", "The update gate", and
//! "Shell integration".

mod common;

use common::Fixture;
use common::Origin;
use common::code;
use common::git_in;

/// A committer epoch far enough in the past that any age floor is satisfied.
const OLD_EPOCH: i64 = 1_700_000_000;

/// Declare a remote in the root manifest, optionally with a per-remote
/// `min_age`, plus a module entry that sources from it.
fn declare(f: &Fixture, name: &str, origin: &Origin, min_age: Option<&str>) {
    declare_only(f, name, origin, min_age);
    f.module(
        name,
        &format!(
            "[[file]]\nsource = \"a.md\"\nremote = \"{name}\"\ntarget = \"~/.a.md\"\n\
             mode = \"copy\"\n"
        ),
    );
}

/// Declare a remote in the root manifest without any entry selecting it.
fn declare_only(f: &Fixture, name: &str, origin: &Origin, min_age: Option<&str>) {
    f.declare_remote(name, &origin.url(), Some("main"));
    let Some(value) = min_age else {
        return;
    };
    let manifest = f.root.join("patina.toml");
    let existing = fs_err::read_to_string(manifest.as_std_path()).expect("read root manifest");
    fs_err::write(
        manifest.as_std_path(),
        format!("{existing}min_age = \"{value}\"\n"),
    )
    .expect("write root manifest");
}

/// A validated remote name, for the cache paths the assertions read.
fn remote_name(spelling: &str) -> patina_core::RemoteName {
    patina_core::RemoteName::parse(spelling).expect("a legal remote name")
}

fn lockfile(f: &Fixture) -> String {
    fs_err::read_to_string(f.root.join("patina.lock").as_std_path()).unwrap_or_default()
}

fn json_of(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("stdout must be a single JSON document")
}

#[test]
fn update_creates_the_first_pin_without_waiting_out_the_age_gate() {
    // The commit is made "now" and the floor is the default 72 hours, so only
    // the first-pin exemption can let this through.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    let now = patina_core::current_epoch_seconds();
    let rev = origin.commit_files(&[("a.md", "first\n")], now);
    declare(&f, "humanizer", &origin, None);

    let out = f.run(&["remote", "update", "--json"], &[]);
    assert_eq!(
        code(&out),
        0,
        "the first pin must be created; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock = lockfile(&f);
    assert!(
        lock.contains(&rev) && lock.contains("[remotes.humanizer]"),
        "the lockfile must record the first pin:\n{lock}"
    );
    let doc = json_of(&out);
    assert_eq!(
        doc.pointer("/remotes/0/action")
            .and_then(serde_json::Value::as_str),
        Some("updated"),
        "the envelope must report the pin as bumped: {doc}"
    );
    assert_eq!(
        doc.pointer("/remotes/0/rev")
            .and_then(serde_json::Value::as_str),
        Some(rev.as_str())
    );
}

#[test]
fn update_is_a_no_op_when_the_pin_is_already_the_upstream_tip() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");
    let before = lockfile(&f);

    let out = f.run(&["remote", "update"], &[]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("already at"),
        "an up-to-date remote must say so: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        lockfile(&f),
        before,
        "an up-to-date remote must not rewrite the lockfile"
    );
}

#[test]
fn a_candidate_inside_its_cooldown_window_is_held_and_reports_when_it_is_eligible() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("7d"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");
    let pinned = lockfile(&f);

    // A brand-new second commit cannot be a week old.
    origin.commit_files(
        &[("a.md", "second\n")],
        patina_core::current_epoch_seconds(),
    );
    let out = f.run(&["remote", "update"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(code(&out), 0, "a cooldown is not a failure");
    assert!(
        stdout.contains("holding") && stdout.contains("min_age"),
        "the cooldown must report why and until when: {stdout}"
    );
    assert_eq!(
        lockfile(&f),
        pinned,
        "a held candidate must leave the pin unchanged"
    );
}

#[test]
fn now_bypasses_the_age_gate_and_warns() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("7d"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");

    let second = origin.commit_files(
        &[("a.md", "second\n")],
        patina_core::current_epoch_seconds(),
    );
    let out = f.run(&["remote", "update", "--now"], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(code(&out), 0);
    assert!(
        stderr.contains("--now` bypasses the age gate"),
        "the bypass must be visible: {stderr}"
    );
    assert!(
        lockfile(&f).contains(&second),
        "`--now` must let the young candidate through:\n{}",
        lockfile(&f)
    );
}

#[test]
fn a_rewritten_history_is_not_bumped_without_confirmation() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");
    let pinned = lockfile(&f);

    // Replace `main` with an unrelated root commit: the shape a force-push
    // leaves behind.
    git_in(
        &origin.dir,
        OLD_EPOCH,
        &["checkout", "--quiet", "--orphan", "rewritten"],
    );
    let rewritten = origin.commit_files(&[("a.md", "rewritten\n")], OLD_EPOCH);
    git_in(&origin.dir, OLD_EPOCH, &["branch", "-M", "main"]);

    // The subprocess has no TTY, so the confirmation cannot be answered.
    let out = f.run(&["remote", "update"], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("history was rewritten"),
        "the rewrite must be reported: {stderr}"
    );
    assert_eq!(
        lockfile(&f),
        pinned,
        "a rewrite must not be bumped without confirmation"
    );

    let confirmed = f.run(&["remote", "update", "--yes"], &[]);
    assert_eq!(code(&confirmed), 0);
    assert!(
        lockfile(&f).contains(&rewritten),
        "--yes must accept the flagged bump:\n{}",
        lockfile(&f)
    );
}

#[test]
fn update_of_an_unknown_remote_name_fails_without_touching_the_lockfile() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let out = f.run(&["remote", "update", "nope"], &[]);
    assert_eq!(code(&out), 1);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nope"),
        "the message must include the remote that was asked for"
    );
    assert_eq!(lockfile(&f), "", "no lockfile may be written");
}

#[test]
fn list_reports_each_remote_and_its_pin() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let unpinned = f.run(&["remote", "list", "--json"], &[]);
    let doc = json_of(&unpinned);
    let row = doc.pointer("/remotes/0").expect("one remote row").clone();
    assert_eq!(
        row.get("name").and_then(serde_json::Value::as_str),
        Some("humanizer")
    );
    assert!(
        row.get("rev").is_some_and(serde_json::Value::is_null),
        "an unpinned remote must report a null rev: {row}"
    );

    assert_eq!(
        code(&f.run(&["remote", "update"], &[])),
        0,
        "create the pin"
    );
    let pinned = json_of(&f.run(&["remote", "list", "--json"], &[]));
    assert!(
        pinned
            .pointer("/remotes/0/rev")
            .is_some_and(|rev| rev.as_str().is_some_and(|s| s.len() == 40)),
        "a pinned remote must report its full rev: {pinned}"
    );
}

#[test]
fn list_says_so_when_no_remote_is_declared() {
    let f = Fixture::new();
    let out = f.run(&["remote", "list"], &[]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("No remotes are declared"),
        "a repository with no remotes must say so, not print nothing"
    );
}

#[test]
fn check_reports_a_moved_upstream_and_writes_the_notice() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");

    let clean = f.run(&["remote", "check"], &[]);
    assert_eq!(code(&clean), 0);
    assert!(
        String::from_utf8_lossy(&clean.stdout).contains("at its pinned rev"),
        "a converged repository must report no pending update: {}",
        String::from_utf8_lossy(&clean.stdout)
    );
    let notice = patina_core::remote::cache::notice_path(&f.state_root());
    assert!(
        !notice.exists(),
        "nothing pending must leave no notice for the shell to print"
    );

    origin.commit_files(&[("a.md", "second\n")], OLD_EPOCH + 60);
    let behind = f.run(&["remote", "check"], &[]);
    assert_eq!(code(&behind), 0);
    let body = fs_err::read_to_string(notice.as_std_path()).expect("the notice is written");
    assert!(
        body.contains("humanizer") && body.contains("patina apply --update"),
        "the notice must include the remote and the next step: {body}"
    );
    assert!(
        patina_core::remote::notice::read_pending(&f.state_root()).contains("humanizer"),
        "the machine-readable pending set must agree with the notice"
    );
}

#[test]
fn a_successful_update_clears_the_pending_notice() {
    // Only `remote check` otherwise rewrites the notice files, and its hook
    // form self-throttles for a day, so a bump that leaves them in place keeps
    // announcing an update the user already accepted.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");

    origin.commit_files(&[("a.md", "second\n")], OLD_EPOCH + 60);
    assert_eq!(code(&f.run(&["remote", "check"], &[])), 0);
    assert!(
        patina_core::remote::notice::read_pending(&f.state_root()).contains("humanizer"),
        "the fixture must start with a pending update"
    );

    // `--yes`: the fixture commit is backdated against the pin's fresh
    // `updated_at`, so the gate flags it and a bare update would hold.
    assert_eq!(code(&f.run(&["remote", "update", "--yes"], &[])), 0);

    assert!(
        patina_core::remote::notice::read_pending(&f.state_root()).is_empty(),
        "the pending set must be cleared by the bump"
    );
    assert!(
        !patina_core::remote::cache::notice_path(&f.state_root()).exists(),
        "the shell notice must be cleared by the bump"
    );
}

#[test]
fn check_json_reports_the_pending_set() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let out = f.run(&["remote", "check", "--json"], &[]);
    assert_eq!(code(&out), 0);
    let doc = json_of(&out);
    assert_eq!(
        doc.pointer("/pending/0")
            .and_then(serde_json::Value::as_str),
        Some("humanizer"),
        "an unpinned remote counts as having an update: {doc}"
    );
}

#[test]
fn check_hook_is_silent_and_self_throttles() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let first = f.run(&["remote", "check", "--hook"], &[]);
    assert_eq!(code(&first), 0);
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        "",
        "the hook must print nothing on stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&first.stderr),
        "",
        "the hook must print nothing on stderr"
    );
    let stamp = patina_core::remote::notice::last_check_epoch(&f.state_root());
    assert!(stamp.is_some(), "the hook must stamp its check");

    // Inside the throttle window the second run skips the check, and the stamp
    // stays untouched.
    assert_eq!(code(&f.run(&["remote", "check", "--hook"], &[])), 0);
    assert_eq!(
        patina_core::remote::notice::last_check_epoch(&f.state_root()),
        stamp,
        "a throttled hook run must not re-stamp"
    );
}

#[test]
fn prune_removes_an_unreferenced_checkout() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let orphan = patina_core::remote::cache::checkout_dir(
        &f.state_root(),
        &remote_name("humanizer"),
        "cccccccccccccccccccccccccccccccccccccccc",
    );
    fs_err::create_dir_all(orphan.as_std_path()).expect("mkdir orphan checkout");

    let out = f.run(&["remote", "prune", "--json"], &[]);
    assert_eq!(code(&out), 0);
    assert!(
        !orphan.exists(),
        "the unreferenced checkout must be removed"
    );
    let doc = json_of(&out);
    assert_eq!(
        doc.pointer("/removed")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1),
        "the envelope must report exactly what was removed: {doc}"
    );
}

#[test]
fn prune_removes_the_whole_cache_tree_of_an_undeclared_remote() {
    // The reachability sweep only ever considers checkout directories, so a
    // remote's own directory and its bare fetch repository would survive
    // deleting the declaration for good.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");
    let cached = patina_core::remote::cache::module_dir(&f.state_root(), &remote_name("humanizer"));
    assert!(
        cached.join("repo.git").is_dir(),
        "the update must have filled the fetch repository at {cached}"
    );

    fs_err::write(
        f.root.join("patina.toml").as_std_path(),
        "[patina]\nroot = true\n",
    )
    .expect("rewrite the root manifest");
    fs_err::remove_dir_all(f.root.join("humanizer").as_std_path()).expect("remove the module");

    let out = f.run(&["remote", "prune"], &[]);
    assert_eq!(
        code(&out),
        0,
        "prune must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !cached.exists(),
        "an undeclared remote's whole cache tree must be removed"
    );
}

#[test]
fn a_mutating_apply_drops_a_pin_no_declaration_selects() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.apply(&["--update", "--yes"])), 0, "priming run");
    assert!(lockfile(&f).contains("[remotes.humanizer]"), "pin recorded");

    fs_err::write(
        f.root.join("patina.toml").as_std_path(),
        "[patina]\nroot = true\n",
    )
    .expect("rewrite the root manifest");
    fs_err::remove_dir_all(f.root.join("humanizer").as_std_path()).expect("remove the module");

    let out = f.apply(&["--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !lockfile(&f).contains("humanizer"),
        "the stale pin must be dropped:\n{}",
        lockfile(&f)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("humanizer"),
        "the drop must be reported: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_preview_apply_reports_a_stale_pin_without_rewriting_the_lockfile() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.apply(&["--update", "--yes"])), 0, "priming run");

    fs_err::write(
        f.root.join("patina.toml").as_std_path(),
        "[patina]\nroot = true\n",
    )
    .expect("rewrite the root manifest");
    fs_err::remove_dir_all(f.root.join("humanizer").as_std_path()).expect("remove the module");
    let before = lockfile(&f);

    // Without `--yes` in a non-interactive shell this is a preview, and it
    // must not write.
    let out = f.apply(&[]);
    assert_eq!(code(&out), 0, "a preview exits 0");
    assert_eq!(
        lockfile(&f),
        before,
        "a preview must not rewrite patina.lock"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("humanizer"),
        "the stale pin must still be reported: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn update_pins_every_declaration_not_only_the_ones_in_use() {
    // The committed lock has to be complete for every machine, so a remote no
    // entry currently selects is still pinned here.
    let f = Fixture::new();
    let used = Origin::new(&f, "used", OLD_EPOCH);
    used.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "used", &used, Some("0s"));
    let unused = Origin::new(&f, "unused", OLD_EPOCH);
    let unused_rev = unused.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare_only(&f, "unused", &unused, Some("0s"));

    let out = f.run(&["remote", "update"], &[]);
    assert_eq!(
        code(&out),
        0,
        "update must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        lockfile(&f).contains(&unused_rev),
        "a declaration no entry selects must still be pinned:\n{}",
        lockfile(&f)
    );
}

#[test]
fn prune_says_so_when_there_is_nothing_to_remove() {
    let f = Fixture::new();
    let out = f.run(&["remote", "prune"], &[]);
    assert_eq!(code(&out), 0);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("No unreferenced remote checkouts"),
        "an already-tidy cache must say so"
    );
}

#[test]
fn apply_update_pins_and_applies_in_one_run() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    let rev = origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let out = f.apply(&["--update", "--yes"]);
    assert_eq!(
        code(&out),
        0,
        "apply --update must pin and then apply; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        lockfile(&f).contains(&rev),
        "the pin must have been created:\n{}",
        lockfile(&f)
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".a.md").as_std_path()).expect("target deployed"),
        "first\n"
    );
}

#[test]
fn apply_update_degrades_to_a_plain_apply_when_the_remote_is_unreachable() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer", OLD_EPOCH);
    let rev = origin.commit_files(&[("a.md", "first\n")], OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.apply(&["--update", "--yes"])), 0, "priming run");

    fs_err::remove_dir_all(origin.dir.as_std_path()).expect("delete the origin");
    fs_err::remove_file(f.home.join(".a.md").as_std_path()).expect("delete the deployed file");

    let out = f.apply(&["--update", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code(&out),
        0,
        "an unreachable remote must degrade, not fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains("applying the pins already committed"),
        "the degradation must be warned about: {stderr}"
    );
    assert_eq!(
        fs_err::read_to_string(f.home.join(".a.md").as_std_path()).expect("target redeployed"),
        "first\n"
    );
    assert!(
        lockfile(&f).contains(&rev),
        "the pin must be left exactly as committed"
    );
}
