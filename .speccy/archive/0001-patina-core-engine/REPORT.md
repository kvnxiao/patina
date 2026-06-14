---
spec: SPEC-0001
outcome: implemented
generated_at: 2026-05-30T00:00:00Z
---

# REPORT: SPEC-0001 Patina core engine — transactional apply with apply/status/rollback CLI

<report spec="SPEC-0001">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002">
T-001 scaffolded the Cargo workspace with `patina-core` (library, `thiserror`,
no `anyhow`) and `patina-cli` (binary, `anyhow`, `patina-core` path dep), both
declaring `edition = "2024"`, MSRV `1.85`, and `license = "MIT"`.
`cargo metadata --format-version 1 --no-deps` lists both members with the
configured edition and licence; `cargo build --workspace --locked` exits 0.
Retry count: 2.
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-003 CHK-004">
T-001 introduced the three public async entry points (`apply`, `status`,
`rollback`) in `patina-core/src/lib.rs` returning typed `Result` with a
`thiserror`-derived `EngineError`, wired `tokio` with the full SPEC-mandated
feature set, and annotated `patina-cli/src/main.rs` with `#[tokio::main]`.
Retry count: 2.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006 CHK-007">
T-003 implemented repository discovery: `PATINA_REPO` env var, then walk-up
from CWD seeking a `patina.toml` with `root = true`, then persisted default in
the state directory. No `--repo` flag is present in the `clap`-derived parser.
Missing-all-sources exits with code 1 and names all three discovery paths in
stderr. Retry count: 3.
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-008 CHK-009">
T-003 enforced the two-level module depth limit during discovery: a
`patina.toml` more than one subdirectory below the root is rejected with a
typed error naming the offending path and "maximum module depth"; a non-root
file declaring `root = true` is similarly rejected. Retry count: 3.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-010 CHK-011 CHK-012 CHK-041 CHK-042 CHK-043 CHK-044 CHK-045 CHK-046 CHK-047">
T-004 implemented the five file modes (`symlink`, `symlink-dir`, `copy`,
`copy-tree`, implicit template render for `.tmpl` sources) and the
`target`/`targets` XOR parse rule. T-014 extended mode coverage with
omitted-mode defaulting to `symlink` and multi-target fan-out: the engine
records one journal operation per `(source, target_i)` pair. Parse errors for
`targets = []`, both keys declared, or neither key declared produce typed
errors with the required substrings. Retry count: 1 (T-004), 1 (T-014).
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-013 CHK-014">
T-004 implemented the `[[hook]]` schema: `event` accepts only `pre_apply` and
`post_apply`; `on_change` and any other value are parse errors naming the
offending value and the accepted set; `must_succeed` defaults to `true`; `when`
is a MiniJinja predicate. T-015 extended hook coverage with `shell` fallback and
`patina.env.*`-based `when` evaluation. Retry count: 1 (T-004), 1 (T-015).
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-015 CHK-016 CHK-040">
T-006 implemented the six-layer variable precedence chain (CLI override,
per-machine, per-profile, per-module, repo-shared, built-in `patina.*`). Any
attempt to set a key under the `patina.*` namespace from user config or CLI
override is rejected with a typed error naming the offending key and the
substring `reserved`. `patina.env.*` resolves from the live process environment.
Retry count: 2.
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-017 CHK-018">
T-007 implemented four-source profile resolution: `PATINA_PROFILE` env var,
persisted state-directory choice, `[[auto_match]]` hostname rule, and no-profile
fallback. No `--profile` CLI flag exists. The resolved profile name appears in
the `--json` output's `profile` field. Retry count: 3.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-019 CHK-020">
T-008 configured a single MiniJinja environment with
`UndefinedBehavior::SemiStrict` shared across `*.tmpl` rendering and `when`
expression evaluation. An undefined variable in a template or `when` clause
produces a typed engine error naming the variable; the Jinja2 `{% else %}`
carve-out renders undefined values as empty string in else-blocks without error.
Retry count: 2.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-021">
T-009 canonicalized all paths to absolute form at intake: existing paths through
the filesystem (resolving symlinks and `.`/`..`); not-yet-existing paths
lexically by joining with the absolute parent or CWD. `~` expands to the
invoking user's home directory. Journal records carry only canonical absolute
paths. Retry count: 0.
</coverage>

<coverage req="REQ-011" result="satisfied" scenarios="CHK-022">
T-010 implemented the single-fsync upfront postcard journal: the full plan is
written to `<state>/patina/journal/<ts>.plan` before any mutation, with a `u16`
major version field at offset 0, the plan file fsync'd and then its parent
directory fsync'd. The plan is deleted only after the `<ts>.COMMIT` sentinel is
written and fsync'd. The `<ts>.COMMIT` sentinel shares the same version envelope
(major bumped to `2` by T-026/REQ-029). Retry count: 1.
</coverage>

