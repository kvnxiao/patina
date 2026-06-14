# Patina — Agent Guide

Patina is a cross-platform dotfile manager written in Rust. This file orients LLM agents to the codebase. **Read this first before doing any work.**

---

## Product north star

Patina is a cross-platform dotfile manager whose source of truth is a user's centralized git repository. A user runs `patina apply` and the configurations declared in `patina.toml` files materialize at the right targets — as symbolic links pointing back into the repo, rendered template output, or byte copies where a link is not appropriate. The engine guarantees that a mid-apply crash leaves the filesystem in either the pre-apply or post-apply state, never an intermediate one.

### Users

- **Fresh-laptop developer** — clones the dotfiles repo, runs one command, expects shell/editor/git config to land everywhere.
- **Existing-machine maintainer** — adds, edits, removes config; re-runs `patina apply` expecting the diff-and-prompt loop to never surprise them.
- **Multi-machine syncer** — runs Patina across macOS / Linux / Windows; same source must produce same result everywhere.
- **Cautious user** — wants a diff and prompt before any mutation; never accidentally overwrites a file edited outside Patina.
- **CI script author** — runs `patina apply` in a non-interactive shell to preview a deployment; expects plan output and zero writes.

### V1.0 outcome

V1.0 ships when a user can:

- **Declare and apply** — `patina apply` materializes `patina.toml` as symlinks / rendered templates / byte copies at the right targets.
- **Preview safely** — diff-and-prompt by default; non-interactive shells fall through to plan-only.
- **Recover** — `patina status` reports drift; `patina rollback` restores pre-apply state; `patina debug journal` decodes the binary journal post-mortem.
- **Bootstrap** — `init`, `add`, `remove`, `promote`, `doctor` cover repo setup and migration; Windows symlink elevation via Developer Mode or UAC.
- **Watch** — background service reapplies on source changes; surfaces files modified outside Patina.

Acceptance criteria live in each SPEC's `<done-when>` and `<scenario>` blocks.

### Quality bar

- **Crash safety.** Single-fsync postcard journal + per-operation progress cursor; `kill -9` mid-apply converges deterministically on the next run.
- **Idempotency.** Re-applying against unchanged source is a no-op — same plan, no writes, byte-identical stdout.
- **Never overwrite without consent.** Files Patina doesn't own are never clobbered.
- **Rollback fidelity.** After `patina rollback`, filesystem matches pre-apply state byte-for-byte (modulo files the user touched outside Patina).
- **Deterministic stdout.** Two consecutive `apply`s against unchanged source produce byte-identical output. No timestamps, PIDs, or random IDs (`--json` included).
- **Cross-platform parity.** macOS, Linux, Windows are first-class. Two-of-three is not done.
- **No panics, tests gate truth.** Enforcement detail in `## Hard rules — never` below.

### Non-goals

Not in v1.0:

- Merge-mode file types (`merge-json`, `merge-toml`, etc.)
- Nested modules beyond two levels
- `on_change` / `on_drift` hook events
- A JSON schema-version field
- A `patina gc` command
- A `--repo <path>` global flag
- A GUI
- Migrations from other dotfile managers
- An embedded scripting language
- Native encryption
- Cross-machine state sync, machine inventory, or dashboards

If the user asks for one of these, the answer is "not in v1.0" — surface as a question for a future SPEC.

### Known unknowns

Each SPEC's `<assumptions>` block is authoritative; load-bearing for v1.0:

- **`postcard` wire-format stability** — mitigated by the journal version envelope.
- **`fs2` advisory lock semantics** — paper over POSIX `flock(2)` vs Windows `LockFileEx` for single-CLI and watcher↔CLI coordination.
- **`tokio` file I/O remains `spawn_blocking`-backed** in v1.0; we accept the cost.
- **MiniJinja strict-undefined** (including the Jinja2 `{% else %}` empty-string rule) is acceptable.
- **Per-machine state directory must not live on cloud-sync paths** (iCloud / OneDrive / Dropbox / Box / Google Drive / Syncthing). SPEC-0002 adds doctor warnings; SPEC-0001 documents only.

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

- **Never edit a locked `SPEC.md` outside of `/speccy-amend`.** Once `TASKS.md` references the spec hash, direct edits desync it and a later `speccy verify` fails. The only exception is cosmetic edits that change no `<requirement>`, `<done-when>`, `<scenario>`, `<goals>`, `<non-goals>`, or `<assumptions>` block (typo and prose fixes).
- **Never hand-edit `TASKS.md`.** It is generated by `/speccy-decompose`; state transitions (`pending → in-progress → in-review → completed`) happen via `/speccy-work` and `/speccy-review`.
- **Never mark a task `completed` based on your own assessment.** `/speccy-review`'s five adversarial personas (business, tests, security, style, correctness) decide; the implementer's self-assessment carries no weight.
- **Never rewrite history in per-task journals.** `.speccy/specs/<NNNN>-<slug>/journal/T-NNN.md` is append-only: the implementer writes one entry, reviewers may append blockers.
- **Never open a PR with a failing `speccy verify`.** `/speccy-ship` enforces it locally, and it is CI's gate.
- **Never hand-edit `.claude/skills/speccy-*` files.** They are shipped by the speccy CLI and refreshed by `speccy init --force`; local edits get clobbered on upgrade. Skill changes go upstream.

---

## Code conventions

General Rust style rules live under `.claude/rules/rust/` and are **authoritative**. The conventions below are project-specific additions and patina-specific crate choices; if anything below appears to conflict with a rule, the rule wins and this file should be updated.

