# Patina — Agent Guide

Patina is a cross-platform dotfile manager written in Rust. This file orients LLM agents to the codebase. **Read this first before doing any work.**

---

## Product north star

Patina is a cross-platform dotfile manager whose source of truth is a user's centralized git repository. A user runs `patina apply` and the configurations declared in `patina.toml` files materialize at the right targets — as symbolic links pointing back into the repo, rendered template output, or byte copies where a link is not appropriate. The engine guarantees that a mid-apply crash leaves the filesystem in either the pre-apply or post-apply state, never an intermediate one.

### Users

- **Developer setting up a fresh laptop** — clones their dotfiles repo, runs one command, expects their shell/editor/git config to land at the right paths so the new machine matches their other machines immediately.
- **Cautious user** — wants the default `patina apply` to show a diff and prompt before any mutation; never wants to accidentally overwrite a file they edited outside of Patina.
- **CI script author** — wants `patina apply` in a non-interactive shell to display the plan and exit without mutating, so pipelines can preview a deployment safely.
- **Contributor debugging a failed apply** — wants `patina debug journal` to decode a binary journal into a human-readable form to see exactly which operations the engine planned and which it executed before failure.

### V1.0 outcome

V1.0 ships across three SPECs:

- **SPEC-0001 (in progress):** core engine + `apply` / `status` / `rollback` CLI surface — enough to exercise the engine end-to-end in integration tests.
- **SPEC-0002:** complete user-facing CLI (`init`, `add`, `remove`, `promote`, `doctor`) plus the Windows symlink Developer-Mode / UAC elevation flow.
- **SPEC-0003:** watch subsystem — filesystem event loop, per-OS service install, drift detection.

Done-enough-to-ship is gated by the `<done-when>` and `<scenario>` blocks inside each SPEC; this section is the elevator pitch, not the acceptance criteria.

### Quality bar

- **Crash safety is not optional.** The single-fsync postcard journal + per-operation progress cursor exists so a `kill -9` mid-apply converges deterministically on the next run. Any change that weakens this invariant is a v1.0 blocker.
- **No panics in production.** `unwrap` / `expect` / `panic!` / `unreachable!` / `todo!` / `unimplemented!` are Clippy-denied outside `#[cfg(test)]`. See `.claude/rules/rust/rust-error-handling.md`.
- **Deterministic stdout.** Two consecutive `patina apply` invocations against an unchanged source repo produce byte-identical stdout. No wall-clock timestamps, PIDs, or random IDs in user-facing output (`--json` included).
- **Cross-platform parity.** macOS, Linux, and Windows are first-class. A feature that works on two of three platforms is not done.
- **Tests are the validation criterion**, not "looks right." Integration tests use `tempfile::TempDir` fixtures; snapshots use `insta`; properties use `proptest`. See `## Code conventions` below for crate choices.

### Non-goals

Each SPEC enumerates its own non-goals authoritatively inside a `<non-goals>` block. At the product level, v1.0 explicitly does **not** include: merge-mode file types (`merge-json`, `merge-toml`, etc.), nested modules beyond two levels, `on_change` / `on_drift` hook events, a JSON schema-version field, a `patina gc` command, a `--repo <path>` global flag, a GUI, migrations from other dotfile managers, an embedded scripting language, native encryption, cross-machine state sync, machine inventory, or dashboards. If the user asks for one of these, the answer is "not in v1.0" — surface it as a question for a future SPEC, not silent scope creep.

### Known unknowns

Tracked authoritatively in each SPEC's `<assumptions>` block. The load-bearing ones for v1.0:

- `postcard` wire-format stability across v1.0 (mitigated by the journal version envelope so the decoder refuses incompatible records explicitly).
- `fs2`'s advisory file lock papering over POSIX `flock(2)` vs Windows `LockFileEx` semantic differences adequately for the single-CLI case and the SPEC-0003 watcher-CLI coordination case.
- `tokio`'s file I/O remaining `spawn_blocking`-backed on every platform in v1.0; engine accepts this rather than waiting for native async file I/O.
- MiniJinja's strict-undefined behavior — including the Jinja2-inherited rule that an undefined value inside `{% else %}` renders as empty string — being acceptable for v1.0.
- Users following the instruction not to place the per-machine state directory on iCloud Drive / OneDrive / Dropbox / Box / Google Drive / Syncthing. SPEC-0002 adds doctor warnings; SPEC-0001 documents only.

