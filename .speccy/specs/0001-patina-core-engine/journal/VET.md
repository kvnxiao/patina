---
spec: SPEC-0001
generated_at: 2026-05-30T05:10:54Z
---

## Invocation 1 — 2026-05-30T05:10:54Z

<drift-review verdict="pass" round="1" date="2026-05-30T14:32:10-07:00" model="claude-opus-4-8[1m]/low">
T-027 faithfully and completely satisfies REQ-030: the apply path (`execute_plan` / library `apply()`) now takes a `LockPolicy` with the three SPEC-named variants, the default `Blocking` arm is byte-for-byte the pre-amendment `acquire_lock(.., Exclusive, exclusive_timeout())`, `NonBlocking` returns the new `LockError::Contended` before `flush_plan_and_fsync` (CHK-065, test green), and `Held(guard)` consumes the caller's exclusive guard without re-acquiring (CHK-066, test green). No drift found; one pre-existing, out-of-scope observation is surfaced below for the SPEC author to weigh before SPEC-0003 builds on `NonBlocking`.

Verified clean (not blocking):
- REQ-030 done-when bullets 1-4 all hold. CLI `apply.rs` passes `LockPolicy::Blocking` at both the human and `--json` `execute_plan` sites; `patina apply`/`rollback` exit-4-on-lock-timeout is preserved — `exit_code.rs:70` maps only `LockError::Timeout` to exit 4, and `Contended` correctly falls through to Generic (exit 1), so REQ-022/REQ-023 are unchanged. See `patina-cli/src/cmd/apply.rs:110` and `patina-cli/src/exit_code.rs:70`.
- `run_rollback` is correctly left untouched (still self-acquires `Blocking`) — TASKS.md T-027 explicitly scopes the policy to the apply entry points only, so this is by design, not a missed call site. See `patina-core/src/rollback/mod.rs:118`.
- No scope creep: the only new public API is `LockPolicy` (+ re-export) and `lock::try_acquire`, plus the `LockError::Contended` variant on the already-`#[non_exhaustive]` enum — all named in REQ-030/T-027. No new dependency (`try_acquire` reuses the existing `fs2`/`is_contended` path). See `patina-core/src/lock.rs:282` and `patina-core/src/apply/engine.rs:90`.
- Cross-SPEC contract matches: the SPEC-0002 (`Held`) and SPEC-0003 (`NonBlocking`) amendments reference exactly the variant shapes T-027 ships (caller-supplied guard for `Held`; single-attempt typed contention error with zero new-apply mutation for `NonBlocking`).

Observation for the human (pre-existing, not T-027 drift — flagged, not blocking):
- REQ-030 NonBlocking done-when (`writes nothing to the filesystem`) — `recover_orphans` (`patina-core/src/apply/engine.rs:337`) runs and can mutate the filesystem (reverse a prior orphan's backups, delete orphaned plan/progress files) *before* the lock is resolved at engine.rs:342, so a contended `NonBlocking` apply that also finds an orphan would mutate before returning the contention error. T-027 satisfies the done-when's enumerated artifacts (no new `<ts>.plan`/`<ts>.COMMIT`/backup — the early return precedes `flush_plan_and_fsync`), and the recover-before-lock ordering is pre-existing (unchanged by the amendment), and `NonBlocking` has no consumer until the SPEC-0003 watcher lands. But the broader "writes nothing" prose is in tension with recover-before-lock. The SPEC author should decide whether to (a) move recovery under the lock, (b) tighten the REQ-030 prose to scope "writes nothing" to the new apply's artifacts only, or (c) accept it before SPEC-0003 wires the watcher to `NonBlocking`. CHK-065's test scene uses an empty journal dir, so this interaction is uncovered by design. See `patina-core/src/apply/engine.rs:337` and `patina-core/src/journal/recovery.rs:101`.
</drift-review>

<simplifier-scan verdict="clean">
No behavior-preserving simplification earns its place; the SPEC-0001 REQ-030 lock-policy diff is already minimal and conventional. The shared open-file logic is correctly extracted into `open_lock_file`, the `LockGuard` construction duplicated between `acquire` and `try_acquire` (patina-core/src/lock.rs:236 and :285) sits inside genuinely different control flow (deadline-poll loop vs single-shot) so factoring it would add cognitive load rather than remove it, and the `LockPolicy` enum, match arms, re-exports, and CLI wiring are each boring and direct.
</simplifier-scan>

<gate verdict="passed" tasks_hash="6bc403d7f630c18ee8235a8fa4a830423e94d794dc8d459459e914dfabe35072" date="2026-05-30T05:15:01Z">
Drift cleared on round 1 (one pre-existing non-blocking observation surfaced re: recover-before-lock vs NonBlocking "writes nothing"); simplifier scan clean; gate passed.
</gate>
