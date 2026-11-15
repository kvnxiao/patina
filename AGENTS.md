# Patina contributor instructions

Patina is a cross-platform dotfile manager written in Rust. Read this file before working in the repository.

## Product contracts

Read [SPEC.md](SPEC.md) for product scope, shared invariants, and the area index. Before changing observable behavior, read the affected area and its declared dependencies. Keep requirement IDs and conformance scenarios consistent with the change. Treat `status: draft` as a proposed contract awaiting approval; approval establishes intended behavior, not implementation conformance.

Read [contract research](docs/research/README.md) when implementation and intended behavior differ. Keep product rules in the spec, evidence and limitations in research, and available usage and migration procedures in the user guide. Do not duplicate those contracts here.

## Always use these Skills

The full engineering rulebook is in the Skills in this repository.

- **Use the `rust-rules` Skill** before writing or reviewing **any** Rust code.
- **Use the `github-actions-rules` Skill** before authoring or editing anything under `.github/workflows/`.

Each Skill's body lists the reference documents for its task.

---

## Hard rules: never

The next review rejects any violation of these rules.

- **Never `unwrap()`, ever.** Use `?` with proper error types in production; use `.expect("descriptive message")` in tests.
- **Never `expect()` in production code.** Allowed only in tests (`clippy.toml` sets `allow-expect-in-tests = true`). See the `rust-rules` Skill's error-handling reference.
- **Never panic in production code.** No `panic!()`, `unreachable!()`, `todo!()`, or `unimplemented!()` outside `#[cfg(test)]`. Return a typed error instead.
- **Never use `println!` / `eprintln!` for user-facing output** outside the dedicated `output::Reporter` layer. Use `tracing` macros (`info!`, `warn!`, `error!`, `debug!`, `trace!`) everywhere else.
- **Never skip writing tests for "obvious" code.** Validation requires tests, not "looks right."
- **Never introduce a dependency without checking it against `deny.toml`.** License, security advisory, and bloat checks gate the merge.
- **Never let docs drift.** If your change alters observable behavior, architecture, or testing conventions, the corresponding `docs/` files change in the same PR.

---

## Code conventions

These Patina-specific rules extend the `rust-rules` Skill with crate choices. When they conflict with a Skill rule, the Skill wins; update this file.

- **Persisted formats:** Read [History format compatibility](specs/history.md#req-history-format-compatibility--history-format-compatibility) before changing journal encoding. Preserve the specified version and migration policy.
- **CLI output:** human-readable by default with color where appropriate, JSON when `--json` is set. Use the `output::Reporter` abstraction, not direct prints.
- **Tests:** integration tests use `tempfile::TempDir` for repo fixtures. Snapshot tests use `insta`. Declare `#![cfg(test)]` in an integration-test crate whose helper functions call `expect`, panic, index, or print, so the `clippy.toml` test allowances cover those helpers.
- **Diagrams in docs:** prefer Mermaid (` ```mermaid ` fenced blocks) when it can express the diagram. GitHub renders Mermaid and diffs it per node. Use ASCII for directory trees with inline comments, exact-byte layouts, and terminal output.

---

## Standard hygiene

The project's hygiene gate is **`just check`** (= `just lint` + `just test` + `just doc` + `just dependencies`). Run it before a task is done or a PR opens. Do not substitute ad-hoc `cargo` commands.

A green local `just check` is necessary, not sufficient: CI additionally runs the per-OS test-behaviour matrix, macOS-native clippy, `just check-msrv` for each package, and coverage. Watch PR checks after pushing. See the `cross_targets` comment in the `justfile` and the one-time setup in `README.md` for the cross-lint targets and tools.

---

## Project conventions

### Test hygiene

A test must gate a real invariant of the system under test. It must not gate editorial decisions, its own source constant, or the build's own ability to compile. Do not write any of the following vacuous shapes:

1. **Substring-matching human-curated prose.** Asserting that a specific sentence appears in a hand-authored document (a README, an AGENTS file, a design doc) gates editorial choices, not behavior. Such tests break on legitimate rewrites. If a concept must be discoverable in docs, enforce it in review, or over a stable structural surface such as section IDs or frontmatter fields. Do not enforce it by substring match.
2. **Copying production constants into the test.** A test that hard-codes the same value the production code uses and compares them proves only that someone updated both sites in sync. It cannot fail in any interesting way. Either derive a property of the constant (length, ordering, prefix relation to another constant) or delete the test.
3. **File existence or non-emptiness only.** Reading a file already gates readability; asserting only that the file is non-empty after a successful read is tautological. Assert at least one property of the content.
4. **Mocking the function under test and asserting the mock was called.** The mock replaces the very behavior the test claims to verify. The assertion proves only that the test plumbing works.
5. **Loose-outcome assertions any input passes.** Assertions so permissive that any input satisfies them gate nothing. Two examples: a check that a function returned without error, when the function is infallible; and a check that an output is non-empty, when the function always returns non-empty. Pick an assertion that would fail for at least one realistic regression.

When a test you wrote is flaky, investigate the flake. Do not retry it until green; intermittent failures point at real races, ordering assumptions, or shared state.

### Commit hygiene

- Prefer narrow, well-scoped commits over sprawling ones. One logical change per commit makes review, revert, and bisect tractable.
