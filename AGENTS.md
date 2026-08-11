# Patina — Agent Guide

Patina is a cross-platform dotfile manager written in Rust. This file orients LLM agents to the codebase. **Read this first before doing any work.**

---

## Product north star

Its source of truth is a user's centralized git repository. A user runs `patina apply` and the configurations declared in `patina.toml` files materialize at the right targets — as symbolic links pointing back into the repo, rendered template output, or byte copies where a link is not appropriate. The engine guarantees that an apply interrupted by process termination (`kill -9`) leaves the filesystem in either the pre-apply or post-apply state, never an intermediate one; power-loss durability is a post-1.0 hardening item (see Known unknowns).

### Users

- **Fresh-laptop developer** — clones the dotfiles repo, runs one command, expects shell/editor/git config to land everywhere.
- **Existing-machine maintainer** — adds, edits, removes config; re-runs `patina apply` expecting the diff-and-prompt loop to never surprise them.
- **Multi-machine syncer** — runs Patina across macOS / Linux / Windows; same source must produce same result everywhere.
- **Cautious user** — wants a diff and prompt before any mutation; never accidentally overwrites a file edited outside Patina.
- **CI script author** — runs `patina apply` in a non-interactive shell to preview a deployment; expects plan output and zero writes.

### V1.0 outcome

V1.0 is considered complete when a user can:

- **Declare and apply** — `patina apply` materializes `patina.toml` as symlinks / rendered templates / byte copies at the right targets.
- **Preview safely** — diff-and-prompt by default; non-interactive shells fall through to plan-only.
- **Recover** — `patina status` reports drift; `patina rollback` restores pre-apply state; `patina debug journal` decodes the binary journal post-mortem.
- **Bootstrap** — `init`, `add`, `remove`, `promote`, `doctor` cover repo setup and migration; Windows symlink elevation via Developer Mode or UAC.
- **Watch** — background service reapplies on source changes; surfaces files modified outside Patina.
- **Consume remote sources** — a module with a `[remote]` table deploys a pinned checkout of someone else's git repository; `patina.lock` is the committed statement every machine converges to, and `patina remote list` / `check` / `update` / `prune` manage it. Normative spec: `docs/REMOTE_SOURCES.md`.

### Quality bar

- **Crash safety.** Single-fsync postcard journal + per-operation progress cursor; `kill -9` mid-apply converges deterministically on the next run. Scope: process termination, where the page cache survives; power-loss / kernel-panic durability is out of scope for v1.0 (see Known unknowns).
- **Idempotency.** Re-applying against unchanged source is a no-op — same plan, no writes, byte-identical stdout.
- **Never overwrite without consent.** Files Patina doesn't own are never clobbered without consent; every overwrite is first backed up so `patina rollback` restores it.
- **Rollback fidelity.** After `patina rollback`, the filesystem matches pre-apply state in content and entry kind (file / symlink / directory), modulo mode/timestamp bits and files the user touched outside Patina.
- **Deterministic stdout.** Two consecutive `apply`s against unchanged source produce byte-identical output. No timestamps, PIDs, or random IDs (`--json` included).
- **Cross-platform parity.** macOS, Linux, Windows are first-class. Two-of-three is not done.
- **Third-party content is never trusted.** A remote checkout supplies bytes only: its `patina.toml` is never read, its `.tmpl` files are never rendered, and every byte still passes the consent diff. Pin bumps are gated (`docs/REMOTE_SOURCES.md` "The update gate"), with an honest statement of what the gate cannot stop.
- **No two entries fight over a target.** Plan-time validation rejects duplicate canonical targets and a directory-mode target that contains another entry's target, before any diff is shown.
- **No panics; tests gate truth.**

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

If the user asks for one of these, the answer is "not in v1.0" — surface as a question for a future change.

### Known unknowns

- **`postcard` wire-format stability** — mitigated by the journal version envelope.
- **`fs2` advisory lock semantics** — paper over POSIX `flock(2)` vs Windows `LockFileEx` for single-CLI and watcher↔CLI coordination.
- **`tokio` file I/O remains `spawn_blocking`-backed** in v1.0; we accept the cost.
- **MiniJinja strict-undefined** (including the Jinja2 `{% else %}` empty-string rule) is acceptable.
- **Power-loss / kernel-panic durability** — backups are not fsync'd before an overwrite, so crash safety holds under process termination (`kill -9`, page cache intact) but not a power cut. Full never-intermediate durability under power loss (atomic temp+rename target writes plus fsync of backups and parent dirs) is a post-1.0 hardening item.
- **Per-machine state directory must not live on cloud-sync paths** (iCloud / OneDrive / Dropbox / Box / Google Drive / Syncthing). Patina does not detect cloud-sync paths in v1.0; the constraint is documented only.
- **Committer timestamps are self-reported.** The update gate's future / backdating / age checks stop untargeted, fast-moving compromises but not an attacker who backdates deliberately. Plain git offers no unforgeable time source; the diff-and-prompt loop is the hard boundary.
- **Fetching a bare SHA needs server cooperation.** A server with `uploadpack.allowReachableSHA1InWant` off refuses the specced shallow fetch by exact SHA, so `remote::git::fetch_commit` falls back to a shallow fetch of the tracked ref and re-checks that the pin arrived.
- **Journal timestamps have one-second resolution.** Two applies inside one second share a `<ts>.COMMIT`, collapsing the older record. Pre-existing, and now load-bearing for the remote cache's reachability sweep.

