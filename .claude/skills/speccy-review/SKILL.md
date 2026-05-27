---
name: speccy-review
description: 'Review one Speccy task per invocation and exit, running one round of adversarial multi-persona review. With an optional `SPEC-NNNN/T-NNN` selector, the session reviews that task; without it, the skill resolves the next reviewable task via `speccy next --json`. Four personas (business, tests, security, style) fan out in parallel and either pass the task to `completed` or flip it back to `pending` with a `<blockers>` block appended to the per-task journal file. Use when the user says "review T-003" or "review the next task". Requires: a task in `state="in-review"`. If no in-review task and work remains → prefer speccy-work. If all tasks `completed` → prefer speccy-ship. Do NOT trigger on generic "review this PR" or "review my code" asks — this skill runs Speccy task-state review only.'
---

# /speccy-review

Runs one round of adversarial review on one task per invocation and
exits. With an optional `[SPEC-NNNN/T-NNN]` selector argument, the
session reviews that specific task. Without an argument, the session
resolves the next reviewable task via `speccy next --json` (workspace
form — used here because no SPEC is known on the no-selector path) and reviews
that one. Task state lives in the `state` attribute on each `<task>`
XML element in TASKS.md; review activity prose lives in the sibling
`.speccy/specs/NNNN-slug/journal/T-NNN.md` file, never inside the
`<task>` body.

This is a single-task primitive. It does not iterate over the
remaining `in-review` tasks; composition across tasks belongs to a
caller (a human at the terminal, the `/loop` skill, or a future
orchestrator).

Within the one task under review, the skill fans out to four
parallel persona sub-agents (default fan-out: `business`, `tests`,
`security`, `style`). That fan-out is intrinsic to the primitive —
adversarial diversity comes from fresh contexts per persona — and is
bounded to one round of four sub-agents on one task.

Because sub-agents cannot spawn sub-agents, this skill must run in a
context that **is** the top-level session — either a human
invocation (`/speccy-review …`) where the host CLI
itself runs the skill body, or the
`/speccy-orchestrate` outer loop which inlines this
skill body into its own session at the `review` dispatch (it cannot
delegate to a wrapper sub-agent that would then try to spawn the
four persona leaves).

## When to use

- With a selector (`/speccy-review SPEC-NNNN/T-003`):
  when the task to review is already known.
- Without an argument: when picking up wherever `TASKS.md` left off.
  The session reviews one task and exits.

The target task must already be in `state="in-review"` (typically
flipped there by `/speccy-work`).

## Steps

**Entry precondition (REQ-007, REQ-008):** before resolving the target task, query `speccy next --json` (per-spec form when a selector was passed, workspace form otherwise). If the returned envelope's `next_action.kind == "reconcile"`, dispatch the reconcile pass per the **Reconcile policy** summary below (canonical policy at `.claude/speccy-references/reconcile-policy.md`) instead of running the normal review flow. Re-query after the pass; resume normal dispatch only when `consistency.status == "ok"`.

**Reconcile policy.** When `speccy next --json` returns `next_action.kind == "reconcile"`, iterate `consistency.drifts[]` and apply the table action per entry, then re-query before proceeding. See `.claude/speccy-references/reconcile-policy.md` for the full policy table, the three properties the dispatch holds by construction (autonomous / rollback-biased / idempotent), and the extension protocol for adding new drift kinds.

### Resolve the target task

- If a `SPEC-NNNN/T-NNN` selector was passed, that is the target.
- Otherwise, query the CLI in workspace form (no SPEC selector
  is known on this no-selector invocation path — we must walk
  the active tree to find a reviewable task):

  ```bash
  # workspace form: no SPEC-NNNN known yet; scan the active tree.
  speccy next --json
  ```

  Workspace-form exit-code-stop contract: exit code 2 with a
  top-level `reason="no_active_specs"` field in the JSON envelope
  means the workspace has no active specs at all. Exit gracefully
  and surface the reason; do not treat the non-zero exit as a CLI
  error.

  On exit code 0, if the resulting `specs` array has no entry with
  `next_action.kind == "review"`, exit and report that no
  reviewable tasks remain. Otherwise, construct the disambiguated
  `<spec>/<task>` form from the JSON's `spec_id` and
  `next_action.task_id` fields (the bare task ID is ambiguous
  across specs — every spec has its own `T-001`).

  Exit-code-stop contract: once SPEC-NNNN is resolved, any
  subsequent per-spec query (`speccy next SPEC-NNNN --json`) that
  exits non-zero means the SPEC has reached a terminal state —
  halt and surface the stderr line. Only parse JSON when exit
  code is 0.

