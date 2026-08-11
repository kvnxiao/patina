# PLAN: remote git sources

Implementation instructions for an AI agent. The normative behavioral
spec is `docs/REMOTE_SOURCES.md`, written ahead of the code; when this
plan and the spec disagree, the spec wins and this plan gets fixed in
the same commit. Do not restate spec content in code comments or
commit messages; link to the spec section instead.

## Before writing any code

1. Read `CLAUDE.md` (repo root). The hard rules are non-negotiable:
   no `unwrap()`/`expect()`/panics in production code, no `println!`
   outside `output::Reporter`, no new dependency without a `deny.toml`
   check, docs change in the same PR as behavior.
2. Use the `rust-rules` Skill before writing or reviewing any Rust.
3. Read `docs/REMOTE_SOURCES.md` in full. Each phase below names the
   spec sections it implements.
4. Line references below are a snapshot from 2026-08-11. Verify each
   seam before editing; symbols outrank line numbers.

## Code seams

| Seam | Location |
| ---- | -------- |
| Module manifest raw model (`RawModule`) | `patina-core/src/config/mod.rs:165-191` (no `deny_unknown_fields`; new tables parse cleanly on old binaries) |
| Entry parsing + mode allowlists (`RawEntry`, `FileMode`) | `patina-core/src/config/file_entry.rs:167-301` |
| Root manifest parsing (`parse_root_config_str`, `RawRoot`) | `patina-core/src/config/root.rs:127-169` |
| Recognized-but-rejected root key precedent (`[watcher]`) | `patina-core/src/watch/mod.rs:47-50, 101-114` |
| Module discovery (`ModuleHandle`, `discover_modules`) | `patina-core/src/discovery/modules.rs:19-24, 101-177` |
| Source resolution (`resolve_entry`: joins `module_path` + `entry.source`) | `patina-core/src/apply/engine.rs:823-830` |
| Planning context (single `repo_root`, module list) | `patina-core/src/apply/engine.rs:381-462` |
| Plan assembly (`assemble_plan_operations`, entry ordering) | `patina-core/src/apply/engine.rs:194-201, 290-367` |
| State directory (`compute_root`, `create_tree`) | `patina-core/src/state_dir.rs:152-192` |
| Journal records (prune reachability source) | `patina-core/src/journal/` |
| Engine errors (`#[non_exhaustive]`, one `#[from]` per subsystem) | `patina-core/src/error.rs:26-94` |
| Clock abstraction (use this, never `SystemTime::now()` inline) | `patina-core/src/clock.rs` |
| CLI command tree (`Command`, nested-subcommand precedent `WatchCommand`) | `patina-cli/src/cli.rs:89-142, 176-201` |
| Dispatch (one match arm per command) | `patina-cli/src/main.rs:41-76` |
| Apply orchestration (plan → diff → confirm → execute) | `patina-cli/src/cmd/apply.rs:82-134` |
| Docs structure test (additive heading gates) | `patina-cli/tests/docs_structure.rs` |

## Phases

Work the phases in order; each lands as its own narrow PR (or commit
series) that leaves `just check` green. Phase 1 is independent of the
rest and ships first.

### Phase 1: plan-time target collision validation

- Spec: `docs/REMOTE_SOURCES.md` "Target collision validation".
- Implement in the planner after entry resolution, over the active
  (post-`when`) set: exact canonical-target duplicates and
  directory-target containment are typed errors before any diff is
  shown. Remember `targets = [...]` fan-out: every element
  participates.
- New `EngineError` variant(s) naming both colliding entries by module
  and source so the user can find them.
- Tests: unit tests for the collision detector (same target, nested
  target under a directory mode, `when`-disjoint non-collision,
  multi-target fan-out collision) and `insta` snapshot tests of the
  error rendering. Follow the test-hygiene rules in `CLAUDE.md`; no
  vacuous shapes.
- Done when: a manifest with two active entries on one target fails
  planning with the typed error; the `when`-disjoint twin passes;
  `just check` green.

### Phase 2: manifest surface

- Spec: "Remote-backed modules" and "Trust boundaries".
- Add `[remote]` (url, optional ref, optional min_age) to the module
  manifest model and `[remotes]` (min_age) to the root manifest model.
  Hand-roll the duration parser (`s`/`m`/`h`/`d` suffixes); do not add
  a dependency for it.
- Enforce at parse/plan time: a remote source never gets the implicit
  `.tmpl` template mode; a checkout-internal `patina.toml` is never
  read (nothing to implement so much as to *not* implement; add a test
  proving a fixture checkout containing a hostile `patina.toml`
  contributes nothing).
- Done when: fixture manifests parse into the new model; duration
  parsing has unit tests including rejection cases; old manifests are
  byte-for-byte unaffected.

### Phase 3: git subprocess layer

