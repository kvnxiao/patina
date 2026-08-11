#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! The producer path: reading upstream, running the gate, and writing a pin.
//!
//! These drive [`patina_core::remote::update`] in-process against throwaway
//! origin repositories, so each gate verdict is asserted against real git
//! history rather than only through the CLI subprocess. The gate's arithmetic
//! is unit-tested in `patina_core::remote::gate`; what is proven here is that
//! the inputs it receives are derived correctly from a repository.
//!
//! See `docs/REMOTE_SOURCES.md` "Commands" and "The update gate".

use camino::Utf8Path;
use camino::Utf8PathBuf;
use patina_core::RemoteSpec;
use patina_core::remote::gate::GateConcern;
use patina_core::remote::gate::GateOutcome;
use patina_core::remote::lockfile::LockEntry;
use patina_core::remote::lockfile::Lockfile;
use patina_core::remote::lockfile::lockfile_path;
use patina_core::remote::update;
use patina_core::remote::update::RemoteInventory;
use patina_core::remote::update::RemoteView;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

/// "Now" for every gate evaluation here, so no assertion depends on the clock.
const NOW: i64 = 1_800_000_000;
const WEEK: i64 = 7 * 24 * 60 * 60;

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

/// An origin repository, an isolated state directory, and a repository root the
/// lockfile is written into.
struct Fixture {
    _temp: TempDir,
    origin: Utf8PathBuf,
    state: Utf8PathBuf,
    repo: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        let origin = root.join("origin");
        for dir in [&origin, &root.join("state"), &root.join("repo")] {
            fs_err::create_dir_all(dir.as_std_path()).expect("mkdir fixture dir");
        }
        git_in(&root, NOW, &["init", "--quiet", "-b", "main", "origin"]);
        Self {
            _temp: temp,
            origin,
            state: root.join("state"),
            repo: root.join("repo"),
        }
    }

    fn commit(&self, body: &str, epoch: i64) -> String {
        fs_err::write(self.origin.join("a.md").as_std_path(), body).expect("write origin file");
        git_in(&self.origin, epoch, &["add", "-A"]);
        git_in(&self.origin, epoch, &["commit", "--quiet", "-m", "fixture"]);
        git_in(&self.origin, epoch, &["rev-parse", "HEAD"])
    }

    /// An inventory holding one remote, optionally already pinned.
    fn inventory(&self, min_age: Option<Duration>, pin: Option<LockEntry>) -> RemoteInventory {
        let mut lockfile = Lockfile::default();
        if let Some(pin) = pin.clone() {
            lockfile.insert("humanizer", pin);
        }
        RemoteInventory {
            repo_root: self.repo.clone(),
            state_dir: self.state.clone(),
            global_min_age: None,
            lockfile,
            remotes: vec![RemoteView {
                name: "humanizer".to_owned(),
                spec: RemoteSpec {
                    name: "humanizer".to_owned(),
                    url: self.origin.as_str().to_owned(),
                    git_ref: Some("main".to_owned()),
                    min_age,
                },
                pin,
            }],
        }
    }
}

/// A pin recorded `weeks_ago` before [`NOW`].
fn pin(rev: &str, weeks_ago: i64) -> LockEntry {
    let at = jiff::Timestamp::from_second(NOW - weeks_ago * WEEK).expect("in-range timestamp");
    LockEntry {
        url: String::new(),
        git_ref: Some("main".to_owned()),
        rev: rev.to_owned(),
        updated_at: at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
    }
}

/// The single view an inventory built by the fixture holds.
fn only(inventory: &RemoteInventory) -> RemoteView {
    inventory
        .find("humanizer")
        .expect("the fixture declares one remote")
        .clone()
}

#[test]
fn check_upstream_reports_the_tip_against_the_pin() {
    let f = Fixture::new();
    let first = f.commit("one\n", NOW - 2 * WEEK);
    let tip = f.commit("two\n", NOW - WEEK);

    let pinned_at_tip = f.inventory(None, Some(pin(&tip, 1)));
    let result = update::check_upstream(&only(&pinned_at_tip)).expect("ls-remote the origin");
    assert_eq!(result.upstream_rev, tip);
    assert!(
        !result.has_update(),
        "a pin already at the tip has no update"
    );

    let behind = f.inventory(None, Some(pin(&first, 2)));
    assert!(
        update::check_upstream(&only(&behind))
            .expect("ls-remote the origin")
            .has_update(),
        "a pin behind the tip has an update"
    );

    let unpinned = f.inventory(None, None);
    assert!(
        update::check_upstream(&only(&unpinned))
            .expect("ls-remote the origin")
            .has_update(),
        "an unpinned remote counts as having an update"
    );
}

#[test]
fn a_pin_already_at_the_tip_proposes_nothing() {
    let f = Fixture::new();
    let tip = f.commit("one\n", NOW - WEEK);
    let inventory = f.inventory(None, Some(pin(&tip, 1)));

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert_eq!(proposal.outcome, GateOutcome::AlreadyPinned);
    assert_eq!(proposal.candidate_rev, tip);
}

#[test]
fn a_first_pin_of_an_old_commit_is_allowed() {
    let f = Fixture::new();
    let tip = f.commit("one\n", NOW - WEEK);
    let inventory = f.inventory(None, None);

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert_eq!(proposal.outcome, GateOutcome::Allowed);
    assert_eq!(proposal.candidate_rev, tip);
    assert_eq!(proposal.current_rev, None);
    assert_eq!(
        proposal.candidate_epoch,
        NOW - WEEK,
        "the committer time must be read from the fetched commit"
    );
}