### Run the persona fan-out and consolidation

Shared with the `/speccy-orchestrate` review
dispatch — both this skill body and that dispatch step include the
same partial below so the fan-out contract has a single source of
truth.


Fan out four reviewer-* sub-agents in parallel against the resolved
task, one per persona. Default fan-out: `reviewer-business`,
`reviewer-tests`, `reviewer-security`, `reviewer-style`. Two
additional personas (`reviewer-architecture`, `reviewer-docs`) are
off the default fan-out and are invoked explicitly when an
architectural or documentation risk is suspected.

The prompt for each spawn is:

> Review task `SPEC-NNNN/T-NNN`. Run `speccy check SPEC-NNNN/T-NNN`
> to load the task scenarios, read the bare `<task>` body in
> TASKS.md and the prior activity in
> `.speccy/specs/NNNN-slug/journal/T-NNN.md`, and apply your
> persona's review criteria. Return your verdict as your final
> message as a
> `<review persona="<persona>" verdict="..." model="...">…</review>`
> element block. The `model` attribute is required and must
> identify the model that produced the verdict (with the optional
> slash-suffix effort convention from the verdict-return contract).
> Do not edit TASKS.md and do not edit the journal file.
>
> The working tree may be dirty: the implementer leaves changes
> uncommitted on purpose, and the orchestrator (not the implementer)
> owns the single atomic commit on review pass per REQ-003/REQ-004.
> On retry rounds the dirty tree is the prior pass's WIP that the
> retry implementer amended in place per the retry-shape contract.
> Do not flag uncommitted state, commit timing, or "changes not
> committed before the in-review flip" -- those are out of scope
> for per-task review.

Substitute the resolved `SPEC-NNNN/T-NNN` and the persona name per
spawn.

Invoke the `Task` tool four times **in one message** (parallel
dispatch) with `subagent_type: "reviewer-business"`,
`subagent_type: "reviewer-tests"`,
`subagent_type: "reviewer-security"`, and
`subagent_type: "reviewer-style"`. Each persona's agent definition
at `.claude/agents/reviewer-<persona>.md` carries the host-native
dispatch metadata.

Canonical journal `<review>` shape:
`.claude/speccy-references/journal-review.md`.

Canonical journal `<blockers>` shape:
`.claude/speccy-references/journal-blockers.md`.

After all spawned sub-agents return, **consolidate** the `<review>`
element blocks from each reviewer's final message and append them
to `.speccy/specs/NNNN-slug/journal/T-NNN.md` **serially in the
running session** — do not delegate the write back to a reviewer
sub-agent, and do not write to TASKS.md.

When transcribing each returned `<review>` into the journal:

- Copy the `model` attribute **verbatim** from the reviewer's reply
  per `resources/modules/personas/verdict_return_contract.md`. Do
  not infer a model value from the persona name, the host
  skill-pack identity, or any other source. If a returned
  `<review>` is missing `model`, halt the fan-out and surface the
  non-conforming persona rather than inventing a value.
- Ensure each appended `<review>` carries the full required
  attribute set: `date` (ISO8601 with seconds and timezone),
  `model` (verbatim from the reviewer), `persona`, `verdict`
  (`pass` or `blocking`), and `round` (positive integer matching
  the implementer round under review). All five are required.
- If `journal/T-NNN.md` does not exist yet (a task can reach
  `in-review` only after the implementer wrote its round-1
  `<implementer>` block, so this should be rare — but if the file
  is somehow missing, surface that as an error rather than
  silently creating one without the implementer entry).

Apply the state transition to **TASKS.md serially in the running
session** (separate write from the journal append):

- If every spawned reviewer's `<review verdict="...">` is
  `verdict="pass"`, flip the task's `state="..."` attribute from
  `in-review` to `completed`.
