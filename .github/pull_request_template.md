<!--
Thanks for contributing to patina! Fill out this template so reviewers
(human or agent) can verify the change.
-->

## Summary

<!-- One-paragraph description of what this PR changes and why. -->

## Related issue / design

- **Tracking issue:** <link, or "none">
- **Type:** <feature, bug fix, refactor, docs, …>

<!-- e.g., "Bug fix for issue #42." -->

## Validation criteria addressed

List the acceptance criteria this PR addresses and mark each as done:

- [ ] ✅ <criterion 1>
- [ ] ✅ <criterion 2>
- [ ] ✅ <criterion 3>

If any criteria are NOT addressed by this PR, explain why and what follow-up is planned:

<!-- e.g., "Manual reboot-survival validation deferred to a follow-up PR; tracked in issue #N." -->

## Notable decisions

<!-- Call out any architectural or design decisions reviewers should know about. -->

- [ ] No notable design decisions in this change.

## Documentation

Both new docs and existing docs must stay current. Confirm each:

- [ ] User-facing docs (`docs/USER_GUIDE.md`) reflect any user-observable change (CLI flags, config schema, output format, install/setup steps).
- [ ] Architecture doc (`docs/ARCHITECTURE.md`) reflects any module / data-flow / abstraction change.
- [ ] No stale cross-references (grepped for renamed paths / identifiers in `docs/`).

If a checkbox doesn't apply (no observable change in that area), state that explicitly here:

<!-- e.g., "No CLI surface change; user docs unchanged." -->

## CI and tests

- [ ] `cargo build --workspace` passes.
- [ ] `cargo test --workspace --all-features` passes.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes.
- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo doc --workspace --no-deps --all-features` builds clean (with `RUSTDOCFLAGS=-D warnings`).
- [ ] `cargo deny check` passes.
- [ ] Coverage threshold (≥85% lines) holds.
- [ ] MSRV job (`cargo build` against the documented `rust-version`) passes.
- [ ] If a manual validation checklist applies, it is signed off under `docs/manual-validation/`.

## Reviewer notes

<!-- Anything reviewers should pay extra attention to: tricky logic, places
     to push back, alternatives to discuss, etc. -->

---

*If you are an agent, identify yourself in a `Co-authored-by:` trailer in your final commit (e.g., `Co-authored-by: claude-opus-4-7 <noreply@anthropic.com>`).*
