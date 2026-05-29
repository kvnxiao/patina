//! Cross-process test harness for the advisory lock (T-013 / REQ-023).
//!
//! The `lock_concurrency` integration test spawns this example as a child
//! process to exercise behaviours that only manifest across real OS
//! processes — exclusive serialization, OS lock release on process death,
//! and the shared/exclusive interaction. A child process is the only way
//! to test the "process holding the lock is killed; the OS releases it"
//! invariant, because dropping a guard in-process can never reproduce an
//! abnormal termination.
//!
//! It is wired as an `examples/` target (rather than a `[[bin]]`) so it
//! ships only with the crate's test build and never pollutes the public
//! binary surface. The integration test locates the compiled artifact at
//! `target/<profile>/examples/lock_helper`.
//!
//! Usage:
//!
//! ```text
//! lock_helper <state_dir> <kind> <hold_ms> <timeout_ms> [--abort]
//! ```
//!
//! - `state_dir` — the per-machine state directory; the lock lives at
//!   `<state_dir>/lock` and a journal `.plan` marker is written under
//!   `<state_dir>/journal/`.
//! - `kind` — `exclusive` or `shared`.
//! - `hold_ms` — how long to hold the lock after acquiring, in milliseconds.
//! - `timeout_ms` — the acquisition cap, in milliseconds.
//! - `--abort` — terminate the process abnormally (`process::abort`) while
//!   still holding the lock, so the test can assert the OS releases it.
//!
//! On a clean acquisition the helper prints the acquire and release
//! timestamps as `ACQUIRED <nanos>` / `RELEASED <nanos>` on stdout and
//! exits 0. On a timeout it prints `TIMEOUT exclusive`/`shared` on stderr
//! and exits with a non-zero code the test maps to the SPEC's
//! exit-code-4 contract. Any other failure prints `ERROR <msg>` and exits
//! 2.

use camino::Utf8PathBuf;
use patina_core::lock::LockError;
use patina_core::lock::LockKind;
use patina_core::lock::acquire;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Exit code the helper uses on a lock timeout. The integration test
/// asserts on this; the real CLI's exit-code-4 mapping is T-020's job.
const EXIT_TIMEOUT: i32 = 4;
/// Exit code for any non-timeout failure (bad args, I/O error).
const EXIT_ERROR: i32 = 2;

/// Parsed command-line arguments.
struct Args {
    state: Utf8PathBuf,
    kind: LockKind,
    hold: Duration,
    timeout: Duration,
    abort: bool,
}

/// A non-zero exit with a stderr line already chosen.
struct Failure {
    code: i32,
    message: String,
}

fn nanos_now() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos(),
        Err(_) => 0,
    }
}

fn parse_args() -> Result<Args, Failure> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let [state, kind, hold_ms, timeout_ms, rest @ ..] = raw.as_slice() else {
        return Err(Failure {
            code: EXIT_ERROR,
            message: "usage: lock_helper <state_dir> <kind> <hold_ms> <timeout_ms> [--abort]"
                .to_owned(),
        });
    };

    let kind = match kind.as_str() {
        "exclusive" => LockKind::Exclusive,
        "shared" => LockKind::Shared,
        other => {
            return Err(Failure {
                code: EXIT_ERROR,
                message: format!("unknown kind `{other}`"),
            });
        }
    };

    let hold = parse_millis(hold_ms, "hold_ms")?;
    let timeout = parse_millis(timeout_ms, "timeout_ms")?;
    let abort = rest.iter().any(|a| a == "--abort");

    Ok(Args {
        state: Utf8PathBuf::from(state),
        kind,
        hold,
        timeout,
        abort,
    })
}

fn parse_millis(raw: &str, field: &str) -> Result<Duration, Failure> {
    raw.parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|e| Failure {
            code: EXIT_ERROR,
            message: format!("bad {field}: {e}"),
        })
}

fn run(args: &Args) -> Result<(), Failure> {
    let lock_path = args.state.join("lock");
    let guard = match acquire(&lock_path, args.kind, args.timeout) {
        Ok(guard) => guard,
        Err(LockError::Timeout { kind, .. }) => {
            return Err(Failure {
                code: EXIT_TIMEOUT,
                message: format!("TIMEOUT {}", kind.label()),
            });
        }
        Err(e) => {
            return Err(Failure {
                code: EXIT_ERROR,
                message: e.to_string(),
            });
        }
    };

    // Mark acquisition in the journal with a unique `.plan` file so the
    // test can confirm both processes ran and inspect the non-overlapping
    // timestamp windows.
    let acquired = nanos_now();
    let journal = args.state.join("journal");
    fs_err::create_dir_all(&journal).map_err(|e| Failure {
        code: EXIT_ERROR,
        message: e.to_string(),
    })?;
    let plan = journal.join(format!("{acquired}.plan"));
    fs_err::write(&plan, acquired.to_le_bytes()).map_err(|e| Failure {
        code: EXIT_ERROR,
        message: e.to_string(),
    })?;
    // This test-harness example talks to its parent test process over stdout /
    // stderr; it is not user-facing CLI output and has no `output::Reporter` to
    // route through, so the workspace-wide `disallowed-macros` ban (REQ-026) is
    // scoped-out here.
    #[expect(
        clippy::disallowed_macros,
        reason = "test-harness IPC over stdout, not user-facing CLI output (REQ-026)"
    )]
    {
        println!("ACQUIRED {acquired}");
    }

    if args.abort {
        // Terminate abnormally while still holding the lock. The OS must
        // release the advisory lock when the process dies.
        std::process::abort();
    }

    std::thread::sleep(args.hold);
    let released = nanos_now();
    #[expect(
        clippy::disallowed_macros,
        reason = "test-harness IPC over stdout, not user-facing CLI output (REQ-026)"
    )]
    {
        println!("RELEASED {released}");
    }
    drop(guard);
    Ok(())
}

fn main() {
    let result = parse_args().and_then(|args| run(&args));
    if let Err(failure) = result {
        #[expect(
            clippy::disallowed_macros,
            reason = "test-harness IPC over stderr, not user-facing CLI output (REQ-026)"
        )]
        {
            eprintln!("{}", failure.message);
        }
        std::process::exit(failure.code);
    }
}
