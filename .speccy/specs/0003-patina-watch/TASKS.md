---
spec: SPEC-0003
spec_hash_at_generation: c111c0f45b89ffe878237ef8b3879f4482ffa8583ff558b7c72e5060c8d6bc62
generated_at: 2026-05-31T22:25:47Z
---
# Tasks: SPEC-0003 Patina watch — filesystem event loop, per-OS service install, drift detection

<task id="T-001" state="completed" covers="REQ-007">
## Extract a shared `pub` version-envelope helper in `patina-core`

REQ-007 requires the drift cache to carry a `u16` major-version envelope at
offset 0, mirroring the journal's envelope, and explicitly calls for
extracting a shared helper rather than hand-rolling a third copy. Today the
envelope logic is duplicated privately: `patina-core/src/journal/plan.rs`
owns `FILE_MAJOR_VERSION: u16 = 2` and `ENVELOPE_LEN`, encodes the envelope
in `Plan::encode`, and decodes it in `Plan::decode`; `journal/record.rs`
re-imports `FILE_MAJOR_VERSION` and reimplements the same offset-0
little-endian decode in `ApplyRecord::decode` (around lines 47, 163-199).

Create `patina-core/src/version_envelope.rs` exposing a small, format-
agnostic helper:

- A `pub const ENVELOPE_LEN: usize = 2` and free functions
  `encode_with_envelope(major: u16, body: &[u8]) -> Vec<u8>` (prepends the
  little-endian `u16`) and
  `decode_envelope(bytes: &[u8], supported_major: u16) -> Result<&[u8], EnvelopeError>`
  returning the post-envelope body slice or a typed `EnvelopeError`.
- A `pub enum EnvelopeError { Truncated, VersionMismatch { found: u16, supported: u16 } }`
  (`thiserror`, `#[non_exhaustive]`) modelling exactly the two failure arms
  the journal already distinguishes.

Re-implement `Plan::{encode,decode}` and `ApplyRecord::{encode,decode}` over
the shared helper. Keep the journal's `FILE_MAJOR_VERSION = 2` exactly where
it is (the journal versions independently); the journal error types
(`JournalError::Truncated` / `VersionMismatch`) wrap or map from
`EnvelopeError` so the journal's public error vocabulary does not change.
Wire `pub mod version_envelope;` into `patina-core/src/lib.rs` and re-export
the helper. This task lands NO drift-cache code — only the extraction and
the journal refactor — so the journal's existing round-trip and
newer-major-refusal tests are the regression guard.

<task-scenarios>
Given the extracted helper,
when a body is run through `encode_with_envelope(2, body)` and then
`decode_envelope(bytes, 2)`,
then the returned slice equals the original `body`.

Given a buffer whose leading `u16` is `FILE_MAJOR_VERSION + 1`,
when `decode_envelope(bytes, FILE_MAJOR_VERSION)` runs,
then it returns `EnvelopeError::VersionMismatch { found, supported }` with
`found == FILE_MAJOR_VERSION + 1`.

Given the refactored journal,
when the existing `patina-core` journal test suite runs,
then `ApplyRecord` and `Plan` round-trip and newer-major-refusal tests pass
unchanged (no behaviour change, no new journal major).

Suggested files: `patina-core/src/version_envelope.rs`,
`patina-core/src/journal/plan.rs`, `patina-core/src/journal/record.rs`,
`patina-core/src/journal/mod.rs`, `patina-core/src/lib.rs`,
`patina-core/tests/version_envelope.rs`
</task-scenarios>
</task>

<task id="T-002" state="completed" covers="REQ-006">
## Hoist the compact-UTC timestamp helper from `patina-cli` to `patina-core`

The watcher's re-apply (REQ-006) keys its journal `<ts>` exactly as
`patina apply` does, but the watcher lives in `patina-core` while the helper
is `pub(crate) fn current_timestamp()` in `patina-cli/src/cmd/apply.rs`
(jiff, `%Y%m%dT%H%M%SZ`, lines ~360-364), already reused by `cmd/managed.rs`
and `cmd/remove.rs`. Hoist it to `patina-core` so both the CLI apply path
and the watcher re-apply call one definition instead of duplicating the
format string.

Create `patina-core/src/clock.rs` with `pub fn current_timestamp() -> String`
carrying the same jiff `%Y%m%dT%H%M%SZ` formatting and a doc example, and
re-export it from `patina-core/src/lib.rs`. Update `patina-cli`'s
`cmd/apply.rs`, `cmd/managed.rs`, and `cmd/remove.rs` to call the hoisted
`patina_core` function and delete the local `pub(crate)` definition. This is
a pure no-behaviour-change refactor; the `deterministic_stdout` integration
test and the existing `timestamp_is_compact_utc` unit test (moved to
`patina-core`) are the regression guard.

