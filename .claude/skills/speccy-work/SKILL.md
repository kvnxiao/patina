---
name: speccy-work
description: 'Implement one Speccy task per invocation and exit. With an optional `SPEC-NNNN/T-NNN` selector, the session implements that task; without a selector, it resolves the next implementable task via `speccy next --json` and implements only that one. Use when the user says "implement T-003", "work the next task", "run the implementer", "pick up the next pending task in SPEC-NNNN", or wants to implement one slice against an existing task list. Requires: `TASKS.md` with ≥1 `state="pending"` task. If no `TASKS.md` → prefer speccy-decompose. If no `SPEC.md` → prefer speccy-plan. If no `.speccy/` → prefer speccy-init. Do NOT trigger for generic "fix bug" or "refactor X" asks that are not scoped to an existing Speccy task.'
---
# /speccy-work

Read `.claude/agents/speccy-work.md` and follow it, or invoke
`/agent speccy-work` for the pinned execution path.

Implements one task per invocation and exits. With an optional
`[SPEC-NNNN/T-NNN]` selector argument, the session implements that
specific task. Without an argument, the session resolves the next
implementable task via `speccy next --json` (workspace form) and
implements that one.

## When to use

- With a selector (`/speccy-work SPEC-NNNN/T-003`):
  when the next task to implement is already known.
- Without an argument: when picking up wherever `TASKS.md` left
  off. The session implements one task and exits.

## Steps

**Entry precondition (SPEC-0045 REQ-002, extended by SPEC-0047 REQ-002):** resolve the target task, read `<spec-dir>/journal/T-NNN.md` (if it exists) and apply the retry-shape invariant below, then run `git status --porcelain`. **First-attempt shape** with non-empty stdout exits the skill with the dirty-paths surface on stderr (today's SPEC-0045/REQ-002 behaviour, unchanged); empty stdout proceeds. **Retry shape** proceeds regardless of stdout — the dirty paths are the prior pass's WIP that the retry implementer amends in place. If `speccy next --json` then returns `next_action.kind == "reconcile"`, dispatch per the reconcile-policy invariant below instead of the implementer.

Resolve the target task. Without a selector, query the CLI in
workspace form:

```bash
speccy next --json
```

**Retry shape.** A task is in retry shape iff its journal contains
both an `<implementer>` element and a `<blockers>` element whose
`round` attribute matches the highest implementer round. Otherwise
it's first-attempt shape — the strict clean-tree gate applies. See
`.claude/speccy-references/retry-shape.md` for the full rule
statement, read-only scope, worked examples, and the
"implementer awaiting review" edge case.

**Reconcile policy.** When `speccy next --json` (in either per-spec
or workspace form) returns `next_action.kind == "reconcile"`, iterate
`consistency.drifts[]` and apply the table action per entry, then
re-query before proceeding. See
`.claude/speccy-references/reconcile-policy.md` for the full
policy table and the three properties the dispatch holds by
construction (autonomous / rollback-biased / idempotent).


**Hygiene gate (REQ-001):** after the implementer turn, before flipping `state` from `in-progress` to `in-review`, run `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo +nightly fmt --all --check`, `cargo deny check`. Any non-zero exit refuses the flip and keeps the task at `in-progress`; on all zeros, the appended `<implementer>` block's `Hygiene checks` field carries one line per gate naming its exit code.

