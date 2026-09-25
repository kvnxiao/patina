# Future features

This document records design constraints for work that could follow v1.0.
None of it is implemented. `AGENTS.md` lists each feature here as a v1.0
non-goal.

---

## Background re-apply and drift notifications

### Value

A background process would re-materialize copy-mode and template entries
after their sources change, and notify when their targets are edited
outside Patina. A symbolic link already exposes its source's current bytes,
so a symlink entry needs a re-apply only when it is a `symlink-tree` entry
whose source directory gains or loses a file. `patina status` already
reports drift on demand.

### Runtime shape

- The core library is synchronous. The background process owns the only
  async runtime, a `current_thread` tokio runtime, and uses it to wait for
  signals, event wakeups, and job completion.
- Each job (rescan, re-apply, drift check) runs through `spawn_blocking`. At
  most one job is in flight: the loop keeps one `JoinHandle` slot and starts
  a new job only when the slot is empty.

### Event intake

- The file-notification callback maps each event path to its subscription:
  a journal sentinel marks a rescan, an exact source match a re-apply, and an
  exact content-target match a drift check. It discards unmatched paths.
- The callback merges the result into a mutex-guarded pending-work record:
  a rescan flag, the newest source-event instant, and a set of drift-target
  paths. It then wakes the loop through `tokio::sync::Notify`.
- The subscription set bounds the record's size. When events arrive faster
  than jobs finish, they merge; the notification thread never blocks, and no
  subscription's change is lost.
- The callback logs error batches from the notification library rather than
  dropping them.
- Journal directory events report child paths, so journal matching uses the
  sentinel suffix and the parent directory name. Sources and targets match by
  exact canonical path.

### Job order

- Rescan runs first, then re-apply, then the drift check.
- A shutdown request takes effect between jobs: once it arrives, the loop
  does not start the next job.
- A source change observed before the last re-apply started does not queue
  another re-apply.
- Pending drift is keyed by path, because a rescan replaces the watch set.
  Paths missing from the new set are dropped.

### Drift correctness

- Before judging a target, the drift check compares the newest COMMIT
  operation ID with the watch set's. If the COMMIT is newer, it rescans first.
- The drift check tries the shared lock without waiting. While another
  process holds the exclusive lock, the drift paths stay pending without
  waking the loop; the apply's journal events wake it later.
- Before the drift check is built, a test must reproduce the stale-hash
  race: a drift check that runs after an apply writes a target but before
  the rescan.

### Shutdown

- The process installs its signal listeners before any startup work.
- On a signal, the process stops admitting jobs, logs `shutdown_waiting`
  with the job kind, and waits up to a 10 s grace period.
- When the grace period expires, or a second signal arrives, the process
  logs `shutdown_abandoned` with the job kind, the re-apply id, and the
  reason (`grace_expired` or `second_signal`). It then exits with code 0
  while the job is still running. The journal's process-kill contract makes
  the interrupted apply converge on the next command that recovers.
- The process exits 0 because a non-zero exit makes launchd relaunch a
  `KeepAlive { SuccessfulExit = false }` job after a stop. That launchd
  behavior is inferred from its documented semantics and has not been
  tested.

### Startup

- The first job recovers orphan plans under a non-blocking exclusive lock,
  and skips recovery when another process holds the lock.
- Startup never re-applies. On a machine that has never applied, a startup
  re-apply would be an apply without consent.

### Blocking operations inside jobs

- A cold remote cache makes planning run `git` fetches, which have no
  timeout.
- Desktop notifications can block: a D-Bus round trip on Linux, and up to
  2 s on macOS.

### Supervisors

- launchd's default `ExitTimeOut` is taken to be 20 s and systemd's default
  `TimeoutStopSec` 90 s. Neither value has been verified.
- `schtasks /end` terminates the process without a signal.
- The lifecycle commands `stop` and `restart` take the exclusive lock before
  stopping the service, so an in-flight re-apply finishes first.

### Testing

- The loop receives its job functions through a small struct, so tests can
  inject jobs that report "started" and block until released.
- Tests drive the loop under `#[tokio::test(start_paused = true)]`. In tokio
  1.53.1 (`runtime/blocking/schedule.rs:18-30`), on a `current_thread`
  runtime with `test-util`, time does not auto-advance while a
  `spawn_blocking` task runs, so the grace timer moves only on `advance`.
- The tests cover:
  - admission stopping after a signal
  - the drain completing when the job is released
  - the grace period expiring
  - a second signal skipping the grace period
  - pending-work merging and its bound
- One real-process test sends SIGTERM during a re-apply blocked in a
  `pre_apply` hook that waits for a release file, then asserts on log lines,
  never on fixed sleeps.