<task-scenarios>
Given the hoisted helper,
when `patina_core::clock::current_timestamp()` is called,
then the result matches the `^\d{8}T\d{6}Z$` compact-UTC shape (8 date
digits, `T`, 6 time digits, `Z`).

Given the refactored CLI,
when `cargo build --workspace` and the existing `deterministic_stdout`
integration test run,
then they pass and no `current_timestamp` definition remains in
`patina-cli`.

Suggested files: `patina-core/src/clock.rs`, `patina-core/src/lib.rs`,
`patina-cli/src/cmd/apply.rs`, `patina-cli/src/cmd/managed.rs`,
`patina-cli/src/cmd/remove.rs`
</task-scenarios>
</task>

<task id="T-003" state="completed" covers="REQ-010">
## Windows `ERROR_SHARING_VIOLATION` retry-with-backoff wrapper in the apply pipeline

REQ-010 / DEC-006: on Windows, file-write operations that fail with
`ERROR_SHARING_VIOLATION` (Win32 code 32) retry with fixed exponential
backoff `[50, 100, 200, 400, 800, 1600]` ms (six retries, ~3.15s total),
then surface a typed error to the normal apply failure/rollback path. On
macOS and Linux the wrapper is a pass-through (the operation runs once, no
retry). Each retry emits a `fs_write_retry` debug `tracing` event with
fields `attempt`, `delay_ms`, `error`.

