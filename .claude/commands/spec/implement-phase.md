---
description: Implement a SPEC phase end-to-end with validation
argument-hint: <SPEC>/<PHASE>  e.g. 0001/3
---

You are implementing a phase of a patina SPEC.

The argument `$ARGUMENTS` is in the form `<SPEC>/<PHASE>`, for example `0001/3` for SPEC 0001 Phase 3. Parse it.

## Step 1 — Read context (do this in order, do not skip)

1. Read `AGENTS.md` at the repo root. This is the universal contract.
2. Read `specs/README.md` for the SPEC process.
3. Read `specs/<NNNN>-*/SPEC.md` for the named SPEC. Find Phase `<N>`.
4. Read `specs/<NNNN>-*/STATUS.md`. Confirm the phase is 📋 Ready.
   - If it is 🚧 In progress with a different owner, STOP and ask.
   - If it is ⏸️ Blocked, STOP and surface the blocking dependency.
   - If it is ✅ Complete, STOP — there is nothing to do.
5. Read `docs/dev/architecture.md` to understand the surrounding code.
6. Read any ADRs in `specs/<NNNN>/decisions/` that touch the same area.

## Step 2 — Update STATUS

Mark the phase 🚧 In progress in `STATUS.md`. Include:

- Today's date.
- Your agent identifier (model name and a short tag).

Commit this change as a separate commit titled `chore: start SPEC <NNNN> Phase <N>`.

## Step 3 — Plan

Before writing code:

1. Read the phase's **Goals**, **Deliverables**, and **Validation Criteria**.
2. List the files you expect to create or modify.
3. List the tests you will write to satisfy each validation criterion.
4. Note any decisions that aren't covered by the SPEC; these will become ADRs.

If anything is ambiguous in the SPEC, STOP and ask the user before proceeding. Do not invent semantics.

## Step 4 — Implement

Implement the deliverables. As you go:

- Write tests **alongside** the code, not after. The validation criteria are the test contract.
- For non-obvious decisions, write an ADR in `specs/<NNNN>/decisions/<MMMM>-<slug>.md` using the template at `docs/dev/adr-template.md`. Reference it in your commits.
- Use the project conventions documented in `AGENTS.md`: `thiserror`/`anyhow`, `camino`, `fs-err`, `tracing`, no `unwrap()` outside tests.
- Keep commits focused. Each should compile and pass tests on its own where possible.

## Step 5 — Update documentation

Before validating, walk through the documentation impact of this phase:

- **User-facing docs (`docs/user/`):** any user-observable change (CLI flags, config schema, error messages, output format, install/setup steps) needs the relevant guide updated or created.
- **Architecture docs (`docs/dev/architecture.md`):** modules added, removed, or renamed; new top-level abstractions; changed data flows.
- **Testing docs (`docs/dev/testing.md`):** new test categories, helpers, fixtures, or conventions you introduced.
- **Cross-references:** if this phase moved or renamed any file, module, or identifier, grep the repo (including SPECs, ADRs, and other docs) and fix stale references.
- **SPEC amendments:** if you amended the SPEC, bump `last_updated` and add a row to the Revisions table.

Documentation changes ship in the same PR as the code. A phase is not "done" with stale docs — the validator will reject (see `.claude/commands/spec/validate-phase.md` Step 5).

## Step 6 — Validate

Before opening a PR, locally run:

```
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo llvm-cov --all-features --workspace --fail-under-lines 85
```

Every validation criterion in the phase MUST be addressed by a test or by an explicit manual-validation entry. Walk the criteria list one by one and confirm.

## Step 7 — Open PR

Use the PR template at `.github/pull_request_template.md`. Fill in:

- The SPEC and phase reference.
- Each validation criterion, marked addressed or with a follow-up explanation.
- Any ADRs added in this PR.
- Confirmation that STATUS.md is updated.

Mark the phase 🔬 In review in STATUS.md as part of the PR.

## Step 8 — On merge

After review and merge with green CI:

- Update STATUS.md to ✅ Complete with the PR number and merge date.
- If any new ADRs were added, update `specs/<NNNN>/decisions/README.md` with their entries.
- Confirm new docs are discoverable: user guides linked from `docs/user/README.md`; architecture changes reflected in `docs/dev/architecture.md`; new test conventions in `docs/dev/testing.md`. (The full doc audit happened in Step 5; this is a final cross-link check.)

---

## Hard rules

- **Do not mark a phase ✅ Complete based on your own assessment.** CI green + reviewer approval + STATUS update is the only path.
- **Do not skip writing tests** for "obvious" code. Validation criteria require tests.
- **Do not silently change a SPEC.** Update the SPEC explicitly with a revision-table entry, in the same PR.
- **Do not introduce a dependency** without checking `deny.toml` and updating it if needed.

If you find yourself wanting to break any of these rules, STOP and ask.
