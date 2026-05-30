---
spec: SPEC-0001
outcome: implemented
generated_at: 2026-05-30T00:00:00Z
---

# REPORT: SPEC-0001 Patina core engine — transactional apply with apply/status/rollback CLI

<report spec="SPEC-0001">

## Summary

SPEC-0001 shipped originally on the `SPEC-0001: patina core engine` PR (T-001 through T-026). It was reopened on 2026-05-30 by a single-task amendment (REQ-030 / T-027) that added a `LockPolicy` enum to the engine apply entry points, unblocking SPEC-0002 (`Held` variant) and SPEC-0003 (`NonBlocking` variant). T-027 closed in two rounds of review; the vet gate passed on invocation 1 with zero drift-fix rounds.

Total tasks this amendment round: 1. Total retries: 1 (round-1 style blocker — test module renamed `lock_policy_tests` → `tests`).

## Requirement coverage

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002">
Cargo workspace with `patina-core` library and `patina-cli` binary. Both crates declare `edition = "2024"`, `rust-version = "1.95"`, and `license = "MIT"`. Covered by T-001.
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-003 CHK-004">
`patina-core` is an async tokio library. Public entry points are `async fn`. `patina-cli` uses `#[tokio::main]`. Covered by T-001.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006 CHK-007">
Repository discovery via `PATINA_REPO`, walk-up, persisted default — no `--repo` flag. Covered by T-003.
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-008 CHK-009">
Flat two-level module structure; deep or double-root `patina.toml` files rejected with typed errors. Covered by T-003.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-010 CHK-011 CHK-012 CHK-041 CHK-042 CHK-043 CHK-044 CHK-045 CHK-046 CHK-047">
Five file modes (symlink, symlink-dir, copy, copy-tree, template render); single and multi-target fan-out; parse errors on invalid mode, both/neither/empty target(s). Covered by T-004 and T-014.
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-013 CHK-014">
`[[hook]]` schema with `pre_apply`/`post_apply` events; `must_succeed` defaults true; `on_change` rejected at parse. Covered by T-004 and T-015.
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-015 CHK-016 CHK-040">
Variable precedence chain; reserved `patina.*` namespace; `patina.env.*` dynamic map. Covered by T-006.
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-017 CHK-018">
Profile resolution chain: env, persisted, auto-match, fallback. No `--profile` flag. Covered by T-007.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-019 CHK-020">
MiniJinja strict-undefined renders templates and evaluates `when` expressions; undefined references surface typed errors. Covered by T-008.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-021">
Path canonicalization — absolute on read with lexical fallback for non-existent paths. Journal stores only canonical absolute paths. Covered by T-009.
</coverage>

<coverage req="REQ-011" result="satisfied" scenarios="CHK-022">
Single-fsync upfront postcard journal with version envelope; plan written and fsync'd before any mutation; plan deleted only after COMMIT is written. Covered by T-010.
</coverage>

<coverage req="REQ-012" result="satisfied" scenarios="CHK-023">
Per-operation progress cursor written without fsync; recovery probes filesystem rather than trusting the cursor. Covered by T-010.
</coverage>

<coverage req="REQ-013" result="satisfied" scenarios="CHK-024">
Crash recovery probes filesystem, reverses completed ops via backups, always restores pre-apply state. Recovery is idempotent. Covered by T-011.
</coverage>

<coverage req="REQ-014" result="satisfied" scenarios="CHK-025">
Backups taken before overwrite to per-machine state directory; dotfiles repository never written during apply. Covered by T-012.
</coverage>

<coverage req="REQ-015" result="satisfied" scenarios="CHK-026">
Backup retention keeps the last ten apply cycles; older cycles GC'd on next successful apply. No `patina gc` command. Covered by T-012.
</coverage>