<coverage req="REQ-012" result="satisfied" scenarios="CHK-023">
T-010 appended a progress record to `<ts>.progress` after each completed
operation without explicit `fsync`; recovery probes the filesystem rather than
relying on progress-cursor accuracy. Retry count: 1.
</coverage>

<coverage req="REQ-013" result="satisfied" scenarios="CHK-024">
T-011 implemented probe-based crash recovery: on finding a plan without a
`COMMIT`, the engine probes each operation's completion state and reverses
completed operations using journaled inverse operations and the backup directory.
T-028 reordered recovery to run strictly after lock acquisition per REQ-030:
lock is acquired first; orphan scan and every reversal execute under the held
lock, so a concurrent apply cannot observe or reverse another run's in-flight
plan, and a contended `NonBlocking` attempt performs no recovery at all.
Retry count: 1 (T-011), 1 (T-028).
</coverage>

<coverage req="REQ-014" result="satisfied" scenarios="CHK-025">
T-012 implemented pre-overwrite backups: before clobbering any pre-existing
target the engine copies it to
`<state>/patina/backups/<ts>/<mirrored-absolute-path>`. Fresh targets produce no
backup entry. The dotfiles repository receives no writes during apply.
Retry count: 1.
</coverage>

<coverage req="REQ-015" result="satisfied" scenarios="CHK-026">
T-012 implemented count-based GC: after each successful apply (post-`COMMIT`)
the engine removes backup directories older than the tenth most recent, retaining
exactly ten. A failed apply (no `COMMIT`) does not trigger GC. No `patina gc`
command exists. Retry count: 1.
</coverage>