- **Errors:** `thiserror` for typed errors in libraries (`patina-core`); `anyhow` for application-level error chaining in `patina-cli`. For genuinely impossible preconditions in production, refactor to make them type-system enforced or use `?` with a proper error variant. (Panic-family lints in `## Hard rules` above are non-negotiable.)
- **Paths:** `camino::Utf8PathBuf` and `camino::Utf8Path` everywhere we know paths are UTF-8 (which is most places). Convert at OS-API boundaries.
- **Filesystem:** prefer `fs-err` over `std::fs` so error messages include the path.
- **On-disk format version (pre-release no-bump policy):** the `postcard` binary formats (the journal plan, the committed apply record, the watch drift cache) share one major-version envelope, `FILE_MAJOR_VERSION` in `patina-core/src/journal/plan.rs`. **Hold the on-disk major at `1` and do not bump it per breaking change until v1.0.** Patina is pre-release with no shipped on-disk state to preserve, so a breaking layout change keeps major `1` and provides no migration; an older binary then refuses a newer file via the version envelope (`decode_envelope` rejects `found > supported`), which is acceptable while pre-release. Bump the major exactly once, at the v1.0 boundary, after which it becomes a real compatibility contract.
- **CLI output:** human-readable by default with color where appropriate, JSON when `--json` is set. Use the `output::Reporter` abstraction (introduced in Phase 10), not direct prints. Logging via `tracing` macros (see Hard rules above for the prohibition on `println!`).
- **Tests:** integration tests use `tempfile::TempDir` for repo fixtures. Snapshot tests use `insta`. Property-based tests use `proptest`.
- **Public API:** every public function has a doc comment with at least one example. `cargo doc --no-deps` must build clean.
- **Local quality gate — `just check`:** run `just check` (= `just lint` + `just test`) before a task is marked done, reviewed, vetted, or shipped — not ad-hoc `cargo`. The `pre-push` hook runs it once activated (`core.hooksPath .githooks`). `just lint` mirrors CI's gates in order (fmt, clippy, docs, deny); the **docs** gate (`cargo doc -D warnings`) is the one checklists most often drop. A green local `just check` is necessary, not sufficient — CI also runs the per-OS test behaviour matrix, macOS-native clippy, the MSRV (1.95) build, and coverage; watch PR checks after pushing. See the `justfile` header for cross-compile mechanics and one-time `rustup target add` setup.
- **Diagrams in docs:** prefer Mermaid (` ```mermaid ` fenced blocks) over ASCII when either works — it renders on GitHub and diffs cleanly per-node. Keep ASCII only for what Mermaid can't express: directory trees with inline comments, exact-byte layouts, terminal output.

---

## Project conventions

### Test hygiene

A test must gate a real invariant of the system under test — not editorial decisions, not its own source constant, not the build's own ability to compile. Do not write any of the following vacuous shapes:

1. **Substring-matching human-curated prose.** Asserting that a specific sentence appears in a hand-authored document (a README, an AGENTS file, a SPEC body) gates editorial choices, not behavior. Such tests break on legitimate rewrites. If a concept must be discoverable in docs, enforce it via review or over a stable structural surface (section IDs, frontmatter fields), not via substring match.
2. **Copying production constants into the test.** A test that hard-codes the same value the production code uses and compares them proves only that someone updated both sites in sync — it cannot fail in any interesting way. Either derive a property of the constant (length, ordering, prefix relation to another constant) or delete the test.
3. **File existence or non-emptiness only.** Reading a file already gates readability; asserting only that the file is non-empty after a successful read is tautological. Assert at least one property of the content.
4. **Mocking the function under test and asserting the mock was called.** The mock replaces the very behavior the test claims to verify. The assertion proves the test plumbing works, not the system.
5. **Loose-outcome assertions any input passes.** Assertions so permissive that any input satisfies them — checking only that a function returned without error when the function is infallible, or that an output is non-empty when the function always returns non-empty — gate nothing. Pick an assertion that would fail for at least one realistic regression.

When a test you wrote is flaky, investigate the flake. Do not retry it until green; intermittent failures point at real races, ordering assumptions, or shared state that will bite again later.

### Commit hygiene

- AI-authored commits identify themselves via the `Co-Authored-By` trailer in the commit message footer, naming the model and a contact address.
- Prefer narrow, well-scoped commits over sprawling ones. One logical change per commit makes review, revert, and bisect tractable.

---

## Speccy conventions

> Managed by `/speccy-bootstrap`; edits inside this section are
> overwritten on re-run. Put project-specific rules in a sibling
> section.

Speccy keeps intent and shipped behavior in sync through a five-phase
loop. Your harness already surfaces each skill's `description` for
routing — read those for the per-skill contract. The order and entry
points:

1. **Plan** — `/speccy-brainstorm` (fuzzy asks) → `/speccy-plan` →
   `/speccy-decompose`.
2. **Impl** — `/speccy-work`, one task per invocation.
3. **Review** — `/speccy-review`, per-task adversarial fan-out.
4. **Vet** — `/speccy-vet`, the pre-ship holistic drift gate.
5. **Ship** — `/speccy-ship`, writes `REPORT.md` and opens the PR.

`/speccy-orchestrate` drives phases 2–4 autonomously; `/speccy-amend`
handles a mid-loop SPEC change.

Per-task implementer notes and reviewer verdicts live in the journal at
`.speccy/specs/NNNN-slug/journal/T-NNN.md`, sibling to `SPEC.md` and
`TASKS.md`.

Future-spec candidates live in `.speccy/BACKLOG.md`: planning reads it as
candidate input; plan and ship append to it.

CI: wire `speccy verify` into whichever CI the project uses. It fails on
broken proof shape (missing requirement coverage, malformed task state)
and passes when intact — informational by design, not a blocker.