Create `patina-core/src/apply/retry.rs` exposing
`with_sharing_violation_retry<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T>`.
Under `#[cfg(windows)]` it inspects `err.raw_os_error() == Some(32)` (mirror
the kind+raw_os_error matching pattern already used in
`patina-core/src/lock.rs` for `fs2`'s contended-lock error) and retries on
that code only, re-raising any other error immediately and re-raising the
violation after the sixth failed attempt. Under `#[cfg(not(windows))]` it
calls `op()` exactly once. Route the engine write sites through it:
`apply/copy.rs` (`fs_err::copy` in `copy_file` ~line 30 and `copy_tree`
~line 74), `apply/template.rs` (`fs_err::write` of rendered output ~line
54), and symlink creation — the forward-apply executor
`apply/symlink.rs::create_symlink_os` (patina's primary materialization
write) plus the rollback/recovery `fsx.rs::symlink_to`. The constant
backoff schedule lives as a private slice in `retry.rs`.

<task-scenarios>
Given a macOS or Linux host,
when an apply writes a target through the wrapper and the underlying write
fails with an ordinary I/O error,
then the error surfaces on the first attempt and the `tracing` log contains
no `fs_write_retry` event (CHK-016).

Given a Windows host and a harness that holds the target open with
`FILE_SHARE_NONE` for ~250ms during an apply (run under
`RUST_LOG=patina_core=debug`),
when `patina apply --yes` writes that target,
then the apply completes with exit code 0 and at least one `fs_write_retry`
debug event with `attempt < 6` is present (CHK-015).

Given a Windows host where the target is held for ~10s,
when an apply attempts the write,
then after the six-retry budget (~3.15s) the wrapper re-raises the
violation and the apply fails/rolls back via the normal pipeline.

Suggested files: `patina-core/src/apply/retry.rs`,
`patina-core/src/apply/copy.rs`, `patina-core/src/apply/template.rs`,
`patina-core/src/apply/symlink.rs`, `patina-core/src/fsx.rs`,
`patina-core/tests/fs_retry.rs`
</task-scenarios>
</task>

<task id="T-004" state="completed" covers="REQ-007">
## Drift-cache format and module (postcard + version envelope, atomic write)

REQ-007: introduce the watcher's drift notification ledger at
`<state>/patina/drift.cache` — a `postcard`-encoded file with a `u16` major
version envelope at offset 0 (reusing the T-001 helper) and its OWN
`DRIFT_CACHE_MAJOR_VERSION` constant, independent of the journal's
`FILE_MAJOR_VERSION`. This task lands the format and its read/write/decode
surface only — no watcher and no notifications yet.

Create the `patina-core/src/watch/` module root (`watch/mod.rs`, minimal)
and `patina-core/src/watch/drift_cache.rs` with:

- `pub const DRIFT_CACHE_MAJOR_VERSION: u16` (start at `1`; the two formats
  version separately, so a journal bump must never force a drift-cache bump).
- A `DriftEntry { target: Utf8PathBuf, expected_hash: [u8; 32],
  actual_hash: [u8; 32], detected_at_unix: i64 }` (the `detected_at_unix`
  field is internal / non-user-facing) and a top-level `DriftCache`
  carrying the version envelope, the `journal_ts: String` this cache is
  bound to, and `entries: Vec<DriftEntry>`. `expected_hash` / `actual_hash`
  are 32-byte `blake3` digests, directly comparable to the journal's
  recorded hash (REQ-029).
- `encode`/`decode` over the T-001 `version_envelope` helper, a typed
  `DriftCacheError` (`thiserror`, `#[non_exhaustive]`) whose
  newer-major arm names both the found and supported versions, an
  atomic `write_drift_cache(path, &DriftCache)` (write to a sibling
  tempfile then rename, so a concurrent reader never sees a half-written
  file), a `load_drift_cache_file(path)` loader parallel to the journal's
  `load_plan_file` (`journal/render.rs`), and a `render_drift_cache`
  formatter parallel to `journal/render.rs::render_plan`.

Re-export `DRIFT_CACHE_MAJOR_VERSION`, `DriftCache`, `load_drift_cache_file`,
`render_drift_cache`, and `DriftCacheError` from `patina-core/src/lib.rs`.

<task-scenarios>
Given a `DriftCache` with one entry,
when it is `write_drift_cache`-d and then `load_drift_cache_file`-d back,
then the loaded value equals the original, including the bound `journal_ts`
and both 32-byte hashes.

Given a drift-cache file whose envelope major is
`DRIFT_CACHE_MAJOR_VERSION + 1`,
when `load_drift_cache_file` runs,
then it returns the typed newer-version `DriftCacheError` naming both the
found and supported majors.

Given the journal's `FILE_MAJOR_VERSION` and `DRIFT_CACHE_MAJOR_VERSION`,
when both are read,
then they are independent constants (a test asserts the drift cache decode
uses its own major and does not reference the journal's), proving the two
formats version separately.

Given a half-written tempfile scenario simulated by inspecting the write
path,
when `write_drift_cache` completes,
then the final bytes appear at the target path via rename (no in-place
truncation of the destination).

Suggested files: `patina-core/src/watch/mod.rs`,
`patina-core/src/watch/drift_cache.rs`, `patina-core/src/lib.rs`,
`patina-core/tests/drift_cache.rs`
</task-scenarios>
</task>

<task id="T-005" state="completed" covers="REQ-007">
## `patina debug drift-cache <path>` decode subcommand

REQ-007 (CHK-018): add a `patina debug drift-cache <path>` subcommand that
decodes a drift cache to human-readable form, parallel to the shipped
`patina debug journal <path>` (`patina-cli/src/cmd/debug.rs`,
`DebugCommand::Journal`). It prints the version envelope, the journal
timestamp the cache is bound to, and one block per entry naming the target
path, expected and actual hashes, and the human-rendered detection time. An
invalid path produces a typed error and exit 1; a cache from a newer Patina
(envelope major exceeds the running binary's) is refused with a typed error
naming both versions.

Extend the `DebugCommand` enum in `patina-cli/src/cli.rs` with a
`DriftCache(DebugDriftCacheArgs)` variant (a single `path: Utf8PathBuf`
positional, mirroring `DebugJournalArgs`), add a `DriftCache` arm to
`cmd/debug.rs::run` dispatching to a new `run_drift_cache` that calls
`patina_core::load_drift_cache_file` + `render_drift_cache` and emits via
the `Reporter`, with exit-code and error mapping identical to `run_journal`.

<task-scenarios>
Given a populated `<state>/patina/drift.cache` (the post-state of CHK-011),
when `patina debug drift-cache <state>/patina/drift.cache` runs,
then stdout contains the substrings `version:`, the bound journal
timestamp, the target path (`.gitconfig`), and both hash values, and the
process exits 0 (CHK-018).

Given a path that does not exist,
when `patina debug drift-cache /no/such/file` runs,
then the process exits 1 with a typed error naming the path.

Given a drift cache whose envelope major exceeds the running binary's,
when `patina debug drift-cache <path>` runs,
then the process exits 1 with a typed error naming both the found and
supported versions.

Suggested files: `patina-cli/src/cli.rs`, `patina-cli/src/cmd/debug.rs`,
`patina-cli/tests/debug_drift_cache_cli.rs`
</task-scenarios>
</task>

<task id="T-006" state="completed" covers="REQ-009">
## `<state>/patina/logs/` directory and the `tracing-appender` rotating-log stack

REQ-009 / DEC-009: SPEC-0003 owns `<state>/patina/logs/` and the watcher's
structured-log sink. `state_dir::resolve()` deliberately creates only
`journal/` and `backups/` (`patina-core/src/state_dir.rs`); it must NOT
create `logs/`. The watcher creates `logs/` lazily on first start and writes
its structured log there via `tracing-appender` with daily rotation,
keeping the 7 most recent files. There is no metrics subcommand, ring
buffer, or HTTP endpoint.

Add `tracing-appender` to `[workspace.dependencies]` (root `Cargo.toml`) and
to `patina-core/Cargo.toml`, and run `cargo deny check` (the hard rule
forbids adding a dependency without clearing `deny.toml`; resolve any
license/advisory/bans finding before proceeding). Create
`patina-core/src/watch/logging.rs` exposing a function that lazily creates
`<state>/patina/logs/` and builds a daily-rotating, keep-7
`RollingFileAppender`, returning the appender's `WorkerGuard` for the
watcher to hold for its process lifetime. The watcher's `tracing` subscriber
composes this file layer; in foreground mode (T-008) it also keeps a stderr
layer. Update the `state_dir.rs` module doc to note that `logs/` is created
by the watcher, not by `resolve`.

<task-scenarios>
Given a fresh state directory,
when `state_dir::resolve()` runs,
then `<state>/patina/journal/` and `<state>/patina/backups/` exist and
`<state>/patina/logs/` does NOT (resolve does not create it).

Given the logging stack,
when it is initialized against a state directory for the first time,
then `<state>/patina/logs/` is created and a daily-rotating appender writes
a log line into a file under it.

Given `tracing-appender` newly added,
when `cargo deny check` runs,
then it passes (license/advisory/bans clear).

Suggested files: `patina-core/src/watch/logging.rs`,
`patina-core/src/state_dir.rs`, `Cargo.toml`, `patina-core/Cargo.toml`,
`patina-core/tests/watch_logging.rs`
</task-scenarios>
</task>

<task id="T-007" state="completed" covers="REQ-005">
## Compute the watcher subscription set from the committed journal record

REQ-005 (CHK-009): the watcher subscribes only to paths the most recent
committed journal records — never recursively to the repository. It reads
the `<ts>.COMMIT` record (the `<ts>.plan` is deleted at commit) via the
already-exported `journal::read_latest_commit`, and from the recorded
`ApplyRecord` computes its subscription set: every target's canonical source
(`ExpectedTarget::source()`), plus every non-symlink (content) target path
(`ExpectedTarget::Content.target`), plus the `<state>/patina/journal/`
directory itself (the journal-rescan subscription). Symlink targets are NOT
separately subscribed (DEC-008) — modifying a symlinked target is modifying
its source, which is already watched.

Create `patina-core/src/watch/subscriptions.rs` with a pure function that
takes an `&ApplyRecord` and the resolved state directory and returns the
ordered, de-duplicated set of paths to watch, plus the journal directory. It
emits the computed set via a `tracing` info event so the foreground watcher
(T-008) and CHK-009 can inspect it. Keep this function free of `notify`
wiring — it is the pure mapping from record to path set; T-008 hands the set
to the debouncer.

<task-scenarios>
Given a synthetic `ApplyRecord` with two symlink targets and one content
(copy-mode) target,
when the subscription set is computed,
then it contains exactly the three source paths plus the one content target
path plus the `<state>/patina/journal/` directory, and contains neither
symlink target path (DEC-008) — five entries total (CHK-009).

Given an `ApplyRecord` whose only target is a symlink,
when the subscription set is computed,
then it contains the source path and the journal directory but no target
path.

Suggested files: `patina-core/src/watch/subscriptions.rs`,
`patina-core/src/watch/mod.rs`
</task-scenarios>
</task>

<task id="T-008" state="completed" covers="REQ-004 REQ-006">
## `patina watch --foreground`: event loop, 500ms debounce, clean shutdown

REQ-004 / REQ-006 / DEC-002 / DEC-011: stand up the `patina watch` command
group and the foreground watcher loop. The watcher runs on the existing
tokio runtime; `notify` / `notify-debouncer-full` deliver events on their
own OS thread, bridged into the async loop via a `tokio::sync::mpsc` channel,
and the core is a single `tokio::select!` awaiting either the next debounced
event batch or the shutdown signal (`tokio::signal::ctrl_c` on all
platforms, plus a `#[cfg(unix)]` SIGTERM arm) — per DEC-011. The debounce
window is the hardcoded `const DEBOUNCE: Duration = Duration::from_millis(500)`
(no config knob; a `[watcher] debounce_ms` key in root `patina.toml`
produces a typed warning, forward-compatible).

Add `notify` and `notify-debouncer-full` to `[workspace.dependencies]` and
`patina-core/Cargo.toml`, run `cargo deny check`, and add the `signal`
feature to `patina-cli`'s `tokio` (and `assert_cmd` / `predicates`
dev-dependencies if absent). Create `patina-core/src/watch/debounce.rs`
(the 500ms wrapper) and flesh out `patina-core/src/watch/mod.rs`'s
`run_foreground(shutdown)` to: resolve the state dir, init the T-006 logging
stack (file + stderr layers), build the T-007 subscription set, hand it to
the debouncer, and run the select-loop. On shutdown it logs a `shutdown`
event, releases subscriptions, and returns Ok. Wire the CLI: add
`Command::Watch` + a `WatchCommand` subcommand enum + `WatchArgs` (with
`--foreground`) in `patina-cli/src/cli.rs`, a new `patina-cli/src/cmd/watch.rs`
dispatching `--foreground` to `patina_core::watch::run_foreground`, register
`pub mod watch;` in `cmd/mod.rs`, and add the dispatch arm in `main.rs`.
`--foreground` does NOT acquire the exclusive advisory lock at the process
level (the watcher takes per-re-apply locks in T-009). This task wires the
loop and shutdown; the re-apply handler body lands in T-009 and the drift
handler in T-010 (here they may be no-op stubs that only log receipt).

<task-scenarios>
Given a test harness that spawns `patina watch --foreground` as a subprocess
with `RUST_LOG=patina_core=info` and a one-file repo,
when the harness inspects the subprocess stderr after startup,
then the logged subscription list is present and names the expected watched
paths (CHK-009 surface).

Given a running foreground watcher,
when the harness sends SIGINT (or Ctrl-C on Windows),
then the process exits 0 within 1 second and stderr contains the substring
`shutdown` (CHK-008).

Given a running foreground watcher,
when SIGTERM is sent on a POSIX host,
then it follows the same clean-exit path as SIGINT (exit 0, `shutdown`
logged).

Given a root `patina.toml` containing a `[watcher] debounce_ms` key,
when it is parsed,
then a typed warning is produced and the key is otherwise ignored (the
500ms debounce is hardcoded).

Suggested files: `patina-core/src/watch/mod.rs`,
`patina-core/src/watch/debounce.rs`, `patina-cli/src/cli.rs`,
`patina-cli/src/cmd/watch.rs`, `patina-cli/src/cmd/mod.rs`,
`patina-cli/src/main.rs`, `Cargo.toml`, `patina-core/Cargo.toml`,
`patina-cli/Cargo.toml`, `patina-cli/tests/watch_foreground_cli.rs`
</task-scenarios>
</task>

<task id="T-009" state="completed" covers="REQ-006 REQ-008">
## Watcher re-apply under `NonBlocking` lock, contention-skip, and journal rescan

REQ-006 / REQ-008 / DEC-007 (and SPEC-0001 REQ-030): on a debounced source
or journal-directory event, the watcher drives the engine re-apply under
`LockPolicy::NonBlocking` (`patina-core/src/apply/engine.rs`). It must NOT
pre-acquire the exclusive lock and then call apply — the engine self-acquires,
so pre-acquiring self-contends; it lets the engine acquire under
`NonBlocking`, which makes a single attempt and, on contention, returns the
typed contention error having mutated nothing (no plan/COMMIT/backups, and —
per the SPEC-0001 acquire-then-recover amendment — no orphan recovery
either). On that contention error the watcher logs a `lock_contention_skip`
event (debug, `skip.reason = "lock_held"`) and skips the cycle; the next FS
event re-arms the debounce. A successful re-apply emits an info `re_apply`
event with `re_apply.id`, `re_apply.duration_ms`, and `re_apply.files_changed`
(REQ-009 metric fields, written to the T-006 log stack), keying its journal
`<ts>` via the T-002 hoisted `current_timestamp`. When the triggering event
was on `<state>/patina/journal/` (a new `.plan` / `.COMMIT` from any apply),
the watcher re-reads the latest commit and recomputes its T-007 subscription
set; a self-triggered re-apply that produces an identical record must not
loop (re-reading the same record yields the same set; re-applying unchanged
source is a no-op).

Create `patina-core/src/watch/reapply.rs` and dispatch source/journal events
to it from `watch/mod.rs`'s select-loop.

<task-scenarios>
Given a running foreground watcher and a test that touches a watched source
file five times within 100ms then waits 1000ms,
when the watcher log is inspected,
then exactly one `re_apply` event is present (the 500ms debounce coalesced
the burst — CHK-010).

Given a running foreground watcher and a synchronous concurrent
`patina apply --yes` launched while the watcher is mid-re-apply (holding the
lock),
when both complete,
then the watcher log contains zero `lock_contention_skip` events for the
CLI's apply (the CLI blocked behind the watcher) and the journal contains
exactly two committed COMMIT records (CHK-013).

Given a running foreground watcher and a parallel process that runs
`patina apply --yes` (writing a new `.plan` and `.COMMIT`),
when 1000ms passes after the CLI exits,
then the watcher logs a journal-rescan event naming the new `.COMMIT`
filename and its subscription set reflects the new journal (CHK-017), and no
unbounded re-apply loop occurs.

Given a watcher whose `NonBlocking` re-apply loses the lock race,
when the contention error returns,
then the watcher logs one `lock_contention_skip` (`skip.reason="lock_held"`)
and performs no filesystem mutation for that cycle.

Suggested files: `patina-core/src/watch/reapply.rs`,
`patina-core/src/watch/mod.rs`,
`patina-cli/tests/watch_foreground_cli.rs`
</task-scenarios>
</task>

<task id="T-010" state="completed" covers="REQ-007">
## Drift detection: hash-compare non-symlink targets, notify, rate-limit, cache write

REQ-007 / DEC-003 / DEC-004 / DEC-008 / DEC-013: for every non-symlink target
in the journal, the watcher subscribes to the target path; when its FS event
fires, the watcher reads the live bytes, computes the `blake3` hash via the
exported `content_hash` (`patina-core/src/journal/record.rs`), and compares
it to the journal-recorded `ExpectedTarget::Content` hash. On divergence it:
emits a desktop notification (title "Patina: drift detected", body naming the
target path), writes a `drift` warn event (`drift.path`,
`drift.expected_hash`, `drift.actual_hash`), and upserts the
`<state>/patina/drift.cache` (T-004) atomically — all gated by a per-target
rate limit of at most one notification per 60-second window (DEC-004, keyed
on the entry's `detected_at_unix`). A FS event on a symlink target is NOT
processed (DEC-008). The drift cache is the watcher's notification ledger and
is NEVER read by `patina status` — `status` derives DRIFTED independently
from SPEC-0001 REQ-018's own live re-hash (`status/classify.rs`), so an
edited-then-reverted file reports CLEAN even while the cache holds the
intervening edit.

Per DEC-013, the notification emit path sits behind a small internal trait:
the production impl calls `notify-rust`; a test impl records `(title, body)`
tuples in memory so the scenarios assert deterministically on headless CI.
Add `notify-rust` to `[workspace.dependencies]` and `patina-core/Cargo.toml`
and run `cargo deny check` — its transitive notification stack (zbus/DBus,
mac-notification-sys, WinRT) must clear `deny.toml` at this gate before the
requirement is built on it (DEC-013); resolve any finding before proceeding.
Create `patina-core/src/watch/drift.rs` and dispatch target events to it from
`watch/mod.rs`.

<task-scenarios>
Given an applied copy-mode `~/.gitconfig` (recorded hash H1), a running
foreground watcher with a capture notification sink, and a test that
overwrites the target with content hashing to H2 ≠ H1,
when 1000ms passes,
then `<state>/patina/drift.cache` contains an entry for `~/.gitconfig` with
`expected_hash = H1`, `actual_hash = H2`, and the capture sink recorded
exactly one notification (CHK-011).

Given the same scenario where the user touches the drifted file twice within
60 seconds,
when both touches complete,
then the capture sink recorded at most one notification (the second is
rate-limited — DEC-004).

Given a symlink target whose source is modified,
when the FS event fires on the target path,
then no drift detection runs and no notification is emitted (DEC-008).

Given the post-state of CHK-011 (target holds H2),
when `patina status --json` runs,
then the `files` array reports `.gitconfig` with `state = "drifted"` derived
from the live re-hash, NOT from the drift cache (CHK-012), and the file
reverted to H1 would report CLEAN even with a stale cache entry present.

Suggested files: `patina-core/src/watch/drift.rs`,
`patina-core/src/watch/mod.rs`, `Cargo.toml`, `patina-core/Cargo.toml`,
`patina-cli/tests/watch_foreground_cli.rs`
</task-scenarios>
</task>

<task id="T-011" state="completed" covers="REQ-001 REQ-003">
## Service abstraction, cross-platform `watch` lifecycle CLI, and the macOS LaunchAgent backend

REQ-001 / REQ-003: introduce the per-OS service abstraction and the lifecycle
command surface, with the macOS backend as the first concrete implementation
(locally testable on the dev platform). Create
`patina-core/src/watch/service/mod.rs` with a `ServiceBackend` trait
(`install`, `uninstall`, `start`, `stop`, `restart`, `status`) returning a
typed `ServiceError`, and a `current()` factory dispatching on
`state_dir::HostOs`. Install must point the service at the running binary's
canonical absolute path (`std::env::current_exe` → canonicalize) invoked with
`watch --foreground`.

Create `patina-core/src/watch/service/launchd.rs` (`#[cfg(target_os = "macos")]`):
write `~/Library/LaunchAgents/com.patina.watcher.plist` (mode 0644,
`RunAtLoad = true`, `KeepAlive` for on-failure restart, `ProgramArguments`
→ canonical binary + `watch --foreground`); `install` invokes
`launchctl bootstrap gui/$(id -u) <plist>`; `start`/`stop` invoke
`launchctl start`/`stop com.patina.watcher`; `uninstall` stops then
`launchctl bootout` and removes the plist; `status` queries `launchctl print`
for liveness, last-fired, last-exit. Extend `patina-cli/src/cmd/watch.rs`
with the `install` / `uninstall` (`--yes`) / `start` / `stop` / `restart` /
`status` arms over the trait, with `--json` envelopes whose `result` field
is `installed` / `uninstalled` etc. and a `status` object containing
`installed`, `running`, `last_fired_at`, `last_exit_code`,
`subscriptions_count`, `re_applies_since_start`. Per DEC-012, `status`
recovers `subscriptions_count` / `re_applies_since_start` by reading the most
recent rotated log under `<state>/patina/logs/` (reporting `null` when
absent), and the supervisor-derived fields from `launchctl print`. All
lifecycle subcommands except `status` acquire the exclusive advisory lock
(SPEC-0001 REQ-023); `status` acquires the shared lock. Lifecycle subcommands
on a not-installed service are no-ops with a clear stderr message, not
supervisor errors. `install` on an already-installed service exits 1 with a
typed error.

<task-scenarios>
Given a macOS test host with no prior installation,
when `patina watch install --json` runs as a normal user,
then `~/Library/LaunchAgents/com.patina.watcher.plist` exists with mode
0644, the JSON `result` is `installed`, and `launchctl print
gui/$(id -u)/com.patina.watcher` reports the service (CHK-001).

Given a macOS test host with the service installed but stopped,
when `patina watch status --json` runs,
then the JSON contains `installed = true`, `running = false`, and
`last_exit_code` is the supervisor's most recent value or `null` (CHK-006).

Given no installed service,
when `patina watch start` runs,
then stderr names "service not installed; run `patina watch install` first"
and the exit code is 1.

Given an already-installed service,
when `patina watch install` runs again,
then it exits 1 with a typed already-installed error.

Suggested files: `patina-core/src/watch/service/mod.rs`,
`patina-core/src/watch/service/launchd.rs`, `patina-cli/src/cli.rs`,
`patina-cli/src/cmd/watch.rs`, `patina-cli/tests/watch_service_cli.rs`
</task-scenarios>
</task>

<task id="T-012" state="completed" covers="REQ-001 REQ-003">
## Linux `systemd --user` backend and the non-systemd `--foreground` fallback

REQ-001 / REQ-003 / DEC-005 / DEC-010: implement the Linux service backend
behind the T-011 `ServiceBackend` trait. Create
`patina-core/src/watch/service/systemd.rs` (`#[cfg(target_os = "linux")]`):
`install` writes `~/.config/systemd/user/patina-watcher.service` (a valid
unit with `Restart=on-failure`, `WantedBy=default.target`,
`ExecStart=<canonical binary> watch --foreground`) then invokes
`systemctl --user enable --now patina-watcher.service`; `start`/`stop` invoke
`systemctl --user start`/`stop`; `restart` is stop-then-start; `uninstall`
stops, `systemctl --user disable`, and removes the unit file; `status`
queries `systemctl --user` for liveness/last-fired/last-exit. Per DEC-005,
neither `install` nor `uninstall` invokes `loginctl enable-linger` /
`disable-linger`, and there is no `--linger` flag. Create
`patina-core/src/watch/service/unsupported.rs` as the fallback the factory
returns when `systemd --user` is unavailable, whose lifecycle methods return
a typed error directing the user to `patina watch --foreground` under their
own supervisor (DEC-010).

<task-scenarios>
Given a Linux test host with `systemd --user` available,
when `patina watch install --json` runs as a normal user,
then `~/.config/systemd/user/patina-watcher.service` exists, the JSON
`result` is `installed`, and `systemctl --user is-active
patina-watcher.service` returns `active` (CHK-002).

Given a Linux test host with the service installed and running,
when `patina watch uninstall --yes --json` runs,
then the unit file no longer exists, `systemctl --user list-unit-files
patina-watcher.service` reports nothing, and the JSON `result` is
`uninstalled` (CHK-005).

Given a host where `systemd --user` is unavailable,
when `patina watch install` runs,
then it returns the typed unsupported-init error pointing at
`patina watch --foreground`, and no unit file is written (DEC-010).

Suggested files: `patina-core/src/watch/service/systemd.rs`,
`patina-core/src/watch/service/unsupported.rs`,
`patina-core/src/watch/service/mod.rs`,
`patina-cli/tests/watch_service_cli.rs`
</task-scenarios>
</task>

<task id="T-013" state="completed" covers="REQ-001 REQ-003">
## Windows per-user Scheduled Task backend via `winsafe` `taskschd`

REQ-001 / REQ-003: implement the Windows service backend behind the T-011
trait. Enable the `taskschd` feature on `patina-core`'s Windows `winsafe`
dependency — `patina-core/Cargo.toml`'s
`[target.'cfg(windows)'.dependencies] winsafe` currently has
`features = ["shell"]`; add `"taskschd"` (SPEC-0002 deliberately deferred
this feature to SPEC-0003). The Scheduled Task is HKCU-scoped and
non-elevated, so it lives in `patina-core`, NOT the elevation-only
`patina-elevate` helper. Create
`patina-core/src/watch/service/scheduled_task.rs` (`#[cfg(windows)]`):
`install` registers a per-user Scheduled Task named `Patina Watcher` with a
logon trigger, `RunLevel = Limited` (non-elevated), and an action pointing at
the canonical binary with `watch --foreground`, via `winsafe`'s `taskschd`
APIs (the same HKCU-scoped surface `schtasks /create /sc onlogon` uses) —
mirror the registry-access pattern already in `patina-core/src/windows/`.
`start` runs the task, `stop` ends it, `restart` is stop-then-start,
`uninstall` deletes the task; `status` queries the task for
liveness/last-run/last-exit. None of these require admin.

<task-scenarios>
Given a Windows test host running as a standard (non-admin) user,
when `patina watch install --json` runs,
then a HKCU-scoped Scheduled Task named `Patina Watcher` exists with an
`OnLogon` trigger and `RunLevel = Limited`, and the JSON `result` is
`installed` (CHK-003).

Given a Windows host with the task installed,
when `patina watch uninstall --yes` runs,
then the `Patina Watcher` task no longer exists under HKCU scope and the
command exits 0 without requiring elevation.

Given a non-Windows build,
when the workspace compiles,
then the `taskschd`-gated code is excluded by `#[cfg(windows)]` and does not
affect the macOS/Linux build.

Suggested files: `patina-core/src/watch/service/scheduled_task.rs`,
`patina-core/src/watch/service/mod.rs`, `patina-core/Cargo.toml`,
`patina-cli/tests/watch_service_cli.rs`
</task-scenarios>
</task>

<task id="T-014" state="completed" covers="REQ-001">
## Document the watch service, the Linux linger opt-in, and the drift-cache decode

REQ-001 / DEC-005 and the "never let docs drift" hard rule: document the
watch subsystem in `docs/USER_GUIDE.md` (the cross-SPEC-referenced docs
target). Add a watch-service section covering `patina watch install` /
`uninstall` / `start` / `stop` / `restart` / `status` and `--foreground`,
the per-OS service locations
(`~/Library/LaunchAgents/com.patina.watcher.plist`,
`~/.config/systemd/user/patina-watcher.service`, the Windows `Patina
Watcher` Scheduled Task), and the non-systemd `--foreground`-under-your-own-
supervisor path (DEC-010). Include the exact `sudo loginctl enable-linger
$USER` one-shot snippet for Linux users who want the watcher to survive
logout, with the one-line explanation that Patina does not run it for them
and ships no `--linger` flag (DEC-005). Document `patina debug drift-cache
<path>` alongside the existing `patina debug journal` reference, and note
that drift surfaces both as a desktop notification (when the watcher runs)
and as `DRIFTED` in `patina status` (always), resolved via `patina apply`
(revert to source) or `patina promote` (update source from target).

<task-scenarios>
Given `docs/USER_GUIDE.md` at HEAD after this task,
when the watch-service section is scanned,
then it names all six lifecycle subcommands plus `--foreground`, the three
per-OS service locations, and contains the literal `sudo loginctl
enable-linger $USER` snippet with the no-`--linger`-flag note.

Given the docs structure test (`patina-cli/tests/docs_structure.rs`) and
`cargo doc --workspace --no-deps -D warnings`,
when they run after the docs edit,
then both pass (the named structural anchors REQ-027 formalizes are intact
and no intra-doc link is broken).

Suggested files: `docs/USER_GUIDE.md`,
`patina-cli/tests/docs_structure.rs`
</task-scenarios>
</task>