---

## Speccy workflow

This repository is driven by **speccy**, a spec-driven development tool. Every non-trivial change lands through a SPEC → TASKS → implement-review-vet → ship loop. **Do not implement code changes that bypass the loop** unless the user explicitly authorizes a one-off (e.g. a single-line typo fix, a CI workflow tweak, a docs nit). When in doubt, ask.

The full toolchain lives in two places:

- **`speccy` CLI** at `$PATH`: deterministic feedback engine. Run `speccy --help` for the command list. The most-used subcommands are `speccy status` (workspace overview), `speccy next --json` (resolve the next actionable task), `speccy lock` (record the SPEC.md content hash into TASKS.md so amendments are detectable), and `speccy verify` (the CI gate that proof-shape-validates the repo).
- **`speccy-*` skills** under `.claude/skills/`: one skill per phase of the loop. Always prefer the skill over hand-running steps; the skill knows the preconditions and the next-step suggestion.

### Picking the right skill

| Situation | Skill |
| --- | --- |
| Fuzzy idea, no SPEC yet, scope unclear | `/speccy-brainstorm` (Socratic atomization, hard-gate on user approval) |
| Scope is clear, ready to draft a SPEC | `/speccy-plan` |
| SPEC exists, need to decompose into tasks | `/speccy-tasks` |
| Want to implement one task | `/speccy-work` (one task per invocation; resolves next via `speccy next --json` if no selector) |
| A task is `state="in-review"` | `/speccy-review` (fans out business / tests / security / style reviewers in parallel) |
| All tasks `state="completed"`, pre-ship boundary | `/speccy-vet` (holistic SPEC-vs-implementation drift check + simplifier polish pass) |
| Ready to open the PR | `/speccy-ship` (writes `REPORT.md`, runs `speccy verify`, opens PR) |
| SPEC.md needs to change mid-loop | `/speccy-amend` (surgical edit + Changelog row + re-lock spec hash) |
| Want to drive a SPEC end-to-end | `/speccy-orchestrate` (chains work → review → vet until ready to ship; stops one step before `/speccy-ship` because PR opens are irreversible) |
| Fresh repo with no `.speccy/` yet | `/speccy-init` (one-shot bootstrap; `--force` to refresh shipped files) |

### How to find what to do next

When in doubt about what state the workspace is in: `speccy status` for the human view, `speccy next --json` for the machine-readable "what's the next actionable task" answer. Both are non-mutating; run them freely.

---

## Hard rules — never

These are hard rules, not preferences. Violating any of them is grounds for the next reviewer turn to reject the work outright. Group 1 ("Code quality") is enforced by Clippy and CI; group 2 ("Speccy loop") is enforced by `speccy verify` and the review skill.

### Code quality

- **Never `unwrap()`, ever.** Use `?` with proper error types in production; use `.expect("descriptive message")` in tests.
- **Never `expect()` in production code.** Allowed only in tests (`clippy.toml` sets `allow-expect-in-tests = true`). See `.claude/rules/rust/rust-error-handling.md`.
- **Never panic in production code.** No `panic!()`, `unreachable!()`, `todo!()`, or `unimplemented!()` outside `#[cfg(test)]` — return a typed error instead.
- **Never use `println!` / `eprintln!` for user-facing output** outside the dedicated `output::Reporter` layer. Use `tracing` macros (`info!`, `warn!`, `error!`, `debug!`, `trace!`) everywhere else.
- **Never skip writing tests for "obvious" code.** Validation requires tests, not "looks right."
- **Never introduce a dependency without checking it against `deny.toml`.** License, security advisory, and bloat checks gate the merge.
- **Never let docs drift.** If your change alters observable behavior, architecture, or testing conventions, the corresponding `docs/` files change in the same PR.

