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

use camino::Utf8Path;
use camino::Utf8PathBuf;
use common::Fixture;
use common::code;
use std::process::Command;

/// A committer epoch far enough in the past that any age floor is satisfied.
const OLD_EPOCH: i64 = 1_700_000_000;

fn git_in(cwd: &Utf8Path, epoch: i64, args: &[&str]) -> String {
    let date = format!("{epoch} +0000");
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd.as_std_path())
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

struct Origin {
    dir: Utf8PathBuf,
}

impl Origin {
    fn new(f: &Fixture, name: &str) -> Self {
        let dir = f.home.join(".origins").join(name);
        fs_err::create_dir_all(dir.as_std_path()).expect("mkdir origin");
        git_in(&dir, OLD_EPOCH, &["init", "--quiet", "-b", "main"]);
        Self { dir }
    }

    /// The origin path spelled for a TOML basic string (see `remote_apply.rs`).
    fn url(&self) -> String {
        self.dir.as_str().replace('\\', "/")
    }

    fn commit(&self, body: &str, epoch: i64) -> String {
        fs_err::write(self.dir.join("a.md").as_std_path(), body).expect("write origin file");
        git_in(&self.dir, epoch, &["add", "-A"]);
        git_in(&self.dir, epoch, &["commit", "--quiet", "-m", "fixture"]);
        git_in(&self.dir, epoch, &["rev-parse", "HEAD"])
    }
}

/// Declare a remote-backed module, optionally with a per-remote `min_age`.
fn declare(f: &Fixture, module: &str, origin: &Origin, min_age: Option<&str>) {
    let floor = min_age.map_or_else(String::new, |value| format!("min_age = \"{value}\"\n"));
    f.module(
        module,
        &format!(
            "[remote]\nurl = \"{}\"\nref = \"main\"\n{floor}\n\
             [[file]]\nsource = \"a.md\"\ntarget = \"~/.a.md\"\nmode = \"copy\"\n",
            origin.url()
        ),
    );
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
    // Adopting a remote is a deliberate act whose content the user is about to
    // review in the consent diff, so the first pin is exempt from the age gate —
    // here proven with a commit made "now" under the default 72-hour floor.
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    let now = patina_core::current_epoch_seconds();
    let rev = origin.commit("first\n", now);
    declare(&f, "humanizer", &origin, None);

    let out = f.run(&["remote", "update"], &[]);
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
    assert!(
        lock.contains("version = 1"),
        "the lockfile must declare its version:\n{lock}"
    );
}

#[test]
fn update_is_a_no_op_when_the_pin_is_already_the_upstream_tip() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
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
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
    // A 7-day floor with the first pin already in place.
    declare(&f, "humanizer", &origin, Some("7d"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");
    let pinned = lockfile(&f);

    // A brand-new second commit cannot be a week old.
    origin.commit("second\n", patina_core::current_epoch_seconds());
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
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("7d"));
    assert_eq!(code(&f.run(&["remote", "update"], &[])), 0, "first pin");

    let second = origin.commit("second\n", patina_core::current_epoch_seconds());
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
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
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
    let rewritten = origin.commit("rewritten\n", OLD_EPOCH);
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

    // With `--yes` the same bump is accepted.
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
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let out = f.run(&["remote", "update", "nope"], &[]);
    assert_eq!(code(&out), 1);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nope"),
        "the message must name the remote that was asked for"
    );
    assert_eq!(lockfile(&f), "", "no lockfile may be written");
}

#[test]
fn list_reports_each_remote_and_its_pin() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let unpinned = f.run(&["remote", "list", "--json"], &[]);
    let doc = json_of(&unpinned);
    let row = doc.pointer("/remotes/0").expect("one remote row").clone();
    assert_eq!(
        row.get("module").and_then(serde_json::Value::as_str),
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
        String::from_utf8_lossy(&out.stdout).contains("No remote-backed modules"),
        "a repository with no remotes must say so, not print nothing"
    );
}

#[test]
fn check_reports_a_moved_upstream_and_writes_the_notice() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
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

    origin.commit("second\n", OLD_EPOCH + 60);
    let behind = f.run(&["remote", "check"], &[]);
    assert_eq!(code(&behind), 0);
    let body = fs_err::read_to_string(notice.as_std_path()).expect("the notice is written");
    assert!(
        body.contains("humanizer") && body.contains("patina apply --update"),
        "the notice must name the remote and the next step: {body}"
    );
    assert!(
        patina_core::remote::notice::read_pending(&f.state_root()).contains("humanizer"),
        "the machine-readable pending set must agree with the notice"
    );
}

#[test]
fn check_json_reports_the_pending_set() {
    let f = Fixture::new();
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
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
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
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

    // Inside the throttle window the second run does no work at all, which is
    // observable as the stamp being untouched.
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
    let origin = Origin::new(&f, "humanizer");
    origin.commit("first\n", OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));

    let orphan = patina_core::remote::cache::checkout_dir(
        &f.state_root(),
        "humanizer",
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
    let origin = Origin::new(&f, "humanizer");
    let rev = origin.commit("first\n", OLD_EPOCH);
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
    let origin = Origin::new(&f, "humanizer");
    let rev = origin.commit("first\n", OLD_EPOCH);
    declare(&f, "humanizer", &origin, Some("0s"));
    assert_eq!(code(&f.apply(&["--update", "--yes"])), 0, "priming run");

    // Offline, warm cache: the update pass fails but the committed pin still
    // applies.
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
