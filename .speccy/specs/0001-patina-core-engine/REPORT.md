---
spec: SPEC-0001
outcome: implemented
generated_at: 2026-05-29T00:00:00Z
---
# REPORT: SPEC-0001 Patina core engine

<report spec="SPEC-0001">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002">
Cargo workspace with patina-core (thiserror) and patina-cli (anyhow) declared as members; both carry edition=2024, rust-version=1.95, license=MIT; cargo build --workspace --locked exits 0.
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-003 CHK-004">
patina-core exposes pub async fn apply/status/rollback returning typed Results; patina-cli main is annotated #[tokio::main] and awaits the library entry points.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006 CHK-007">
Repository discovery: PATINA_REPO env var, walk-up from CWD for a root patina.toml, persisted default in state directory; no --repo flag.
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-008 CHK-009">
Modules discovered only at depth-1 below root; nested patina.toml and non-root root=true rejected with typed errors naming the offending path and depth constraint.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-010 CHK-011 CHK-012 CHK-041 CHK-042 CHK-043 CHK-044 CHK-045 CHK-046 CHK-047">
All five modes (symlink, symlink-dir, copy, copy-tree, template render) implemented; default symlink; multi-target targets array supported; parse errors for both/neither/empty-targets; .tmpl suffix stripped; unknown modes rejected with accepted-set in the error.
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-013 CHK-014">
[[hook]] schema accepts pre_apply and post_apply events; on_change and any other value rejected at parse; must_succeed defaults to true; when and shell fields functional.
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-015 CHK-016 CHK-040">
Variable precedence chain (CLI > machine > profile > module > repo > builtins) implemented; patina.* namespace reserved; patina.env.* dynamic map exposes process env; user attempts to set reserved keys rejected with typed error.
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-017 CHK-018">
Profile resolution chain (PATINA_PROFILE env var > persisted > auto_match > no-profile fallback) implemented; no --profile flag on any subcommand.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-019 CHK-020">
MiniJinja environment configured with UndefinedBehavior::Strict shared across .tmpl rendering and when expression evaluation; undefined variable references produce typed errors naming the variable; else fallback renders empty string per Jinja2 semantics.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-021">
Every path canonicalized to absolute form; existing paths resolved through the filesystem, not-yet-existing paths resolved lexically; tilde expanded; only canonical absolute paths appear in journal records.
</coverage>

<coverage req="REQ-011" result="satisfied" scenarios="CHK-022">
Single-fsync upfront postcard journal writes plan to state/patina/journal/ts.plan before any mutation; u16 major version envelope at offset 0; plan and parent dir fsynced; ts.COMMIT shares the same envelope format; plan deleted after successful COMMIT.
</coverage>

<coverage req="REQ-012" result="satisfied" scenarios="CHK-023">
Per-operation progress cursor appended to ts.progress without fsync; recovery probes filesystem rather than trusting cursor as ground truth.
</coverage>

<coverage req="REQ-013" result="satisfied" scenarios="CHK-024">
Recovery mode entered when plan exists without COMMIT; probes filesystem per operation; reverses completed ops via journal inverse ops and backups to restore pre-apply state; idempotent; never completes a partial apply forward.
</coverage>

<coverage req="REQ-014" result="satisfied" scenarios="CHK-025">
Pre-existing files backed up to state/patina/backups/ts/mirrored-path before overwrite; fresh targets produce no backup entry; dotfiles repository never written by the engine.
</coverage>

<coverage req="REQ-015" result="satisfied" scenarios="CHK-026">
Backup GC runs after successful COMMIT; retains ten most recent apply cycles by timestamp; older cycles removed; failed applies do not trigger GC; no patina gc subcommand.
</coverage>

<coverage req="REQ-016" result="satisfied" scenarios="CHK-027">
State directory resolved per OS: Linux XDG_STATE_HOME/patina/ fallback HOME/.local/state/patina/; macOS HOME/Library/Application Support/patina/; Windows LOCALAPPDATA\patina\; contains journal/, backups/, profile, default_repo, lock.
</coverage>

<coverage req="REQ-017" result="satisfied" scenarios="CHK-028 CHK-029 CHK-030">
TTY detection via is-terminal; bare apply in TTY prompts after diff; bare apply in non-TTY prints diff and exits 0 without mutation; --yes skips prompt regardless of TTY; --force-deploy overrides all hooks to must_succeed=false; --json alone previews; --json --yes applies; --pager falls back to similar if tool absent.
</coverage>

<coverage req="REQ-018" result="satisfied" scenarios="CHK-031 CHK-032 CHK-048">
patina status classifies every managed target CLEAN/DRIFTED/MISSING/ORPHANED by comparing live filesystem against blake3 content hash (content-mode) or recorded link target (symlink-mode) from COMMIT record; --json emits last_apply, files, aggregate counters; multi-target entries reported one row per target.
</coverage>