- If any spawned reviewer's `<review verdict="...">` is
  `verdict="blocking"`, flip `state="..."` from `in-review` to
  `pending`, and append a single consolidated
  `<blockers>…</blockers>` element block to `journal/T-NNN.md`
  that aggregates all failing reviewers' feedback — not one
  `<blockers>` per reviewer, not a partial write. The block
  carries required attributes `date` and `round` (matching the
  round of the `<review>` blocks just appended) and has the form:

      <blockers date="2026-05-21T22:10:00Z" round="1">
      <one-line summary of what to change before the next
      implementer pass>.
      <optional bullets enumerating each persona's blocker>.
      </blockers>

This serial write in the running session eliminates the
parallel-write race that would occur if each reviewer sub-agent
wrote to the journal or TASKS.md directly (per DEC-008). Per-task
journal files do not introduce parallel writes from reviewer
sub-agents — the running session remains the sole journal writer
during review.

### Atomic commit on review pass (REQ-003, REQ-004)

When every spawned reviewer returned `verdict="pass"` and the
journal append + TASKS.md flip to `completed` are written, the
running session performs the commit step:

1. Run `git status --porcelain`. If stdout is empty, **skip the
   commit step silently** (no surface to the user, no error). This
   handles two cases uniformly: tasks whose net filesystem change is
   zero, and idempotent re-entry from the reconcile pass against an
   already-converged state.
2. If stdout is non-empty, run `git add -A` followed by `git commit`
   with the message format below. The commit captures the
   implementer's code changes, the TASKS.md state flip, and the
   journal append in a single atomic commit (parent count = 1).

Commit message format (REQ-004):

- **Title:** `[SPEC-NNNN/T-NNN]: <task title>` — `<task title>` is
  read verbatim from the `<task>` element's `## ` heading in
  TASKS.md (the one-line H2 immediately after the `<task ...>`
  opening tag). Substitute the resolved spec and task IDs.
- **Body:** the trimmed content of the `Completed` field from the
  latest `<implementer>` block in the per-task journal file. Extract
  mechanically as the bytes between the `- Completed:` bullet marker
  and the next `- <Field>:` bullet marker (one of `Undone`,
  `Hygiene checks`, `Evidence`, `Discovered issues`,
  `Procedural compliance`). Trim leading and trailing whitespace.
- **Trailer:** a single `Co-Authored-By: <model> <noreply@anthropic.com>`
  line where `<model>` is sourced from the host harness's runtime
  model identifier (env var, runtime API, or host-specific
  equivalent). When the host does not expose a model identifier,
  use the documented fallback string
  `Co-Authored-By: Speccy Skill Pack <noreply@anthropic.com>`.

Pass the body via a HEREDOC so newlines and special characters
survive verbatim, e.g.:

```
git commit -m "$(cat <<'EOF'
[SPEC-NNNN/T-NNN]: <task title>

<trimmed Completed field>

Co-Authored-By: <model> <noreply@anthropic.com>
EOF
)"
```

The title prefix is the sole task-identity link in git history; the
consistency check correlates commits to tasks by grepping for this
prefix. Do not stage selectively — `git add -A` is sound under the
clean-tree precondition (REQ-002) that fires at the start of work
dispatch, which guarantees every dirty path at commit time is
task-scoped.

The skill body does not check the current git branch; it trusts the
caller / host to have placed the working tree on a feature branch.
Commits land on whatever HEAD is.


Reviewers do not write to TASKS.md and do not write to
`journal/T-NNN.md` directly; they return their verdict to this
running session, which is the **sole writer to
`.speccy/specs/NNNN-slug/journal/T-NNN.md`** for the review-induced
journal appends and the **sole writer to TASKS.md** for the
review-induced `state` transition. No `<review>` block is ever
appended to the `<task>` body in TASKS.md — TSK-006 rejects
journal elements there.

### Exit

Do not pick up another `in-review` task. If the caller wants
another task reviewed, the caller invokes this skill again.

After exit, the next reasonable step depends on TASKS.md state:
if any task is `state="pending"` (a retry), suggest
`/speccy-work SPEC-NNNN`. If any remain
`state="in-review"`, suggest
`/speccy-review SPEC-NNNN` again. If all tasks are
`state="completed"`, suggest `/speccy-vet SPEC-NNNN`.