- Spec: "The remote cache".
- New `patina-core` module wrapping `git` via `std::process::Command`
  (no `git2`/`gix` dependency): `ls-remote`, shallow fetch by exact
  SHA into a bare repo, checkout of a SHA into a directory, ancestry
  test (`merge-base --is-ancestor`), committer-time read
  (`show -s --format=%ct`). Typed errors; stderr captured into them;
  no output printed from this layer.
- Cache layout exactly as specced: `<state>/remotes/<module>/repo.git`
  plus `<state>/remotes/<module>/<sha>/`. Extend
  `state_dir::create_tree` for `<state>/remotes/`.
- Tests: integration tests build throwaway origin repos in a
  `tempfile::TempDir` with the real `git` binary (CI has git
  everywhere). Cover fetch-by-SHA, ancestry, committer time, and the
  offline/cold-cache error text.

### Phase 4: lockfile

- Spec: "The lockfile".
- Read/write `patina.lock` beside the root manifest: `version = 1`
  scalar plus `[remotes.<name>]` tables `{ url, ref, rev, updated_at }`.
  Serialization must be deterministic (stable table order: sort by
  name). RFC 3339 UTC timestamps via the existing clock abstraction.
- Plan-time wiring: a remote-backed module with no lock entry is a
  typed error pointing at `patina remote update <name>`.
- Tests: round-trip determinism (serialize twice, byte-identical),
  unknown-version rejection, missing-entry error snapshot.

### Phase 5: resolver + apply integration

- Spec: "Remote-backed modules", "The remote cache", "Commands"
  (consumer rows and the failure shapes list).
- Give `ModuleHandle` an origin so `resolve_entry` resolves sources
  against `<state>/remotes/<module>/<rev>/` for remote-backed modules.
  `build_planning_context` fetches any pinned rev missing from the
  cache before planning; offline + cold cache is the specced typed
  error, offline + warm cache works fully.
- Post-apply prune: delete checkouts unreferenced by any journal
  record on disk, after COMMIT, mirroring the existing backup
  retention pass.
- Tests: end-to-end apply from a fixture remote (symlink and copy
  modes), rollback re-points to the prior checkout, prune retains
  journal-referenced checkouts and removes orphans, idempotent
  re-apply stays a byte-identical no-op.

### Phase 6: `patina remote` command group + update gate

- Spec: "Commands", "The update gate", "Multi-machine flow".
- New `Command::Remote` with `list` / `check` / `update` / `prune`
  subcommands (follow the `WatchCommand` pattern) plus an `--update`
  flag on `apply`. All output through `output::Reporter`, `--json`
  variants included; exit codes through the existing funnel.
- Gate implementation order per spec: future check (hard reject),
  ancestry (confirm), backdating floor vs `updated_at` (confirm), age
  vs effective `min_age` (per-remote, else global, else 72h). First
  pin exempt from the age gate. `--now` bypasses age only, with a
  warning.
- `check` is ls-remote-only and maintains `<state>/remotes/notice` and
  `last_check`, including the dotfiles-repo-behind message; `--hook`
  adds the self-throttle (24h) and fully silent success.
- Tests: one unit test per gate check (both verdicts each), cooldown
  reporting, `--now` warning snapshot, notice-file content snapshots
  for both message shapes, `apply --update` offline degradation.

### Phase 7: surfacing + docs

- Spec: "Shell integration" plus the `doctor` and `status` mentions
  scattered through the doc.
- `patina status` reports pending remote updates from the notice
  state; `patina doctor` gains a git-on-PATH finding.
- Docs, same PR: add a `## Remote sources` section to
  `docs/USER_GUIDE.md` summarizing the feature and linking to
  `docs/REMOTE_SOURCES.md`; extend `docs/ARCHITECTURE.md` with the
  remote cache and lockfile subsystems (additive headings only; the
  structure test hard-fails on renames); add a
  `remote_sources_has_required_h2_headings` test to
  `patina-cli/tests/docs_structure.rs` gating this doc's H2 set;
  refresh the `CLAUDE.md` product north star (the v1.0 outcome list
  predates this feature and is stale).
- Delete this `PLAN.md` in the final PR of the series.

## Testing strategy

- Fixture remotes are real git repos built in `TempDir` by test
  helpers; no network in any test.
- Snapshot (`insta`) everything user-visible: diffs, gate verdicts,
  notice files, error text, `--json` envelopes.
- Determinism is load-bearing: assert byte-identical stdout for
  repeated plans and byte-identical lockfile serialization. No
  timestamps, PIDs, or ordering nondeterminism may leak into stdout.
- Windows is first-class: path handling goes through `camino` +
  `dunce`-aware `paths::canonicalize` like the rest of the engine, and
  the per-OS CI matrix must pass, not just local.

## Hygiene

- `just check` before every PR; watch the per-OS CI matrix, macOS
  clippy, MSRV (1.95), and coverage jobs after pushing.
- Narrow commits, one logical change each, with the AI trailer
  required by `CLAUDE.md` commit hygiene.
- New dependencies: none are anticipated. If one becomes necessary,
  check it against `deny.toml` first and justify it in the PR body.