### Speccy loop

- **Never edit a locked `SPEC.md` outside of `/speccy-amend`.** Once `TASKS.md` exists and references the spec hash, `.speccy/specs/<NNNN>-<slug>/SPEC.md` is editable only through `/speccy-amend`. Direct edits silently desync the spec hash from `TASKS.md` and a subsequent `speccy verify` will fail. The only exceptions are cosmetic edits that do not change any `<requirement>`, `<done-when>`, `<scenario>`, `<goals>`, `<non-goals>`, or `<assumptions>` block — in practice, typo fixes and prose polish on the Summary section.
- **Never hand-edit `TASKS.md`.** It is generated by `/speccy-tasks`. Task state transitions (`pending → in-progress → in-review → completed`) happen via `/speccy-work` and `/speccy-review`. Hand-editing to flip a task to `completed` bypasses the review gate.
- **Never mark a task `completed` based on your own assessment.** `/speccy-review` runs four adversarial personas (business, tests, security, style) in parallel and they decide. If they pass it, the task moves to `completed`. If they block, the task flips back to `pending` with a `<blockers>` block appended to the per-task journal file. The implementer's self-assessment carries no weight.
- **Never rewrite history in per-task journals.** `.speccy/specs/<NNNN>-<slug>/journal/T-NNN.md` is append-only during the loop: the implementer writes one entry; the reviewers may append blockers. The journal is the audit trail for why a task landed the way it did.
- **Never open a PR with a failing `speccy verify`.** `/speccy-ship` enforces this locally, and it is CI's gate. Don't push a branch that fails `speccy verify` locally and hope CI sorts it out.
- **Never run `speccy archive` casually.** It relocates a shipped/dropped/superseded SPEC into `.speccy/archive/` and is essentially irreversible without a `git revert`. Only archive after the SPEC is genuinely closed out.
- **Never hand-edit `.claude/skills/speccy-*` files.** They are shipped by the speccy CLI and refreshed by `speccy init --force`. Local edits will be clobbered the next time someone upgrades the toolchain. If a skill needs to change, the change goes upstream.

---

## Code conventions

General Rust style rules live under `.claude/rules/rust/` and are **authoritative**. The conventions below are project-specific additions and patina-specific crate choices; if anything below appears to conflict with a rule, the rule wins and this file should be updated.

- **Errors:** `thiserror` for typed errors in libraries (`patina-core`); `anyhow` for application-level error chaining in `patina-cli`. For genuinely impossible preconditions in production, refactor to make them type-system enforced or use `?` with a proper error variant. (Panic-family lints in `## Hard rules` above are non-negotiable.)
- **Paths:** `camino::Utf8PathBuf` and `camino::Utf8Path` everywhere we know paths are UTF-8 (which is most places). Convert at OS-API boundaries.
- **Filesystem:** prefer `fs-err` over `std::fs` so error messages include the path.
- **Logging:** `tracing` macros (`info!`, `warn!`, `error!`, `debug!`, `trace!`) everywhere except the CLI's user-facing output layer.
- **CLI output:** human-readable by default with color where appropriate, JSON when `--json` is set. Use the `output::Reporter` abstraction (introduced in Phase 10), not direct prints.
- **Tests:** integration tests use `tempfile::TempDir` for repo fixtures. Snapshot tests use `insta`. Property-based tests use `proptest`.
- **Public API:** every public function has a doc comment with at least one example. `cargo doc --no-deps` must build clean.
- **Diagrams in docs:** when a diagram could be expressed equivalently in either ASCII art or Mermaid, use Mermaid (` ```mermaid ` fenced code blocks). Mermaid renders natively on GitHub, in most viewers, and diffs cleanly per-node/per-edge instead of redrawing the whole picture. Keep ASCII only when it conveys information Mermaid loses — directory trees with inline comments, exact-byte file layouts, terminal output examples.
