---
description: Independently validate that a SPEC phase meets its acceptance criteria
argument-hint: <SPEC>/<PHASE>  e.g. 0001/3
---

You are validating a phase of a patina SPEC. **You did not implement this phase.** Your job is to verify, not to defend.

The argument `$ARGUMENTS` is in the form `<SPEC>/<PHASE>`. Parse it.

## Step 1 — Load the contract

1. Read `specs/<NNNN>-*/SPEC.md` for the named SPEC. Find Phase `<N>`.
2. Extract the phase's **Deliverables** and **Validation Criteria** lists.
3. Read `specs/<NNNN>-*/STATUS.md` to confirm the phase claims completion (✅ or 🔬).

If the phase is not at least 🔬 In review, there's nothing to validate yet.

## Step 2 — Verify deliverables exist

For each deliverable listed in the SPEC phase:

- Confirm the file or module exists in the repo.
- Confirm it has non-trivial content (not a stub).
- Note any deliverable that is missing or insufficient.

Produce a checklist with each deliverable marked ✓ or ✗.

## Step 3 — Verify validation criteria

For each ✅ validation criterion in the SPEC phase:

- Find the test or check that addresses it. Trace from the criterion to a specific test name or test file path.
- Run the test locally. Confirm it passes.
- Confirm the test actually exercises the behavior the criterion describes (not a tautology).

Produce a checklist with each criterion marked ✓ (validated) or ✗ (not validated, with a reason).

## Step 4 — Verify CI status

- Confirm the latest CI run on the relevant PR or branch is green.
- Confirm coverage threshold is met (≥85% lines).
- Confirm clippy is clean and fmt is clean.

## Step 5 — Verify documentation is current

Documentation drift is a quiet failure mode. Audit the docs this phase
touched or could have invalidated:

1. **User-facing (`docs/user/`):** any user-observable change (CLI flags,
   config schema, error messages, output format, install/setup steps)
   reflected in the relevant guide. If the phase introduced a new user-
   visible feature, the guide for it exists.
2. **Architecture (`docs/dev/architecture.md`):** modules added, removed,
   or renamed; new top-level abstractions; data-flow changes captured.
3. **Testing conventions (`docs/dev/testing.md`):** new test categories,
   helpers, fixtures, or patterns mentioned.
4. **Cross-references:** grep for paths, modules, identifiers, or file
   names this phase renamed or moved; any docs still referring to the old
   name are stale.
5. **SPEC integrity:** if this phase amended the SPEC, `last_updated` and
   the Revisions table are bumped (also covered by Step 7 below).

Produce a per-item ✓ / ✗ checklist with the specific evidence (file path
+ line, or "no change applicable"). A ✗ here is a REJECT verdict — the
implementer must update the docs in the same PR before re-validation. Do
not wave through stale docs because "the code is right"; future agents
read the docs.

## Step 6 — Verify manual validation (if applicable)

If the phase has a manual validation checklist in `docs/manual-validation/`:

- Confirm the file exists and is filled out.
- Confirm there is a sign-off (committer or agent name + date).
- If you are the validator, you may sign off here only if you have actually performed the manual steps. Do not sign off on someone else's manual validation.

## Step 7 — Verify SPEC and STATUS integrity

- Confirm `STATUS.md` reflects current state.
- Confirm any new ADRs are indexed in `specs/<NNNN>/decisions/README.md`.
- Confirm any SPEC amendments have a revision-table entry.

## Step 8 — Report

Produce a verdict:

**ACCEPT** — all deliverables exist, all validation criteria pass, CI is green, manual validation (if any) is signed off, STATUS is consistent.

**REJECT** — list specifically which criteria are unmet, missing, or insufficient. Be precise. The implementer should know exactly what to fix.

**INCOMPLETE** — validation could not be performed because of an environmental issue (CI down, can't run a particular test, etc.). Explain.

If you ACCEPT, optionally update STATUS.md to ✅ Complete (only if not already done) and add yourself as the validator with the date.

---

## Hard rules

- **You are not the implementer's advocate.** Be skeptical. If a test "tests the wrong thing," call it out.
- **Self-grading is invalid.** If you implemented this phase, you cannot validate it; another agent or human must.
- **Do not lower the bar to make the phase pass.** If a validation criterion is hard to test, the answer is to write the test, not to wave it through.