<coverage req="REQ-019" result="satisfied" scenarios="CHK-033 CHK-049">
patina rollback --yes reverses most recent COMMITted apply; fresh targets deleted, backed-up targets restored; ts.ROLLED_BACK sentinel written and fsynced; no prior apply emits typed error and exits 1; multi-target entries rolled back atomically as a unit.
</coverage>

<coverage req="REQ-020" result="satisfied" scenarios="CHK-034 CHK-050">
patina debug clap subcommand group with journal path child; decodes postcard plan file, validates version envelope, prints one operation per block with mode/source/target; missing path exits 1; incompatible envelope major exits 1 naming both versions.
</coverage>

<coverage req="REQ-021" result="satisfied" scenarios="CHK-035">
No wall-clock timestamps in human-readable or JSON stdout; two consecutive applies on unchanged input produce byte-identical stdout; now-style calls absent from user-facing output paths.
</coverage>

<coverage req="REQ-022" result="satisfied" scenarios="CHK-036">
Exit codes formalized: 0 success, 1 generic, 2 pre_apply must_succeed failure, 3 post_apply must_succeed rollback, 4 lock timeout, 5 user declined.
</coverage>

<coverage req="REQ-023" result="satisfied" scenarios="CHK-037">
Advisory file lock at state/patina/lock via fs2; mutating commands acquire exclusive lock (60s timeout, exit 4); read-only commands acquire shared lock (5s timeout, warn and proceed); lock released automatically on crash.
</coverage>

<coverage req="REQ-024" result="satisfied" scenarios="CHK-038 CHK-039">
No unwrap, expect, panic!, unreachable!, todo!, or unimplemented! in production code; clippy.toml denies these patterns for non-test code; allow-expect-in-tests=true permits .expect() in tests; cargo clippy --workspace --all-targets --locked -- -D warnings exits 0.
</coverage>

<coverage req="REQ-025" result="satisfied" scenarios="CHK-051 CHK-052">
CI workflow runs test and clippy matrix on macos-latest, ubuntu-latest, windows-latest on every push to main and every pull_request; all three jobs in required-status-checks; third-party actions pinned to latest published major version.
</coverage>

<coverage req="REQ-026" result="satisfied" scenarios="CHK-053 CHK-054 CHK-055">
output::Reporter trait with HumanReporter and JsonReporter implementations; clippy.toml disallowed-macros denies std::println, std::eprintln, std::print, std::eprint outside the output module; tracing macros permitted everywhere; both impls satisfy deterministic-stdout property.
</coverage>

<coverage req="REQ-027" result="satisfied" scenarios="CHK-056 CHK-057 CHK-058">
docs/ARCHITECTURE.md carries Engine layers, Journal format, Apply phases, Recovery headings; docs/USER_GUIDE.md carries Installation, Declaring dotfiles, Apply flow, State directory, Recovery, Troubleshooting; cloud-sync providers bullet list (iCloud Drive, OneDrive, Dropbox, Box, Google Drive, Syncthing) under State directory; integration tests gate structural presence.
</coverage>

<coverage req="REQ-028" result="satisfied" scenarios="CHK-059 CHK-060 CHK-061">
deny.toml at repository root with [licenses], [advisories], [bans], [sources] tables; cargo deny check runs as required-status-check on push and pull_request; GPL-family licenses excluded from allowlist.
</coverage>

<coverage req="REQ-029" result="satisfied" scenarios="CHK-062 CHK-063 CHK-064">
ApplyRecord in ts.COMMIT retains per-target canonical source path and 32-byte blake3 content hash (content-mode) or link target (symlink-mode); [[file]]-entry index for atomic rollback grouping; version envelope major bumped to 2; incompatible majors refused with typed error naming both versions; patina status CLEAN/DRIFTED classification uses recorded blake3 hash.
</coverage>

## Retry counts

All 29 requirements implemented across 26 tasks. Tasks T-001 through T-025 each completed in one review pass. T-019 required one reviewer-flip to pending (style blocker on plan narrowing) and a follow-up SPEC amendment (REQ-020/REQ-011 prose alignment) before passing. T-026 (REQ-029 widening: blake3 + per-target source + version bump) implemented and reviewed in a single pass.

## Out-of-scope items absorbed

The following items were addressed during implementation but lie outside the SPEC requirements at their original draft. They are recorded here for future SPECs to carry forward or explicitly close:

- The [[file]] default-mode (symlink) and multi-target targets array were added via SPEC amendment (2026-05-26) before any task reached completed; all tasks reflect the amended SPEC.
- REQ-025 through REQ-028 were added via a second amendment (2026-05-27) before implementation; all four were implemented and reviewed as ordinary tasks.
- REQ-029 (blake3 + per-target source in COMMIT record, version-envelope bump to 2) was added via a third amendment (2026-05-29) after the initial task set completed, prompted by SPEC-0002/0003 cross-SPEC gap analysis; T-026 delivered the change atomically.

</report>