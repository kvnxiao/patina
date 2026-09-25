<!--
Complete this template so reviewers can verify the change.
-->

## Summary

<!-- Describe what this PR changes and why in one paragraph. -->

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

<!-- e.g., "Manual Windows UAC-prompt validation deferred to a follow-up PR; tracked in issue #N." -->

## Notable decisions

<!-- List architectural or design decisions reviewers should know about. -->

- [ ] No notable design decisions in this change.

## Documentation

New and existing docs must stay current. Confirm each:

- [ ] User-facing docs (`docs/USER_GUIDE.md`) reflect any user-observable change (CLI flags, config schema, output format, install/setup steps).
- [ ] Architecture doc (`docs/ARCHITECTURE.md`) reflects any module / data-flow / abstraction change.
- [ ] No stale cross-references (grepped for renamed paths / identifiers in `docs/`).

If a checkbox doesn't apply (no observable change in that area), state that explicitly here:

<!-- e.g., "No CLI surface change; user docs unchanged." -->

## CI and tests

- [ ] `just lint` passes (nightly fmt check and Clippy with `-D warnings`).
- [ ] `just test` passes.
- [ ] `just doc` passes.
- [ ] `just dependencies` passes (`cargo audit`, `cargo machete`, `cargo deny check`).
- [ ] Line coverage is reported here: <n>%, from the `TOTAL` row of `cargo llvm-cov report --summary-only` after `just coverage`, or from Codecov's project status on this PR.
- [ ] `just check-msrv <package>` passes for each package.
- [ ] If a manual validation checklist applies, it is signed off under `docs/manual-validation/`.

## Reviewer notes

<!-- List tricky logic, places to challenge, and alternatives to discuss. -->

---

*If you are an agent, name yourself in a `Co-authored-by:` trailer on your final commit (e.g. `Co-authored-by: claude-opus-5 <noreply@anthropic.com>`).*
