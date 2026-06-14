# Reconcile policy

Canonical dispatch policy for the reconcile pass. The consuming
bodies — the `speccy-orchestrate`, `speccy-review`, and `speccy-work`
skills and the `speccy-work` agent — each carry a one-paragraph
summary plus a pointer to this file; the policy table below is the
single source of truth for what each drift kind means at the
dispatch layer.

## Dispatch trigger

The reconcile pass is triggered when `speccy next --json` (in either
per-spec or workspace form) returns `next_action.kind == "reconcile"`.

The CLI sets this value whenever the envelope's `consistency.status`
field is anything other than `"ok"` — i.e. `"drift"` or `"blocked"`.
When `consistency.status == "ok"`, `next_action.kind` reflects the
normal dispatch (`work`, `review`, `ship`, `decompose`, ...) and the
calling skill proceeds normally without invoking this policy.

The calling skill (one of `/speccy-orchestrate`, `/speccy-work`,
`/speccy-review`) iterates the `consistency.drifts[]` array and
applies one action per entry per the table below.

## Policy table

All `state` flips in the actions below run through `speccy task
transition SPEC-NNNN/T-NNN --to <state>` — the CLI enforces the legal
state graph and rewrites the `state` attribute byte-surgically. Never
edit a `state` attribute in TASKS.md with file-editing tools.

| `kind` | `severity` | Action |
|---|---|---|
| `commit_without_state` | `auto_fixable` | `speccy task transition SPEC-NNNN/T-NNN --to completed`. |
| `state_completed_no_commit` (dirty tree, `details.working_tree_dirty == true`) | `blocking` | Run `git add -A` followed by `git commit` using the standard commit message format (title `[SPEC-NNNN/T-NNN]: <task title>`; body extracted from the latest `<implementer>` block's `Completed` field in `journal/T-NNN.md`; `Co-Authored-By` trailer per host). |
| `state_completed_no_commit` (clean tree, `details.working_tree_dirty == false`) | `blocking` | `speccy task transition SPEC-NNNN/T-NNN --to in-review` to roll the task back. Journal file is preserved intact as evidence for the next reviewer round. |
| `state_in_progress_orphaned` | `blocking` | Run `git restore .` and `git clean -fd` to discard the partial implementer work, then `speccy task transition SPEC-NNNN/T-NNN --to pending`. The orchestrator's per-task retry budget will redo the work. |
| `state_in_progress_clean` (`details.working_tree_dirty == false`) | `blocking` | `speccy task transition SPEC-NNNN/T-NNN --to pending` to roll the task back. No git mutation — the tree is already clean, so there is no partial work to discard. The orchestrator's per-task retry budget will redo the work. |
| `journal_xml_malformed` | `blocking` | Truncate the journal file at `details.journal_path` to `details.last_well_formed_byte_offset` bytes — a corruption-recovery truncation, the one journal mutation with no `journal append` equivalent. Then run `speccy task transition` to reset the corresponding TASKS.md `state` to whatever the truncated journal implies (if the last well-formed element is `<implementer>`, `--to in-review`; if a closing `<review>` block survived and all spawned personas passed, the per-task journal already reflects a passing round and state may flip `--to completed` via the standard commit step). |

## Post-dispatch re-query discipline

After applying actions for every entry in `consistency.drifts[]`, the
skill re-queries `speccy next --json` (in the same per-spec form it
was invoked with).

- If `consistency.status == "ok"` on the re-query, the skill resumes
  its normal dispatch on the returned `next_action.kind`.
- If `consistency.status` is still `"drift"` or `"blocked"`, the
  skill applies actions again for the new drift list. The mechanism
  has no hard round budget; idempotency plus re-detectability on
  subsequent sessions bounds the worst case.

## Three properties

The reconcile pass holds three properties by construction:

1. **Autonomous.** The pass applies the action for each drift kind
   without prompting the user, without surfacing a fork ("re-commit
   or roll back?"), and without halting the orchestration loop. No
   `AskUserQuestion` invocation, no "press enter to continue"
   surface, appears anywhere in the dispatch path. The policy table
   above is exhaustive over the documented enum; an unknown `kind`
   is the only path that escalates (treated as `blocking` and
   surfaced to the caller).

2. **Rollback-biased.** When recovery is ambiguous — most notably
   `state_completed_no_commit` with a clean working tree, meaning
   the lost commit's content is truly unrecoverable — the policy
   prefers rolling backward (TASKS.md state reset to `in-review`,
   journal preserved as evidence) over any forward-recovery attempt
   that might guess at lost content. The orchestrator's per-task
   retry budget absorbs the redo cost.

3. **Idempotent.** Each policy action is a no-op when applied to
   already-converged state. Re-running `git add -A && git commit`
   on a clean tree produces no commit; re-running `speccy task
   transition … --to <state>` when the state is already at the target
   value is a same-state no-op (exit 0, file byte-identical);
   truncating a journal at its current length is a no-op.
   Successive session crashes during reconciliation converge to the
   same eventual state.
