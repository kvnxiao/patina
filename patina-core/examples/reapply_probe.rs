//! Cross-process test harness for the watcher's re-apply cycle.
//!
//! The `watch_lock_contention` integration test spawns this example as a child
//! process, with the repository / state / home wired through the environment
//! exactly as the CLI resolves them, so it can drive
//! [`patina_core::watch::reapply::run_reapply`] in a separate process while the
//! test process holds the exclusive advisory lock. Running the re-apply in a
//! child (rather than in-process) is required because the crate forbids
//! `unsafe`, so a test cannot mutate its own process environment, and
//! `run_reapply` resolves the repo and state dir from that environment.
//!
//! It is wired as an `examples/` target so it ships only with the crate's test
//! build and never pollutes the public binary surface. The integration test
//! locates the compiled artifact at `target/<profile>/examples/reapply_probe`.
//!
//! It prints exactly one outcome word on stdout (`SKIPPED`, `APPLIED`, or
//! `FAILED`) and exits 0, or prints `ERROR <msg>` on stderr and exits 2 if the
//! async runtime cannot be built.

use patina_core::watch::reapply::ReapplyOutcome;
use patina_core::watch::reapply::run_reapply;

fn main() {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            #[expect(
                clippy::disallowed_macros,
                reason = "test-harness IPC over stderr, not user-facing CLI output"
            )]
            {
                eprintln!("ERROR {error}");
            }
            std::process::exit(2);
        }
    };

    let label = match runtime.block_on(run_reapply()) {
        ReapplyOutcome::Applied { .. } => "APPLIED",
        ReapplyOutcome::Skipped => "SKIPPED",
        ReapplyOutcome::Failed => "FAILED",
    };

    // The test-harness example talks to its parent test process over stdout; it
    // is not user-facing CLI output and has no `output::Reporter` to route
    // through, so the workspace-wide `disallowed-macros` ban is scoped-out here.
    #[expect(
        clippy::disallowed_macros,
        reason = "test-harness IPC over stdout, not user-facing CLI output"
    )]
    {
        println!("{label}");
    }
}