<coverage req="REQ-016" result="satisfied" scenarios="CHK-027">
Per-machine state directory uses OS-appropriate locations (XDG on Linux, `Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows). Covered by T-005.
</coverage>

<coverage req="REQ-017" result="satisfied" scenarios="CHK-028 CHK-029 CHK-030">
`patina apply` prompts in TTY, exits without mutation in non-TTY, accepts `--yes` and `--force-deploy`. `--json` alone previews; `--json --yes` applies. Covered by T-016.
</coverage>

<coverage req="REQ-018" result="satisfied" scenarios="CHK-031 CHK-032 CHK-048">
`patina status` classifies managed files as CLEAN / DRIFTED / MISSING / ORPHANED. Multi-target entries report one row per target. Covered by T-017.
</coverage>

<coverage req="REQ-019" result="satisfied" scenarios="CHK-033 CHK-049">
`patina rollback` reverses the last successful apply via journal and backups; multi-target entries roll back atomically. Covered by T-018.
</coverage>

<coverage req="REQ-020" result="satisfied" scenarios="CHK-034 CHK-050">
`patina debug` subcommand group; `patina debug journal <path>` decodes a binary plan into human-readable form. Covered by T-019.
</coverage>

<coverage req="REQ-021" result="satisfied" scenarios="CHK-035">
Stdout output is deterministic — no wall-clock timestamps in human-readable output; two consecutive applies produce byte-identical stdout. Covered by T-021.
</coverage>

<coverage req="REQ-022" result="satisfied" scenarios="CHK-036">
Exit codes formalized: 0 success, 1 generic, 2 pre_apply abort, 3 post_apply rollback, 4 lock timeout, 5 user declined. Covered by T-020.
</coverage>

<coverage req="REQ-023" result="satisfied" scenarios="CHK-037">
Advisory file lock coordinates mutations and read-only commands; exclusive for mutating subcommands, shared for status. Covered by T-013.
</coverage>

<coverage req="REQ-024" result="satisfied" scenarios="CHK-038 CHK-039">
No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, or `unimplemented!` in production code; enforced by Clippy. Covered by T-002.
</coverage>

<coverage req="REQ-025" result="satisfied" scenarios="CHK-051 CHK-052">
CI matrix runs the full test suite on macOS, Linux, and Windows. All three are required-status-checks. Covered by T-022.
</coverage>

<coverage req="REQ-026" result="satisfied" scenarios="CHK-053 CHK-054 CHK-055">
User-facing output flows through `output::Reporter`; raw print macros denied by Clippy outside the `output` module. Covered by T-023.
</coverage>

<coverage req="REQ-027" result="satisfied" scenarios="CHK-056 CHK-057 CHK-058">
`docs/ARCHITECTURE.md` and `docs/USER_GUIDE.md` with named structural anchors; cloud-sync providers listed under `## State directory`. Covered by T-024.
</coverage>

<coverage req="REQ-028" result="satisfied" scenarios="CHK-059 CHK-060 CHK-061">
`deny.toml` configured with `[licenses]`, `[advisories]`, `[bans]`, `[sources]`; `cargo deny check` gates CI. Covered by T-025.
</coverage>

<coverage req="REQ-029" result="satisfied" scenarios="CHK-062 CHK-063 CHK-064">
Committed apply record retains canonical source path and blake3 content hash per target; shared version-envelope major bumped to 2. Covered by T-026.
</coverage>

<coverage req="REQ-030" result="satisfied" scenarios="CHK-065 CHK-066 CHK-067">
Engine apply entry points accept a `LockPolicy` with `Blocking` (default, byte-for-byte pre-amendment), `NonBlocking` (single attempt, typed contention error, zero mutation), and `Held` (caller-supplied guard, no re-acquire). `patina apply` / `patina rollback` unchanged. Covered by T-027.
</coverage>

## Known follow-ups

One non-blocking observation from the vet drift reviewer is deferred to SPEC-0003: `recover_orphans` (`engine.rs:337`) runs before the lock is resolved at `engine.rs:342`, so a contended `NonBlocking` apply that also finds an orphan would mutate the filesystem before returning the contention error, in tension with REQ-030's "writes nothing" prose. This is pre-existing (the recover-before-lock ordering predates the amendment), has no active consumer until the SPEC-0003 watcher wires `NonBlocking`, and the proper fix interacts with REQ-030's byte-for-byte Blocking constraint. Deferred to SPEC-0003.

## Out-of-scope items absorbed

None. The two downstream call sites (SPEC-0003 watcher `NonBlocking`; SPEC-0002 `remove`/`promote` `Held`) were explicitly kept out of T-027 scope and are owned by those SPECs.

</report>
