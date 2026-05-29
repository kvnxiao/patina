---
spec: SPEC-0001
outcome: implemented
generated_at: 2026-05-29T00:00:00Z
---

# REPORT: SPEC-0001 Patina core engine — transactional apply with apply/status/rollback CLI

<report spec="SPEC-0001">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002">
T-001 wired up the Cargo workspace with patina-core (library,
thiserror, edition = "2024", rust-version = "1.95", license = "MIT",
no anyhow) and patina-cli (binary, anyhow plus patina-core as direct
deps). cargo metadata confirms both package names, editions, and
licences. cargo build --workspace --locked exits 0 on the MSRV
toolchain. Retry count: 1 (first review pass caught a missing
rust-version propagation; fixed in the same task).
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-003 CHK-004">
T-001 added tokio to patina-core with the full feature set
(rt-multi-thread, fs, process, signal, sync, time, io-util, macros)
and exposed pub async fn apply, status, and rollback from
patina-core/src/lib.rs. patina-cli/src/main.rs carries the tokio::main
attribute and awaits the library entry points. No sync facade exists in
the library. Retry count: 0.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006 CHK-007">
T-002 implemented three-source repository discovery: PATINA_REPO env
var (highest priority), walk-up from CWD for a patina.toml with
root = true, then a persisted default in the state directory. No
--repo flag exists on any subcommand. Error output on all-sources-
missing names all three sources tried. Retry count: 0.
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-008 CHK-009">
T-003 implemented flat two-level module discovery. A nested patina.toml
at depth greater than one is rejected with a typed error naming the
path and the phrase "maximum module depth". A non-root file declaring
root = true is rejected. A root file missing root = true is rejected.
All three rejection paths are covered by integration tests. Retry count: 0.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-010 CHK-011 CHK-012 CHK-041 CHK-042 CHK-043 CHK-044 CHK-045 CHK-046 CHK-047">
T-004 (five file modes + single-target), T-008 (multi-target fan-out),
and T-021 (template render determinism) jointly satisfy REQ-005. All
five modes (symlink, symlink-dir, copy, copy-tree, implicit template
render via .tmpl suffix) are implemented and tested. target/targets XOR
parsing is enforced; both-present, neither-present, and targets = []
are typed parse errors. Multi-target fan-out records one journal
operation per (source, target_i) pair. Default mode is symlink. A .tmpl
source path declared explicitly in a mode entry is rejected. Retry
count: T-004: 1; T-008: 0; T-021: 0.
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-013 CHK-014">
T-005 implemented the [[hook]] schema. Only pre_apply and post_apply
are accepted event values; on_change and any other value produce a
typed parse error naming the offending value and the accepted set.
must_succeed defaults to true. The when field is evaluated against the
same MiniJinja context as .tmpl rendering. Retry count: 0.
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-015 CHK-016 CHK-040">
T-006 implemented the six-layer variable precedence chain (CLI
overrides, per-machine, per-profile, per-module, repo-shared,
built-ins) with the reserved patina.* namespace. The built-ins
patina.os, patina.arch, patina.hostname, patina.user, patina.home,
patina.profile, and the dynamic patina.env.* map are all implemented.
User attempts to set patina.* keys at any layer are rejected with a
typed error. Retry count: 0.
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-017 CHK-018">
T-007 implemented four-source profile resolution: PATINA_PROFILE env
var, persisted profile in state directory, [[auto_match]] rules in root
patina.toml, and a no-profile fallback. No --profile CLI flag exists.
JSON output carries a profile field. Retry count: 0.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-019 CHK-020">
T-009 configured MiniJinja with UndefinedBehavior::Strict as the single
engine shared between .tmpl rendering and when expression evaluation.
Undefined variable references in either context produce a typed error
naming the variable. The Jinja2 inherited else-block empty-string
behavior is preserved. Retry count: 0.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-021">
T-010 implemented path canonicalization throughout the engine. Paths
that exist are canonicalized through the filesystem; paths that do not
yet exist are resolved lexically against the canonical parent. Journal
records store only canonical absolute paths. Retry count: 1 (first
review pass flagged a missing lexical fallback for not-yet-existing
parent directories; fixed and tests expanded).
</coverage>

<coverage req="REQ-011" result="satisfied" scenarios="CHK-022">
T-011 implemented the single-fsync upfront postcard journal. The plan
is serialized with postcard, wrapped in a version envelope (u16 major
at offset 0), written to the state journal path, fsync'd, then the
parent directory is fsync'd before any mutation begins. The plan file
persists until a COMMIT sentinel is written and fsync'd. Retry count: 0.
</coverage>

