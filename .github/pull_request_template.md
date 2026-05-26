<!--
Thanks for contributing to patina! Fill out this template so reviewers
(human or agent) can verify the change against the SPEC.
-->

## Summary

<!-- One-paragraph description of what this PR changes and why. -->

## SPEC reference

- **SPEC:** `specs/<NNNN>-<slug>/SPEC.md`
- **Phase:** Phase `<N>` — `<title>`

If this PR isn't a SPEC phase implementation, explain what it is (bug fix, SPEC amendment, ADR, refactor):

<!-- e.g., "Bug fix for issue #42; no SPEC change required." -->

## Validation criteria addressed

Copy the relevant phase's validation criteria here and mark each as addressed:

- [ ] ✅ <criterion 1>
- [ ] ✅ <criterion 2>
- [ ] ✅ <criterion 3>

If any criteria are NOT addressed by this PR, explain why and what follow-up is planned:

<!-- e.g., "Manual reboot-survival validation deferred to a follow-up PR; tracked in issue #N." -->

## ADRs

If this PR introduces architectural decisions not covered by the SPEC, link the ADRs:

- `specs/<NNNN>/decisions/<MMMM>-<slug>.md` — <one-line summary>

If no ADRs were needed, confirm:

- [ ] No new ADRs required for this change.

## STATUS update

- [ ] `specs/<NNNN>-<slug>/STATUS.md` is updated to reflect the new phase status.
- [ ] If a phase moves to ✅ Complete, the STATUS table includes the PR number and date.

## Documentation

Per AGENTS.md "Definition of done" #3, both new docs and existing docs must be current. Confirm each:

- [ ] User-facing docs (`docs/user/`) reflect any user-observable change in this phase (CLI flags, config schema, output format, install/setup steps).
- [ ] Architecture doc (`docs/dev/architecture.md`) reflects any module / data-flow / abstraction change.
- [ ] Testing doc (`docs/dev/testing.md`) captures any new test category, helper, or pattern introduced.
- [ ] No stale cross-references (grepped for renamed paths / identifiers in `docs/`, `specs/`, and ADRs).
- [ ] If the SPEC was amended, `last_updated` and the Revisions table are bumped.

If a checkbox doesn't apply (the phase had no observable change in that area), state that explicitly here:

<!-- e.g., "No CLI surface change in this phase; user docs unchanged." -->

## CI and tests

- [ ] `cargo build --workspace` passes.
- [ ] `cargo test --workspace --all-features` passes.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes.
- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo doc --workspace --no-deps --all-features` builds clean (with `RUSTDOCFLAGS=-D warnings`).
- [ ] `cargo deny check` passes.
- [ ] Coverage threshold (≥85% lines) holds.
- [ ] MSRV job (`cargo build` against the documented `rust-version`) passes.
- [ ] If a manual validation checklist applies, it is signed off in `docs/manual-validation/<phase>.md`.

## Reviewer notes

<!-- Anything reviewers should pay extra attention to: tricky logic, places
     to push back, alternatives to discuss, etc. -->

---

*If you are an agent, identify yourself in a `Co-authored-by:` trailer in your final commit (e.g., `Co-authored-by: claude-opus-4-7 <noreply@anthropic.com>`).*