<coverage req="REQ-016" result="satisfied" scenarios="CHK-027">
T-005 implemented OS-appropriate state directory resolution: Linux uses
`$XDG_STATE_HOME/patina/` falling back to `$HOME/.local/state/patina/`; macOS
uses `$HOME/Library/Application Support/patina/`; Windows uses
`%LOCALAPPDATA%\patina\`. The directory contains `journal/`, `backups/`,
`profile`, `default_repo`, and `lock`. Retry count: 1.
</coverage>

<coverage req="REQ-017" result="satisfied" scenarios="CHK-028 CHK-029 CHK-030">
T-016 implemented TTY-aware apply behaviour: interactive TTY prompts
`Apply? [y/N]`; non-TTY exits 0 after printing the diff with no mutation;
`--yes` skips the prompt unconditionally; `--force-deploy` overrides every hook
in the plan to `must_succeed = false` for the invocation. Retry count: 1.
</coverage>

<coverage req="REQ-018" result="satisfied" scenarios="CHK-031 CHK-032 CHK-048">
T-017 implemented `patina status` output: each managed target is classified as
CLEAN, DRIFTED, MISSING, or ORPHANED. Content targets compare a freshly computed
`blake3` of the live file against the committed hash (REQ-029); symlink targets
compare the live link destination against the recorded source path. Multi-target
entries report each target as an independent row. Retry count: 1.
</coverage>

<coverage req="REQ-019" result="satisfied" scenarios="CHK-033 CHK-049">
T-018 implemented `patina rollback`: the engine reads the most-recent
`<ts>.COMMIT` record, reverses each materialized target using the backup
directory and inverse operations, and writes a `<ts>.ROLLED_BACK` sentinel.
Rollback is idempotent. Multi-target entries are rolled back atomically per
`[[file]]` entry. Retry count: 2.
</coverage>

<coverage req="REQ-020" result="satisfied" scenarios="CHK-034 CHK-050">
T-019 implemented `patina debug journal <path>` as a `clap` subcommand group,
decoding the binary plan or commit record into human-readable form showing
operation mode, source, target, and timestamp for each recorded op. Retry count: 2.
</coverage>

<coverage req="REQ-021" result="satisfied" scenarios="CHK-035">
T-021 verified deterministic stdout: two consecutive `patina apply` runs against
an unchanged source produce byte-identical stdout (no wall-clock timestamps,
PIDs, or random IDs in either human-readable or `--json` output). Retry count: 2.
</coverage>

<coverage req="REQ-022" result="satisfied" scenarios="CHK-036">
T-020 implemented the exit-code contract: 0 (success/no-op), 1 (config error),
2 (user declined), 3 (drift detected), 4 (lock timeout), 5 (post-apply hook
failure triggering rollback). Retry count: 1.
</coverage>

<coverage req="REQ-023" result="satisfied" scenarios="CHK-037">
T-013 implemented the exclusive advisory lock at `<state>/patina/lock` via
`fs2`, with a sixty-second blocking poll cap; timeout maps to exit code 4.
The lock is held for the entire apply/rollback execution and released on drop.
Retry count: 1.
</coverage>

<coverage req="REQ-024" result="satisfied" scenarios="CHK-038 CHK-039">
T-002 added `clippy.toml` rules denying `unwrap()`, `expect()` (in production;
`allow-expect-in-tests = true`), `panic!`, `unreachable!`, `todo!`, and
`unimplemented!` outside `#[cfg(test)]`. `cargo clippy --workspace --all-targets
--locked -- -D warnings` exits 0 at HEAD. Retry count: 0.
</coverage>

<coverage req="REQ-025" result="satisfied" scenarios="CHK-051 CHK-052">
T-022 wired a three-platform CI matrix (macOS, Linux, Windows) running
`cargo test --workspace --locked`, `cargo clippy`, and `cargo deny check` as
required status checks. Merge to `main` is blocked when any platform fails.
Retry count: 0.
</coverage>

<coverage req="REQ-026" result="satisfied" scenarios="CHK-053 CHK-054 CHK-055">
T-023 introduced the `output::Reporter` trait with a `StreamReporter` production
implementation (human vs JSON split by which method the command layer calls) and
a test-only `BufferReporter`. `clippy.toml` lists `std::println`, `std::eprintln`,
`std::print`, and `std::eprint` under `disallowed-macros`; `tracing` macros
remain permitted. Retry count: 0.
</coverage>

<coverage req="REQ-027" result="satisfied" scenarios="CHK-056 CHK-057 CHK-058">
T-024 created `docs/ARCHITECTURE.md` (headings: `Engine layers`, `Journal
format`, `Apply phases`, `Recovery`) and `docs/USER_GUIDE.md` (headings:
`Installation`, `Declaring dotfiles`, `Apply flow`, `State directory`,
`Recovery`, `Troubleshooting`). The `State directory` section contains a
markdown bullet list naming `iCloud Drive`, `OneDrive`, `Dropbox`, `Box`,
`Google Drive`, `Syncthing`. An integration test asserts heading presence by
exact text and cloud-sync bullet membership by literal entry. Retry count: 1.
</coverage>

<coverage req="REQ-028" result="satisfied" scenarios="CHK-059 CHK-060 CHK-061">
T-025 added `deny.toml` at the repo root with `[licenses]`, `[advisories]`,
`[bans]`, and `[sources]` tables; GPL-family licences are not in the allowlist.
A CI workflow runs `cargo deny check` on every push and pull request as a
required status check pinned to the latest major action version. Retry count: 0.
</coverage>

<coverage req="REQ-029" result="satisfied" scenarios="CHK-062 CHK-063 CHK-064">
T-026 widened the `ApplyRecord` embedded in the `<ts>.COMMIT` sentinel to retain,
per target, the canonical source path and — for content-mode targets — a 32-byte
`blake3` content hash. The shared version-envelope major was bumped from `1` to
`2`. A binary reading a major it does not support refuses the record with a typed
version-mismatch error. `patina status` compares freshly computed `blake3` hashes
against recorded values to detect drift. Retry count: 0.
</coverage>

<coverage req="REQ-030" result="satisfied" scenarios="CHK-065 CHK-066 CHK-067 CHK-068">
T-027 added a `LockPolicy` enum to the apply entry points: `Blocking` (default;
sixty-second cap, exit code 4 on timeout — observably identical to the
pre-amendment behaviour for a single uncontended process); `NonBlocking` (single
attempt; typed contention error, writes nothing to the filesystem even when an
orphan plan is pending); `Held` (caller supplies the acquired `LockGuard`;
engine does not re-acquire). `patina apply` and `patina rollback` continue to
use `Blocking`. T-028 moved orphan recovery to run after lock resolution for all
three policies (see REQ-013). Retry count: 1 (T-027), 1 (T-028).
</coverage>

</report>

## Notes

This REPORT covers the amendment that added REQ-030 (lock-acquisition policy)
and the post-vet hardening that amended REQ-013 (recovery-under-lock ordering),
REQ-009 (SemiStrict vs Strict MiniJinja variant prose), and REQ-026
(StreamReporter single-impl shape prose). The two documentation-only amendments
required no implementation tasks; the code was already conformant. REQ-030 and
the REQ-013 ordering fix are the substantive work, implemented in T-027 and T-028
respectively. The SPEC-0003 open question (e) on orphan-recovery vs NonBlocking
lock ordering is resolved by T-028; no further downstream amendment is needed.