---

## Always use these Skills

The full engineering rulebook ships as Skills in this repository.

- **Use the `rust-rules` Skill** before writing or reviewing **any** Rust code.
- **Use the `github-actions-rules` Skill** before authoring or editing anything under `.github/workflows/`.

Each Skill's body lists the reference docs to read for the task at hand, so using it pulls in the right rules.

---

## Hard rules — never

These are hard rules, not preferences. Violating any of them is grounds for the next reviewer turn to reject the work outright.

- **Never `unwrap()`, ever.** Use `?` with proper error types in production; use `.expect("descriptive message")` in tests.
- **Never `expect()` in production code.** Allowed only in tests (`clippy.toml` sets `allow-expect-in-tests = true`). See the `rust-rules` Skill's error-handling reference.
- **Never panic in production code.** No `panic!()`, `unreachable!()`, `todo!()`, or `unimplemented!()` outside `#[cfg(test)]` — return a typed error instead.
- **Never use `println!` / `eprintln!` for user-facing output** outside the dedicated `output::Reporter` layer. Use `tracing` macros (`info!`, `warn!`, `error!`, `debug!`, `trace!`) everywhere else.
- **Never skip writing tests for "obvious" code.** Validation requires tests, not "looks right."
- **Never introduce a dependency without checking it against `deny.toml`.** License, security advisory, and bloat checks gate the merge.
- **Never let docs drift.** If your change alters observable behavior, architecture, or testing conventions, the corresponding `docs/` files change in the same PR.

---

## Code conventions

Patina-specific additions and crate choices layered on the `rust-rules` Skill. Where these conflict with a Skill rule, the Skill wins and this file should be updated.

- **On-disk format version (pre-release no-bump policy):** the `postcard` binary formats (journal plan, committed apply record, watch drift cache) share one major-version envelope, `FILE_MAJOR_VERSION` in `patina-core/src/journal/plan.rs`. **Hold the major at `1` until v1.0.** Pre-release has no shipped state to preserve, so breaking layout changes keep major `1` with no migration; an older binary then refuses a newer file (`decode_envelope` rejects `found > supported`). Bump the major once, at the v1.0 boundary, where it becomes a real compatibility contract.
- **CLI output:** human-readable by default with color where appropriate, JSON when `--json` is set. Use the `output::Reporter` abstraction, not direct prints.
- **Tests:** integration tests use `tempfile::TempDir` for repo fixtures. Snapshot tests use `insta`.
- **Diagrams in docs:** prefer Mermaid (` ```mermaid ` fenced blocks) over ASCII when either works — it renders on GitHub and diffs cleanly per-node. Keep ASCII only for what Mermaid can't express: directory trees with inline comments, exact-byte layouts, terminal output.

---

## Standard hygiene

The project's hygiene gate is **`just check`** (= `just lint` + `just test`; the `pre-push` hook runs it once activated via `core.hooksPath .githooks`). Run it before a task is done or a PR opens — not ad-hoc `cargo`.

A green local `just check` is necessary, not sufficient: CI additionally runs the per-OS test-behaviour matrix, macOS-native clippy, the MSRV (1.95) build, and coverage — watch PR checks after pushing. See the `justfile` header for cross-compile mechanics and one-time `rustup target add` setup.

---

## Project conventions

### Test hygiene

A test must gate a real invariant of the system under test — not editorial decisions, not its own source constant, not the build's own ability to compile. Do not write any of the following vacuous shapes:

1. **Substring-matching human-curated prose.** Asserting that a specific sentence appears in a hand-authored document (a README, an AGENTS file, a design doc) gates editorial choices, not behavior. Such tests break on legitimate rewrites. If a concept must be discoverable in docs, enforce it via review or over a stable structural surface (section IDs, frontmatter fields), not via substring match.
2. **Copying production constants into the test.** A test that hard-codes the same value the production code uses and compares them proves only that someone updated both sites in sync — it cannot fail in any interesting way. Either derive a property of the constant (length, ordering, prefix relation to another constant) or delete the test.
3. **File existence or non-emptiness only.** Reading a file already gates readability; asserting only that the file is non-empty after a successful read is tautological. Assert at least one property of the content.
4. **Mocking the function under test and asserting the mock was called.** The mock replaces the very behavior the test claims to verify. The assertion proves the test plumbing works, not the system.
5. **Loose-outcome assertions any input passes.** Assertions so permissive that any input satisfies them — checking only that a function returned without error when the function is infallible, or that an output is non-empty when the function always returns non-empty — gate nothing. Pick an assertion that would fail for at least one realistic regression.

When a test you wrote is flaky, investigate the flake. Do not retry it until green; intermittent failures point at real races, ordering assumptions, or shared state that will bite again later.

### Commit hygiene

- AI-authored commits identify themselves via the `Co-Authored-By` trailer in the commit message footer, naming the model and a contact address.
- Prefer narrow, well-scoped commits over sprawling ones. One logical change per commit makes review, revert, and bisect tractable.
