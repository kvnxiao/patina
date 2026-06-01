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
- **CLI output:** human-readable by default with color where appropriate, JSON when `--json` is set. Use the `output::Reporter` abstraction (introduced in Phase 10), not direct prints. Logging via `tracing` macros (see Hard rules above for the prohibition on `println!`).
- **Tests:** integration tests use `tempfile::TempDir` for repo fixtures. Snapshot tests use `insta`. Property-based tests use `proptest`.
- **Public API:** every public function has a doc comment with at least one example. `cargo doc --no-deps` must build clean.
- **Local quality gate — `just check`:** the project's standard hygiene suite is `just check` (= `just lint` + `just test`), run before a task is marked done, reviewed, vetted, or shipped — not ad-hoc `cargo` invocations. The `pre-push` git hook runs it automatically once activated (`git config core.hooksPath .githooks`; see `.githooks/README.md`). `just lint` runs CI's lint gates in CI's order: nightly `fmt --all --check`, clippy (`--workspace --all-targets --all-features --locked -- -D warnings`), `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`, and `cargo deny check`. The **docs** gate is the one hand-run checklists most often omit — broken or redundant intra-doc links fail *only* under `cargo doc -D warnings`, never under `clippy` — so always go through `just lint` rather than re-typing the cargo commands and dropping `docs`. `just test` is `cargo test --workspace --locked`. Clippy runs as its own gate: CI lints each OS **natively** (the `Clippy (<os>)` matrix over ubuntu/macos/windows, separate from the `Test` matrix), so every `#[cfg(...)]`-gated path is checked on its real target. `just` runs on one OS, so `lint-clippy` instead **cross-compiles** the non-host targets (`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-gnu`) — clippy never links, so a `cfg(windows)` lint or compile error fails on `just check` here instead of only on CI's Windows runner. The macOS target compiles Objective-C (`notify-rust` → `mac-notification-sys`), so it can only be linted from a macOS host; off a Mac, `lint-clippy` skips it and CI's `macos-latest` leg is the backstop. (Cross targets need a one-time `rustup target add`; see the `justfile` header.) What `just` **cannot** reproduce on a single dev box, and what therefore only fails on the PR's CI checks: the **Windows/macOS/Linux test *behaviour* matrix** (clippy proves the cfg code compiles and lints, not that it *runs* correctly — a path-separator assumption that passes on your OS can still fail another at runtime), macOS-cfg clippy when you are not on a Mac, the **MSRV (Rust 1.95)** build, and **coverage**. Watch the PR checks after pushing; a green local `just lint` + `just test` is necessary, not sufficient. Also note `cargo test --workspace` can pass against a *stale* `target/` (e.g. a feature-gated bin left over from an earlier `--features` build) where a fresh CI checkout fails — when in doubt, `cargo clean -p <crate>` first.
- **Diagrams in docs:** when a diagram could be expressed equivalently in either ASCII art or Mermaid, use Mermaid (` ```mermaid ` fenced code blocks). Mermaid renders natively on GitHub, in most viewers, and diffs cleanly per-node/per-edge instead of redrawing the whole picture. Keep ASCII only when it conveys information Mermaid loses — directory trees with inline comments, exact-byte file layouts, terminal output examples.

---

## Speccy conventions

> Managed by `/speccy-init`; edits inside this section are overwritten on re-run. Put project-specific additions in a sibling section.

### When to use which skill

- `/speccy-init` — bootstrap a new Speccy workspace by scaffolding `.speccy/` and seeding both the product north star and this conventions section into `AGENTS.md`. Run once per project before any other `speccy-*` skill. Re-running refreshes this section.
- `/speccy-brainstorm` — atomize a fuzzy ask into first-principle requirements before any `SPEC.md` is written. Use when the user says "help me brainstorm", "let's think about X", or when the scope is unclear. Stops at a hard gate until the framing is user-approved.
- `/speccy-plan` — draft a new `SPEC.md` from the product north star. Use when the user says "write a spec", "draft a SPEC", or "spec out X". Requires `.speccy/` and `AGENTS.md`.
- `/speccy-amend` — orchestrate a mid-loop SPEC change. Edits `SPEC.md` with a Changelog row, reconciles `TASKS.md`, and re-records the spec hash. Use when requirements shift or `speccy` reports the SPEC and tasks are out of sync.
- `/speccy-decompose` — decompose a SPEC into a checklist of agent-sized tasks in `TASKS.md`, or reconcile the list after an amendment. Use when the user says "break the spec into tasks" or the task list looks stale.
- `/speccy-work` — implement one Speccy task per invocation. With an optional `SPEC-NNNN/T-NNN` selector, implements that task; without one, resolves the next implementable task. Use when the user says "implement T-003" or "work the next task".
- `/speccy-review` — review one Speccy task per invocation by fanning out adversarial multi-persona review (business, tests, security, style by default). Passes the task to `completed` or flips it back to `pending` with a blockers block in the journal.
- `/speccy-vet` — run a holistic SPEC-vs-implementation drift review at the pre-ship boundary, with an autonomous drift-fix retry loop and a simplifier polish pass. Use when the user says "check for drift before shipping".
- `/speccy-ship` — close out a Speccy spec: write `REPORT.md`, run `speccy verify`, commit, and open a pull request. Use when every task is `state="completed"`.
- `/speccy-orchestrate` — drive the full implementation + review loop for one SPEC end-to-end by chaining `/speccy-work`, `/speccy-review`, and `/speccy-vet` until the spec is ready-to-ship. Stops one step before shipping so the operator can decide.

### The dev loop

Speccy work moves through five phases:

1. **Plan** — draft `SPEC.md` (`/speccy-plan`, optionally preceded by `/speccy-brainstorm`).
2. **Tasks** — decompose into agent-sized work (`/speccy-decompose`).
3. **Impl** — implement one task at a time (`/speccy-work`).
4. **Review** — adversarial per-task review (`/speccy-review`), followed by holistic pre-ship drift review (`/speccy-vet`).
5. **Ship** — produce the report and open the PR (`/speccy-ship`).

Per-task implementer notes, reviewer verdicts, and blocker directives all live in a per-task journal file at `.speccy/specs/NNNN-slug/journal/T-NNN.md`, sibling to `SPEC.md` and `TASKS.md`. Inspect that file to follow the conversation between implementer and reviewer rounds for any given task.

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

### CI gate (suggestion)

`speccy verify` is designed to run as a CI gate. It fails when the proof shape is broken (missing requirement coverage, malformed task state, parser-rejected journal elements) and passes when intact. Wire it into whichever CI service the project uses so drift surfaces on every push rather than at ship time. The gate is informational by design: it tells you when the contract between intent and shipped behavior is visibly broken; it does not block anyone from making mistakes.