#[test]
fn a_fast_forward_within_the_cooldown_is_held() {
    let f = Fixture::new();
    let first = f.commit("one\n", NOW - 3 * WEEK);
    f.commit("two\n", NOW - 60);
    let inventory = f.inventory(Some(Duration::from_hours(72)), Some(pin(&first, 3)));

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert_eq!(
        proposal.outcome,
        GateOutcome::Cooldown {
            eligible_at: NOW - 60 + 72 * 60 * 60
        }
    );
}

#[test]
fn a_fast_forward_past_the_cooldown_is_allowed() {
    let f = Fixture::new();
    let first = f.commit("one\n", NOW - 3 * WEEK);
    let second = f.commit("two\n", NOW - 2 * WEEK);
    let inventory = f.inventory(Some(Duration::from_hours(72)), Some(pin(&first, 3)));

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert_eq!(proposal.outcome, GateOutcome::Allowed);
    assert_eq!(proposal.candidate_rev, second);
    assert_eq!(proposal.current_rev.as_deref(), Some(first.as_str()));
}

#[test]
fn a_rewritten_history_needs_confirmation() {
    let f = Fixture::new();
    let first = f.commit("one\n", NOW - 3 * WEEK);
    // An orphan root commit promoted to `main` is what a force-push leaves.
    git_in(
        &f.origin,
        NOW - 2 * WEEK,
        &["checkout", "--quiet", "--orphan", "rewritten"],
    );
    f.commit("rewritten\n", NOW - 2 * WEEK);
    git_in(&f.origin, NOW - 2 * WEEK, &["branch", "-M", "main"]);
    let inventory = f.inventory(Some(Duration::from_secs(0)), Some(pin(&first, 3)));

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert_eq!(
        proposal.outcome,
        GateOutcome::NeedsConfirmation(vec![GateConcern::HistoryRewritten])
    );
}

#[test]
fn a_candidate_older_than_the_pin_timestamp_needs_confirmation() {
    let f = Fixture::new();
    let first = f.commit("one\n", NOW - 3 * WEEK);
    // The new tip descends from the pin but carries an older committer date, the
    // long-lived-branch fast-forward the backdating floor is a prompt for.
    f.commit("two\n", NOW - 4 * WEEK);
    let inventory = f.inventory(Some(Duration::from_secs(0)), Some(pin(&first, 1)));

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert!(
        matches!(
            proposal.outcome,
            GateOutcome::NeedsConfirmation(ref concerns)
                if concerns.contains(&GateConcern::Backdated {
                    candidate_epoch: NOW - 4 * WEEK,
                    pinned_updated_at: NOW - WEEK,
                })
        ),
        "expected a backdating concern, got {:?}",
        proposal.outcome
    );
}

#[test]
fn a_committer_time_far_in_the_future_is_rejected() {
    let f = Fixture::new();
    f.commit("one\n", NOW + 10 * 60 * 60);
    let inventory = f.inventory(None, None);

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, false).expect("propose an update");

    assert!(
        matches!(proposal.outcome, GateOutcome::RejectedFuture { .. }),
        "got {:?}",
        proposal.outcome
    );
}

#[test]
fn bypassing_the_age_gate_admits_a_brand_new_commit() {
    let f = Fixture::new();
    let first = f.commit("one\n", NOW - 3 * WEEK);
    let second = f.commit("two\n", NOW - 60);
    let inventory = f.inventory(Some(Duration::from_hours(72)), Some(pin(&first, 3)));

    let proposal =
        update::propose(&inventory, &only(&inventory), NOW, true).expect("propose an update");

    assert_eq!(proposal.outcome, GateOutcome::Allowed);
    assert_eq!(proposal.candidate_rev, second);
}

#[test]
fn accept_writes_the_pin_and_it_reads_back() {
    let f = Fixture::new();
    let tip = f.commit("one\n", NOW - WEEK);
    let mut inventory = f.inventory(None, None);
    let view = only(&inventory);
    let proposal = update::propose(&inventory, &view, NOW, false).expect("propose an update");

    update::accept(&mut inventory, &view, &proposal, "2026-08-11T14:00:00Z")
        .expect("write the lockfile");

    let written = Lockfile::load(&lockfile_path(&f.repo)).expect("read the lockfile back");
    let entry = written.get("humanizer").expect("the pin was recorded");
    assert_eq!(entry.rev, tip);
    assert_eq!(entry.updated_at, "2026-08-11T14:00:00Z");
    assert_eq!(entry.url, f.origin.as_str());
    assert_eq!(entry.git_ref.as_deref(), Some("main"));
}

#[test]
fn an_unreachable_remote_is_an_error_rather_than_a_silent_no_op() {
    let f = Fixture::new();
    f.commit("one\n", NOW - WEEK);
    let inventory = f.inventory(None, None);
    fs_err::remove_dir_all(f.origin.as_std_path()).expect("delete the origin");

    update::propose(&inventory, &only(&inventory), NOW, false)
        .expect_err("an unreachable remote must surface as an error");
}

#[test]
fn find_returns_none_for_a_module_that_is_not_a_remote() {
    let f = Fixture::new();
    let inventory = f.inventory(None, None);
    assert!(inventory.find("no-such-module").is_none());
}
