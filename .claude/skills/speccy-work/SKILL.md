---
name: speccy-work
description: 'Implement one Speccy task per invocation and exit. With an optional `SPEC-NNNN/T-NNN` selector, the session implements that task; without a selector, it resolves the next implementable task via `speccy next --json` and implements only that one. Use when the user says "implement T-003", "work the next task", "run the implementer", "pick up the next pending task in SPEC-NNNN", or wants to implement one slice against an existing task list. Requires: `TASKS.md` with ≥1 `state="pending"` task. If no `TASKS.md` → prefer speccy-decompose. If no `SPEC.md` → prefer speccy-plan. If no `.speccy/` → prefer speccy-bootstrap. Do NOT trigger for generic "fix bug" or "refactor X" asks that are not scoped to an existing Speccy task.'
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

- With a selector (`/speccy-work SPEC-NNNN/T-NNN`):
  when the next task to implement is already known.
- Without an argument: when picking up wherever `TASKS.md` left
  off. The session implements one task and exits.

## Steps

**Retry shape.** A task is in retry shape iff its journal contains
both an `<implementer>` element and a `<blockers>` element whose
`round` attribute matches the highest implementer round. Otherwise
it's first-attempt shape — the strict clean-tree gate applies. See
`.claude/speccy-references/retry-shape.md` for the full rule
statement, read-only scope, worked examples, and the
"implementer awaiting review" edge case.

Resolve the target task — without a selector, query the CLI in workspace form
with `speccy next --json` — classify its shape from the journal via the rule
above, then apply the clean-tree gate:

**Clean-tree gate.** With the target task resolved and its shape classified by
the retry-shape rule, run `git status --porcelain` and branch:

- **First-attempt shape + non-empty stdout** → stop without dispatching the
  implementer and surface the dirty paths on stderr. The gate keeps a turn from
  starting on top of unrelated working-tree changes.
- **First-attempt shape + empty stdout** → proceed.
- **Retry shape** → proceed regardless of stdout. The dirty paths are the prior
  pass's WIP that the retry implementer amends in place; no dirty-paths surface
  is written.

An orchestrator runs this gate before spawning the worker; the worker re-runs it
defensively at its own entry.


**Reconcile policy.** When `speccy next --json` (in either per-spec
or workspace form) returns `next_action.kind == "reconcile"`, iterate
`consistency.drifts[]` and apply the table action per entry, then
re-query before proceeding. See
`.claude/speccy-references/reconcile-policy.md` for the full
policy table and the three properties the dispatch holds by
construction (autonomous / rollback-biased / idempotent).


**Hygiene gate.** After the implementer turn, before flipping `state` from
`in-progress` to `in-review`, run the project's hygiene gates as defined in its
`AGENTS.md` (`## Standard hygiene` or the project-equivalent) — the gates and
their count are whatever that project declares; Speccy prescribes no fixed set.
Any non-zero exit refuses the flip and keeps the task at `in-progress`; on all
zeros, the appended `<implementer>` block's `Hygiene checks` field records one
line per gate with its exit code.

## Exit

One task moves to `state="in-review"` with a single `<implementer>` block
appended to its journal via `speccy journal append`. This is a single-task
primitive — it does not pick up the next task. Control returns to the caller;
the next reasonable step is `/speccy-review SPEC-NNNN` while any
task is `in-review`, or `/speccy-vet SPEC-NNNN` once all tasks
are `completed`.

