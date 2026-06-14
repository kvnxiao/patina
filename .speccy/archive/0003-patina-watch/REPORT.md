---
spec: SPEC-0003
outcome: implemented
generated_at: 2026-06-01T00:00:00Z
---

# REPORT: SPEC-0003 Patina watch — filesystem event loop, per-OS service install, drift detection

<report spec="SPEC-0003">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002 CHK-003">
T-011 introduced the `ServiceBackend` trait, the lifecycle CLI subcommands (`install`, `uninstall`, `start`, `stop`, `restart`, `status`), and the macOS LaunchAgent backend (`launchd.rs`): writes `~/Library/LaunchAgents/com.patina.watcher.plist` (mode 0644, `RunAtLoad = true`, `KeepAlive`, `ProgramArguments` pointing at canonical binary with `watch --foreground`) and invokes `launchctl bootstrap gui/$(id -u) <plist>` to load it; exits 1 with a typed `AlreadyInstalled` error if the service is already present. T-012 added the Linux `systemd --user` backend: writes `~/.config/systemd/user/patina-watcher.service` (`Restart=on-failure`, `WantedBy=default.target`, canonical `ExecStart`) and invokes `systemctl --user enable --now`. T-013 added the Windows per-user Scheduled Task backend via `winsafe` `taskschd`, registering the `Patina Watcher` task under HKCU scope with a logon trigger and `RunLevel = Limited` (non-elevated). All three backends install without admin or sudo. Per DEC-005, no backend invokes `loginctl enable-linger`; the `docs/USER_GUIDE.md` watch-service section (T-014) carries the `sudo loginctl enable-linger $USER` one-shot snippet. CHK-001 (macOS), CHK-002 (Linux), CHK-003 (Windows) cover the install path. Retry count: 3 (T-011: 1, T-012: 1, T-013: 1).
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006">
T-011 wired `uninstall --yes` (stops then `launchctl bootout` and removes the plist), `start`, `stop`, `restart`, and `status` over the `ServiceBackend` trait; `status --json` emits `installed`, `running`, `last_fired_at`, `last_exit_code`, `subscriptions_count`, `re_applies_since_start`. The `subscriptions_count`/`re_applies_since_start` counters are recovered from the most recent rotated log under `<state>/patina/logs/` (DEC-012); supervisor-derived fields come from `launchctl print`. Lifecycle subcommands on a not-installed service emit a clear stderr message and exit 1. T-012 implemented the Linux equivalents via `systemctl --user` (CHK-005 covers uninstall; CHK-006 covers status). T-013 implemented the Windows task equivalents. All lifecycle subcommands except `status` acquire the exclusive advisory lock (SPEC-0001 REQ-023); `status` acquires the shared lock. Retry count: 3 (T-011: 1, T-012: 1, T-013: 1).
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-007 CHK-008">
T-008 stood up `patina watch --foreground`: the watcher runs on the existing tokio runtime, bridging `notify`/`notify-debouncer-full`'s OS-thread callback into a `tokio::select!` loop via a `tokio::sync::mpsc` channel, with `tokio::signal::ctrl_c` on all platforms and a `#[cfg(unix)]` SIGTERM arm (DEC-011). The foreground process acquires no long-held exclusive advisory lock. On shutdown it logs a `shutdown` event, releases subscriptions, and returns Ok. The integration test `sigint_shuts_down_cleanly` verifies CHK-008 (exit 0 within 1s, stderr contains `shutdown`); the subscription-listing startup behaviour satisfies the CHK-007 surface. Retry count: 1 (T-008: 1).
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-009 CHK-017">
T-007 introduced `patina-core/src/watch/subscriptions.rs`: a pure function taking an `&ApplyRecord` and the resolved state directory that returns the ordered, de-duplicated subscription set — every target's canonical source path, every non-symlink content target path, and the `<state>/patina/journal/` directory itself. Symlink targets are not separately subscribed (DEC-008). T-009 wired the journal-rescan handler: when a new `.plan` or `.COMMIT` appears under `<state>/patina/journal/`, the watcher acquires the shared advisory lock, re-reads the latest committed journal via `journal::read_latest_commit`, and recomputes its subscription set. A self-triggered re-apply that produces an identical record does not loop. CHK-009 (subscription count = three source paths + one content target path + journal dir) and CHK-017 (journal-rescan event naming the new `.COMMIT` filename) are exercised by integration tests. Retry count: 1 (T-007: 1).
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-010">
T-008 introduced `patina-core/src/watch/debounce.rs` with `const DEBOUNCE: Duration = Duration::from_millis(500)` (no config knob; a `[watcher] debounce_ms` key in root `patina.toml` produces a typed warning, forward-compatible). T-009 wired the re-apply handler under `LockPolicy::NonBlocking`: on a contention error the watcher logs a `lock_contention_skip` debug event (`skip.reason = "lock_held"`) and skips the cycle; the next FS event re-arms the debounce. CHK-010 (five touches within 100ms produce exactly one re-apply event after 1000ms) is exercised by the `debounce_coalesces_burst_into_one_reapply` integration test. Retry count: 2 (T-008: 1, T-009: 1).
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-011 CHK-012 CHK-018">
T-001 extracted `patina-core/src/version_envelope.rs` with a format-agnostic `encode_with_envelope`/`decode_envelope` helper reused by the journal and the drift cache. T-004 introduced `patina-core/src/watch/drift_cache.rs`: postcard-encoded with an independent `DRIFT_CACHE_MAJOR_VERSION = 1`, `DriftEntry` carrying `target`, `expected_hash`, `actual_hash`, `detected_at_unix`, atomic tempfile-then-rename write, and a typed `DriftCacheError::VersionMismatch { found, supported }`. T-005 added `patina debug drift-cache <path>` parallel to `patina debug journal`. T-010 implemented drift detection: on a FS event for a non-symlink target, computes the live `blake3` hash, compares to the journal-recorded hash, and on divergence emits a desktop notification via a `NotifySink` trait (production: `notify-rust`; tests: capture sink per DEC-013), writes a `drift` warn event, and upserts the drift cache atomically under a per-target 60-second rate limit (DEC-004). Symlink targets are excluded (DEC-008). The drift cache is never read by `patina status`; status derives DRIFTED from SPEC-0001 REQ-018's live re-hash independently (CHK-012). CHK-011, CHK-012, and CHK-018 are all exercised by integration tests. Retry count: 2 (T-001: 1, T-010: 1).
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-013">
T-009 wired the re-apply cycle under SPEC-0001 REQ-030's `LockPolicy::NonBlocking`: the engine self-acquires the lock with a single attempt and, on contention, returns a typed contention error having mutated nothing (per the SPEC-0001 acquire-then-recover amendment, no orphan recovery runs either). The watcher logs `lock_contention_skip` on each skip. The watcher never pre-acquires the lock and does not hold it across debounce cycles. CHK-013 (watcher wins the lock, Blocking CLI blocks and waits, journal contains exactly two committed records) is verified at the engine level by SPEC-0001's `non_blocking_contention_*` unit tests and at the watcher level by the `cli_blocks_while_watcher_holds_lock` integration test. Retry count: 1 (T-009: 1).
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-014">
T-006 added `tracing-appender` and introduced `patina-core/src/watch/logging.rs`: lazily creates `<state>/patina/logs/` on first start and builds a daily-rotating, keep-7 `RollingFileAppender`, returning the `WorkerGuard` for the watcher to hold for its process lifetime. The watcher's `tracing` subscriber composes this file layer; in foreground mode it also keeps a stderr layer. T-009 emits the required structured events: info-level `re_apply` with `re_apply.id`, `re_apply.duration_ms`, `re_apply.files_changed`; warn-level `drift` with `drift.path`, `drift.expected_hash`, `drift.actual_hash`; debug-level `lock_contention_skip` with `skip.reason = "lock_held"`. No metrics subcommand, ring buffer, or HTTP endpoint is exposed. CHK-014 (three distinct `re_apply` info events with distinct `re_apply.id` fields) is exercised by the `metrics_three_reapplies_logged` integration test. Retry count: 0.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-015 CHK-016">
T-003 introduced `patina-core/src/apply/retry.rs` with `with_sharing_violation_retry<T>`: under `#[cfg(windows)]` it retries on `raw_os_error() == Some(32)` with the fixed schedule `[50, 100, 200, 400, 800, 1600]` ms (six retries, ~3.15s total), logging a `fs_write_retry` debug event per attempt with `attempt`, `delay_ms`, `error`; on exhaustion it re-raises the violation to the apply failure/rollback path. Under `#[cfg(not(windows))]` it calls `op()` exactly once. The wrapper is routed through all four engine write sites: `apply/copy.rs` (`copy_file`, `copy_tree`), `apply/template.rs` (rendered output write), `apply/symlink.rs::create_symlink_os` (forward-apply primary materialization), and `fsx.rs::symlink_to` (rollback/recovery). CHK-015 (Windows: apply succeeds with retries visible in log) and CHK-016 (Linux: no retry events, write fails immediately with OS error) are covered by `patina-core/tests/fs_retry.rs`. Retry count: 1 (T-003: 1).
</coverage>

</report>

## Notes

T-002 hoisted `current_timestamp()` from `patina-cli/src/cmd/apply.rs` to `patina-core/src/clock.rs` so both the CLI apply path and the watcher re-apply key journal timestamps via one definition. This was a pure no-behaviour-change refactor absorbed by REQ-006 (the watcher re-apply depends on it).

T-014 documented the watch subsystem in `docs/USER_GUIDE.md` — service locations, lifecycle subcommands, the non-systemd `--foreground`-under-supervisor path (DEC-010), the Linux linger one-shot snippet (DEC-005), and the `patina debug drift-cache` reference. Coverage is absorbed by REQ-001 (the `docs/USER_GUIDE.md` linger snippet is an explicit done-when of REQ-001).