<coverage req="REQ-012" result="satisfied" scenarios="CHK-023">
T-012 implemented per-operation progress cursor writes to the progress
file. Records are written unbuffered with no per-operation fsync; the
engine documents that filesystem probing during recovery is the
source-of-truth, not the progress file. Retry count: 0.
</coverage>

<coverage req="REQ-013" result="satisfied" scenarios="CHK-024">
T-013 implemented crash recovery. On startup the engine checks for a
plan file without a COMMIT sentinel and enters recovery mode: it probes
the filesystem per operation to determine actual completion state, then
reverses completed operations using journaled inverse operations and the
backup directory. Recovery is idempotent and always restores pre-apply
state (never forward-completes). Retry count: 0.
</coverage>

<coverage req="REQ-014" result="satisfied" scenarios="CHK-025">
T-014 implemented pre-overwrite backups. Before overwriting any
pre-existing target, the engine copies the original to
state/patina/backups/ts/mirrored-target-path. Files created fresh
produce no backup entry. The dotfiles repository is never written to
during apply. Retry count: 0.
</coverage>

<coverage req="REQ-015" result="satisfied" scenarios="CHK-026">
T-015 implemented count-based backup retention. After each successful
apply (post-COMMIT), the engine garbage-collects backup directories
older than the tenth most recent, retaining the ten newest by timestamp
sort. A failed apply does not trigger GC. No patina gc subcommand
exists. Retry count: 1 (first review pass caught off-by-one in
retention count when fewer than 10 prior applies existed).
</coverage>

<coverage req="REQ-016" result="satisfied" scenarios="CHK-027">
T-016 implemented OS-appropriate state directory resolution: Linux uses
XDG_STATE_HOME/patina falling back to HOME/.local/state/patina; macOS
uses HOME/Library/Application Support/patina; Windows uses
LOCALAPPDATA\patina. The state directory holds journal/, backups/,
profile, default_repo, and lock. Retry count: 1 (first review pass
identified a missing XDG fallback test on Linux; added and verified).
</coverage>

<coverage req="REQ-017" result="satisfied" scenarios="CHK-028 CHK-029 CHK-030">
T-017 implemented TTY-driven apply semantics. In a TTY: print diff,
prompt Apply? [y/N] on stderr, apply only on y/Y. In a non-TTY: print
diff and exit 0 without mutation. --yes skips the prompt regardless of
TTY. --force-deploy overrides every hook to must_succeed = false for
the invocation. --json alone previews without mutation (result:
previewed); --json --yes applies (result: applied or rolled_back).
--pager=delta/difft falls back to the embedded similar renderer with a
stderr warning when the named tool is not on PATH. Retry count: 1
(first review pass flagged missing non-TTY detection on Windows;
verified with is-terminal crate).
</coverage>

<coverage req="REQ-018" result="satisfied" scenarios="CHK-031 CHK-032 CHK-048">
T-018 implemented patina status with CLEAN/DRIFTED/MISSING/ORPHANED
classification. The command reads the last COMMIT-sentineled journal and
compares recorded expected hashes to current filesystem state. --json
output carries last_apply (with at, user, host), a files array (one
element per managed target), and aggregate counters. Multi-target
entries produce one row per target with independent counters. Retry
count: 2 (first review pass caught missing ORPHANED detection; second
pass caught multi-target counter independence; both fixed).
</coverage>

<coverage req="REQ-019" result="satisfied" scenarios="CHK-033 CHK-049">
T-018 implemented patina rollback. The command reverses the most recent
COMMITed apply by replaying inverse operations using the journal and
backup directory. Files created fresh are deleted; files with backups
are restored. The rolled-back journal is marked with a ROLLED_BACK
sentinel. Multi-target entry targets are restored as an atomic unit.
Invocation with no prior successful apply exits 1. Retry count:
included in T-018 count above.
</coverage>

<coverage req="REQ-020" result="satisfied" scenarios="CHK-034 CHK-050">
T-019 implemented patina debug journal. The debug subcommand group
exists as a clap extension point; patina debug --help lists journal as
a subcommand. The command validates the version envelope, decodes the
recorded file operations, and prints one operation per line identifying
each op mode, source, and target. An invalid path or incompatible
version envelope exits 1. Retry count: 1 (first review pass caught SPEC
drift where the plan model was narrowed to file-operations-only; fixed
after SPEC amendment).
</coverage>

