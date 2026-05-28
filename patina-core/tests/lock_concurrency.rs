#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Cross-process integration coverage for the advisory lock (T-013 /
//! REQ-023, scenario CHK-037).
//!
//! These tests exercise behaviours that only manifest across real OS
//! processes and that an in-process unit test in `src/lock.rs` cannot
//! reproduce:
//!
//! - **Exclusive serialization (CHK-037).** Two processes that both want the
//!   exclusive lock run sequentially — the second's acquire/release window
//!   starts only after the first's window ends. Each process drops a
//!   `<nanos>.plan` marker, so the union numbers exactly two and the timestamp
//!   windows are non-overlapping.
//! - **Exclusive timeout maps to exit code 4.** A process blocked on a held
//!   exclusive lock past its (test-parameterised) cap exits with the code the
//!   SPEC reserves for lock timeout and prints a `TIMEOUT` message to stderr.
//! - **OS release on abnormal death.** A process that aborts while holding the
//!   lock leaves the lock free; the next acquirer gets in cleanly. This is the
//!   invariant that justifies relying on `Drop` / the OS rather than an
//!   explicit unlock.
//!
//! The end-to-end `patina apply --yes` / `patina status` surface the SPEC
//! scenario names cannot run yet — the CLI subcommands, executor loop,
//! and exit-code mapping land in T-014 / T-020. These tests drive the
//! `patina_core::lock` primitive directly through the `lock_helper`
//! example process, which is the layer T-013 owns.

use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Once;
use std::time::Duration;
use tempfile::TempDir;

static BUILD: Once = Once::new();

/// Build the `lock_helper` example once for the whole test binary so each
/// spawn is a fast exec of an already-compiled artifact rather than a
/// `cargo run` that might race the build cache.
fn ensure_helper_built() {
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--example", "lock_helper"])
            .status()
            .expect("spawn cargo build for lock_helper example");
        assert!(status.success(), "building lock_helper example failed");
    });
}

/// Locate the compiled `lock_helper` example next to this test binary.
/// `CARGO_BIN_EXE_*` is not populated for examples, so we derive the path
/// from the test executable's own location: integration test binaries
/// live in `target/<profile>/deps/`, and examples live in the sibling
/// `target/<profile>/examples/`.
fn helper_path() -> Utf8PathBuf {
    let test_exe = std::env::current_exe().expect("current test exe path");
    let deps_dir = test_exe.parent().expect("deps dir");
    let profile_dir = deps_dir.parent().expect("profile dir");
    let mut helper = profile_dir.join("examples").join("lock_helper");
    if cfg!(windows) {
        helper.set_extension("exe");
    }
    Utf8PathBuf::from_path_buf(helper).expect("utf8 helper path")
}

struct State {
    _temp: TempDir,
    dir: Utf8PathBuf,
}

impl State {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        fs_err::create_dir_all(dir.join("journal")).expect("create journal dir");
        Self { _temp: temp, dir }
    }

    /// Count `<nanos>.plan` markers the helper processes wrote.
    fn plan_files(&self) -> Vec<Utf8PathBuf> {
        let journal = self.dir.join("journal");
        let mut out = Vec::new();
        for entry in fs_err::read_dir(&journal).expect("read journal dir") {
            let entry = entry.expect("dir entry");
            let path = Utf8PathBuf::from_path_buf(entry.path()).expect("utf8 entry");
            if path.extension() == Some("plan") {
                out.push(path);
            }
        }
        out
    }
}

/// One parsed acquire/release window from a helper's stdout.
struct Window {
    acquired: u128,
    released: u128,
}

fn parse_window(stdout: &str) -> Window {
    let mut acquired = None;
    let mut released = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("ACQUIRED ") {
            acquired = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("RELEASED ") {
            released = rest.trim().parse().ok();
        }
    }
    Window {
        acquired: acquired.expect("helper printed ACQUIRED"),
        released: released.expect("helper printed RELEASED"),
    }
}