<coverage req="REQ-021" result="satisfied" scenarios="CHK-035">
T-021 enforced deterministic stdout. Two consecutive patina apply --yes
--json invocations against an unchanged source repository produce
byte-identical stdout. No wall-clock values appear in user-facing output
paths. A template-render non-determinism surfaced during the vet pass
and was fixed before ship. Retry count: 0.
</coverage>

<coverage req="REQ-022" result="satisfied" scenarios="CHK-036">
T-020 formalized CLI exit codes: 0 success, 1 generic error, 2
pre_apply must_succeed hook failure (no file ops executed), 3 post_apply
must_succeed hook failure (file ops rolled back), 4 lock timeout, 5
user declined. All six codes are covered by integration tests.
Retry count: 0.
</coverage>

<coverage req="REQ-023" result="satisfied" scenarios="CHK-037">
T-022 implemented the advisory file lock at state/patina/lock via the
fs2 crate. Mutating commands acquire an exclusive lock; status acquires
a shared lock with a 5-second timeout after which it warns and proceeds.
apply/rollback block up to 60 seconds then exit 4. OS-level lock
release on crash is relied on for cleanup. Retry count: 0.
</coverage>

<coverage req="REQ-024" result="satisfied" scenarios="CHK-038 CHK-039">
T-023 configured Clippy to deny unwrap_used, expect_used, panic,
unreachable, todo, and unimplemented for non-test code. clippy.toml
declares allow-expect-in-tests = true. cargo clippy --workspace
--all-targets --locked -- -D warnings exits 0 at HEAD. Retry count: 0.
</coverage>

<coverage req="REQ-025" result="satisfied" scenarios="CHK-051 CHK-052">
T-024 delivered the CI matrix workflow running on macos-latest,
ubuntu-latest, and windows-latest with both cargo test --workspace
--locked and cargo clippy --workspace --all-targets --locked -- -D
warnings on every push to main and pull_request. Third-party actions
are pinned to their latest published major version. Retry count: 0.
</coverage>

<coverage req="REQ-026" result="satisfied" scenarios="CHK-053 CHK-054 CHK-055">
T-025 implemented the output::Reporter trait with HumanReporter and
JsonReporter implementations. All user-facing output routes through a
Reporter method; clippy.toml lists std::println, std::eprintln,
std::print, and std::eprint under disallowed-macros. The output module
is the sole permitted call site. tracing macros remain permitted
everywhere. Clippy exits 0 at HEAD. Retry count: 0.
</coverage>

<coverage req="REQ-027" result="satisfied" scenarios="CHK-056 CHK-057 CHK-058">
docs/ARCHITECTURE.md ships with ##-level headings Engine layers,
Journal format, Apply phases, and Recovery. docs/USER_GUIDE.md ships
with ##-level headings Installation, Declaring dotfiles, Apply flow,
State directory, Recovery, and Troubleshooting. The State directory
section carries a bullet list naming iCloud Drive, OneDrive, Dropbox,
Box, Google Drive, and Syncthing. Integration tests assert heading
existence and bullet membership by exact text. Retry count: 0.
</coverage>

<coverage req="REQ-028" result="satisfied" scenarios="CHK-059 CHK-060 CHK-061">
deny.toml at the repository root carries licenses, advisories, bans,
and sources tables. The licenses allowlist includes MIT and Apache-2.0.
cargo deny check runs as a required CI job on every push and
pull_request and exits 0 at HEAD. Retry count: 0.
</coverage>

</report>

## Notes

The most significant mid-loop SPEC amendment was the REQ-020 /
REQ-011 reconciliation (2026-05-28 Changelog row): the serialized
Plan/PlannedOperation model records only file operations, not hooks or
the resolved variable context. T-019 (debug journal) was in review when
this was discovered; the amendment resolved the SPEC drift and T-019
proceeded to completion in the same pass.

REQ-018 (status) and REQ-019 (rollback) were co-implemented in T-018
because the rollback machinery reuses the same journal-reading
primitives as status classification. T-018 required two review rounds.

The vet pass surfaced a holistic template-render non-determinism in the
apply and template modules that per-task reviews had missed. Fixed in
the vet loop before ship; the fix is included in the uncommitted changes
staged with this REPORT.