fn spawn(
    helper: &Utf8Path,
    state: &Utf8Path,
    kind: &str,
    hold_ms: u64,
    timeout_ms: u64,
    abort: bool,
) -> std::process::Child {
    let mut cmd = Command::new(helper.as_std_path());
    cmd.arg(state.as_str())
        .arg(kind)
        .arg(hold_ms.to_string())
        .arg(timeout_ms.to_string());
    if abort {
        cmd.arg("--abort");
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lock_helper")
}

#[test]
fn two_exclusive_applies_do_not_interleave() {
    // CHK-037: two processes contend for the exclusive lock; their
    // acquire/release windows must be disjoint and exactly two `.plan`
    // markers must result.
    ensure_helper_built();
    let helper = helper_path();
    let state = State::new();

    // Each holds the lock for 300ms; both wait up to 5s to acquire, so the
    // loser blocks on the winner rather than timing out. Started as close
    // together as two spawns allow (well within the 100ms window the SPEC
    // describes).
    let a = spawn(&helper, &state.dir, "exclusive", 300, 5_000, false);
    let b = spawn(&helper, &state.dir, "exclusive", 300, 5_000, false);

    let out_a = a.wait_with_output().expect("wait a");
    let out_b = b.wait_with_output().expect("wait b");
    assert!(
        out_a.status.success(),
        "process a failed: {}",
        String::from_utf8_lossy(&out_a.stderr)
    );
    assert!(
        out_b.status.success(),
        "process b failed: {}",
        String::from_utf8_lossy(&out_b.stderr)
    );

    let plans = state.plan_files();
    assert_eq!(
        plans.len(),
        2,
        "expected exactly two journal .plan markers, got {plans:?}"
    );

    let wa = parse_window(&String::from_utf8_lossy(&out_a.stdout));
    let wb = parse_window(&String::from_utf8_lossy(&out_b.stdout));

    // Non-overlapping: whichever acquired first must have released before
    // the other acquired. Equivalent to "the windows are disjoint".
    let (first, second) = if wa.acquired <= wb.acquired {
        (&wa, &wb)
    } else {
        (&wb, &wa)
    };
    assert!(
        first.released <= second.acquired,
        "windows interleaved: first {}..{}, second {}..{}",
        first.acquired,
        first.released,
        second.acquired,
        second.released
    );
}

#[test]
fn blocked_exclusive_exits_with_lock_timeout_code() {
    // A second exclusive acquirer whose cap expires while the first still
    // holds the lock exits with the reserved lock-timeout code and names
    // the timeout on stderr. The 60s SPEC cap is parameterised down to
    // 200ms so the test runs fast.
    ensure_helper_built();
    let helper = helper_path();
    let state = State::new();

    // Holder keeps the lock for 1.5s; the blocked process caps at 200ms.
    let holder = spawn(&helper, &state.dir, "exclusive", 1_500, 5_000, false);
    // Give the holder a moment to acquire before the contender starts.
    std::thread::sleep(Duration::from_millis(150));
    let blocked = spawn(&helper, &state.dir, "exclusive", 0, 200, false);

    let blocked_out = blocked.wait_with_output().expect("wait blocked");
    let holder_out = holder.wait_with_output().expect("wait holder");

    assert!(
        holder_out.status.success(),
        "holder failed: {}",
        String::from_utf8_lossy(&holder_out.stderr)
    );
    assert_eq!(
        blocked_out.status.code(),
        Some(4),
        "blocked acquirer should exit with the lock-timeout code 4; stderr: {}",
        String::from_utf8_lossy(&blocked_out.stderr)
    );
    let stderr = String::from_utf8_lossy(&blocked_out.stderr);
    assert!(
        stderr.contains("TIMEOUT") && stderr.contains("exclusive"),
        "stderr should name the exclusive lock timeout, got: {stderr}"
    );
}

#[test]
fn os_releases_lock_when_holder_aborts() {
    // A process that aborts while holding the exclusive lock must leave
    // the lock free for the next acquirer — the OS releases on death.
    ensure_helper_built();
    let helper = helper_path();
    let state = State::new();

    // First process acquires then aborts immediately while holding it.
    let aborter = spawn(&helper, &state.dir, "exclusive", 10_000, 5_000, true);
    let aborter_out = aborter.wait_with_output().expect("wait aborter");
    assert!(
        !aborter_out.status.success(),
        "aborter should terminate abnormally, not exit 0"
    );
    assert!(
        String::from_utf8_lossy(&aborter_out.stdout).contains("ACQUIRED"),
        "aborter should have acquired the lock before aborting"
    );

    // The next acquirer must get in cleanly with a short cap — if the OS
    // had not released the dead holder's lock, this would time out.
    let next = spawn(&helper, &state.dir, "exclusive", 0, 1_000, false);
    let next_out = next.wait_with_output().expect("wait next");
    assert!(
        next_out.status.success(),
        "next acquirer should acquire the lock the dead holder left; stderr: {}",
        String::from_utf8_lossy(&next_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&next_out.stdout).contains("ACQUIRED"),
        "next acquirer should report a clean acquisition"
    );
}

#[test]
fn shared_holder_does_not_block_another_shared_acquirer() {
    // The read-only `status` path takes a shared lock; multiple shared
    // holders coexist. A second shared acquirer with a short cap succeeds
    // while the first still holds, proving shared/shared is non-exclusive.
    ensure_helper_built();
    let helper = helper_path();
    let state = State::new();

    let first = spawn(&helper, &state.dir, "shared", 800, 5_000, false);
    std::thread::sleep(Duration::from_millis(150));
    let second = spawn(&helper, &state.dir, "shared", 0, 300, false);

    let second_out = second.wait_with_output().expect("wait second shared");
    let first_out = first.wait_with_output().expect("wait first shared");

    assert!(
        first_out.status.success(),
        "first shared holder failed: {}",
        String::from_utf8_lossy(&first_out.stderr)
    );
    assert!(
        second_out.status.success(),
        "second shared acquirer should coexist, not time out; stderr: {}",
        String::from_utf8_lossy(&second_out.stderr)
    );
}
