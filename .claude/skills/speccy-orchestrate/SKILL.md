---
name: speccy-orchestrate
description: 'Drive the Speccy implementation + review loop for one SPEC end-to-end by chaining speccy-work, speccy-review, and speccy-vet until the spec is ready to ship. Use when the user says "orchestrate SPEC-NNNN", "speccy-orchestrate SPEC-NNNN", "run the full loop on SPEC-NNNN", "autopilot SPEC-NNNN", or wants to drive a spec from current state to ready-to-ship without chaining single-task skills by hand. Requires: an existing SPEC-NNNN with TASKS.md. Stops one step before shipping (calling speccy-ship is irreversible — it opens a PR) and asks the user before continuing. Do NOT trigger for ad-hoc "implement one task" or "review one task" asks — prefer speccy-work or speccy-review for single-task primitives.'
---

# /speccy-orchestrate

Thin composition layer over the Speccy single-task primitives.
Queries `speccy next SPEC-NNNN --json`, dispatches each step to one or more
sub-agents, and re-queries until the SPEC is ready to ship. This
skill itself holds the outer dispatch loop, the per-task retry
counter, and (for review and ship steps) the multi-persona /
multi-round fan-out — all leaf work happens in sub-agent contexts
that exit when done.

**Why fan-outs run inline in this skill's session.** Sub-agents
cannot spawn sub-agents. The persona fan-out in `speccy-review`
(four reviewer personas in parallel) and the drift-fix loop in
`speccy-vet` (reviewer + implementer + simplifier sub-agents across
up to three rounds) therefore cannot be delegated to a single
"wrapper" sub-agent that follows the skill body — the wrapper would
fail to spawn its leaf sub-agents. Instead, this orchestrator
follows the `speccy-review` and `speccy-vet` skill bodies **inline
in its own session** and spawns the leaf sub-agents directly. Only
`speccy-work` (which never fans out) is delegated to a single
sub-agent.

## When to use

- The user wants to drive `SPEC-NNNN` from its current state to
  ready-to-ship without chaining `/speccy-work`,
  `/speccy-review`, and
  `/speccy-vet` by hand.
- The SPEC already has a `TASKS.md` — this orchestrator dispatches
  against existing tasks; it does not plan or decompose.

Stops one step before invoking `/speccy-ship`
(ship opens a PR — irreversible — and is always confirmed by the
user). Do not invoke this skill for ad-hoc "implement one task" or
"review one task" asks; prefer the single-task primitives.

## Argument

```
/speccy-orchestrate SPEC-NNNN
```

The `SPEC-NNNN` argument is required. If missing, ask the user
which spec to drive and exit without looping.

## Lifecycle (outer loop + inline fan-outs)

```
outer:    speccy-orchestrate dispatch loop  ← this skill's session
            └── on `next_action.kind`:
                  ├── work   → spawn ONE speccy-work sub-agent
                  ├── review → fan out 4 reviewer-* sub-agents
                  │             in parallel from this session
                  │             (follows the speccy-review skill body)
                  ├── vet    → run the speccy-vet skill body inline
                  │             in this session, spawning
                  │             vet-reviewer / vet-implementer /
                  │             vet-simplifier leaf sub-agents directly
                  │             (drift-fix loop bounded to 3 rounds,
                  │              then one simplifier polish pass)
                  └── ship   → ask the user, then spawn a
                                speccy-ship sub-agent on confirm
inner-1:  per-task retry — same task_id flipping pending after review
            (bounded here in the orchestrator: 5 rounds, then stop)
inner-2:  holistic drift fix — described in speccy-vet, run inline
            here (bounded: 3 rounds, then fail)
inner-3:  simplifier polish — described in speccy-vet, run inline
            here (no loop: one scan + one apply with hygiene gate)
```

The orchestrator owns the outer loop, the per-task retry counter,
the review consolidation step (journal append + TASKS.md state
flip), and the vet round counter / snapshot management. Only the
leaf work (one implementer pass, one persona review, one drift
review, one drift fix, one simplifier scan / apply) lives in a
spawned sub-agent.

## Context discipline

`speccy-work` dispatches to **one** sub-agent because the
implementer pass is single-shot and does not need to spawn its own
sub-agents. Its final message comes back as a short status hint.

`speccy-review` and `speccy-vet` cannot be delegated to a single
wrapper sub-agent because they themselves fan out — and sub-agents
cannot spawn sub-agents. This orchestrator follows their skill
bodies inline in its own session and spawns the leaf sub-agents
(`reviewer-business`, `reviewer-tests`, `reviewer-security`,
`reviewer-style`, `vet-reviewer`, `vet-implementer`,
`vet-simplifier`) directly. The leaf sub-agents each return one
short verdict block as their final message; only those final
messages — not the per-persona reasoning — flow back into the
orchestrator's context.

Sub-agent final messages are **status hints, not state**. The
orchestrator always re-queries `speccy next SPEC-NNNN --json` after a
dispatch settles to get ground truth.

**Exit-code-stop contract.** If `speccy next SPEC-NNNN --json` exits
non-zero, the SPEC has reached a terminal state — halt the loop and
surface the stderr line to the caller. Only parse the JSON envelope
when exit code is 0.

## Startup integrity check

Before entering the dispatch loop, run one one-time sanity check
to catch state left by a crashed prior session.

**Reconcile-pass dispatch (REQ-007, REQ-008).** The very first
`speccy next SPEC-NNNN --json` of the session — the same one that
resolves the spec directory in step 1 below — may return
`next_action.kind == "reconcile"` when the CLI's consistency check
flags drift from a prior crashed session. When it does, dispatch the
reconcile pass per the **Reconcile policy** summary below (canonical
policy at `.claude/speccy-references/reconcile-policy.md`)
**before** entering the dispatch loop. Apply the per-drift action
for every entry in `consistency.drifts[]`, then re-query
`speccy next SPEC-NNNN --json`. Only when the re-query returns
`consistency.status == "ok"` does this orchestrator proceed to the
dispatch loop. The reconcile pass is autonomous — no
`AskUserQuestion`, no "press enter to continue" surface, anywhere
in the dispatch path. Each subsequent loop iteration's
`speccy next SPEC-NNNN --json` (step 1 of the [Loop](#loop) section
below) re-runs the same check: if a per-task operation leaves
drift, the next iteration catches it and dispatches the reconcile
pass before continuing to the normal `work` / `review` / `ship`
dispatch. The reconcile pass owns every drift kind in REQ-006's
enum — including `state_in_progress_orphaned` (dirty tree) and
`state_in_progress_clean` (clean tree) — so the orchestrator no
longer scans TASKS.md for `state="in-progress"` itself.

**Reconcile policy.** When `speccy next SPEC-NNNN --json` returns
`next_action.kind == "reconcile"`, iterate `consistency.drifts[]` and
apply the table action per entry, then re-query before proceeding.
See `.claude/speccy-references/reconcile-policy.md` for the full
policy table, the three properties the dispatch holds by construction
(autonomous / rollback-biased / idempotent), and the extension
protocol for adding new drift kinds.

1. Resolve the spec directory from `speccy next SPEC-NNNN --json`:
   take the `spec_md_path` field and strip the trailing `/SPEC.md`
   to get `<spec-dir>`. If the command exits non-zero, surface the
   stderr line and stop — the SPEC is in a terminal state. If the
   command reports the spec is unknown, stop and report.

2. If `next_action.kind == "reconcile"`, run the reconcile pass per
   the **Reconcile policy** summary above (canonical policy at
   `.claude/speccy-references/reconcile-policy.md`), then re-query
   and continue. Otherwise proceed directly to the dispatch loop.

## Loop

Repeat until a stop condition fires:

1. Query the CLI:

   ```bash
   speccy next SPEC-NNNN --json
   ```

   If the command exits non-zero, the SPEC has reached a terminal
   state (completed / dropped / superseded). Surface the stderr
   line to the caller and stop the loop. Only parse the JSON
   envelope when exit code is 0.

2. Dispatch on `next_action.kind`:

   - **`work`** — execute the [Work dispatch](#work-dispatch)
     section below.
   - **`review`** — execute the [Review dispatch](#review-dispatch)
     section below.
   - **`vet`** — execute the [Vet dispatch](#vet-dispatch) section
     below.
   - **`ship`** — execute the [Ship dispatch](#ship-dispatch)
     section below.
   - **`decompose`** — STOP. Tell the user to run
     `/speccy-decompose` first; the orchestrator cannot
     loop on a spec without a task list.
   - **anything else** (unknown kind, missing field, `done`,
     `plan`, etc.) — STOP and report the observed `next_action`
     verbatim so the user can react.

3. After the dispatch settles (the one `speccy-work` sub-agent
   returns; or the four reviewer-* personas have all returned and
   this orchestrator has written the journal + TASKS.md; or the
   inline vet workflow has written its `<gate>` block), re-query
   `speccy next SPEC-NNNN --json` and loop from step 1.

## Work dispatch

**Clean-tree precondition (SPEC-0045 REQ-002, extended by SPEC-0047
REQ-002).** Before spawning the `speccy-work` sub-agent, run three
steps in the orchestrator's running session:

1. Resolve the target task from `next_action.task_id`.
2. Read `<spec-dir>/journal/T-NNN.md` (if it exists) and apply the
   retry-shape rule summarized immediately below (canonical
   statement at `.claude/speccy-references/retry-shape.md`).
3. Run `git status --porcelain`. **First-attempt shape** with
   non-empty stdout halts the outer loop and surfaces the dirty
   paths to the user — no `speccy-work` sub-agent is spawned, and
   the loop does not advance (today's SPEC-0045/REQ-002 behaviour,
   unchanged). Empty stdout proceeds to the dispatch below.
   **Retry shape** proceeds to spawn the `speccy-work` sub-agent
   regardless of stdout — the dirty paths are the prior pass's WIP
   that the retry implementer amends in place; no dirty-paths
   surface is written. The retry case is the one named by the
   `SPEC-NNNN → work T-NNN (retry 2/5 after blocking review)`
   status line below.

This check fires in the orchestrator's session (not delegated to
the sub-agent) because the outer loop's invariant — every
dispatched implementer turn begins on a known tree shape — must be
enforced by the loop owner. The `speccy-work` skill body re-runs
the same retry-aware precondition defensively at its own entry as
a second-level guard.

**Retry shape.** A task is in retry shape iff its journal contains
both an `<implementer>` element and a `<blockers>` element whose
`round` attribute matches the highest implementer round. Otherwise
it's first-attempt shape — the strict clean-tree gate applies. See
`.claude/speccy-references/retry-shape.md` for the full rule
statement, read-only scope, worked examples, and the
"implementer awaiting review" edge case.

Spawn a sub-agent that runs the `speccy-work` primitive for the
resolved task. Prompt:

> Implement task `SPEC-NNNN/T-NNN` per the `speccy-work` skill.
> Single-task primitive; do not iterate. Keep your final message
> short — the caller reads `speccy next SPEC-NNNN --json` for ground truth.

Substitute the resolved `task_id` from `next_action.task_id`.

Invoke the `Task` tool with `subagent_type: "speccy-work"`. The
sub-agent definition at `.claude/agents/speccy-work.md` carries
the host-native dispatch metadata.

## Review dispatch

Run the `speccy-review` skill body **inline in this orchestrator
session** (do NOT wrap it in a single general-purpose sub-agent —
that wrapper would need to spawn the four persona leaves, and
sub-agents cannot spawn sub-agents). The shared partial below is
the single source of truth, included by both this orchestrator's
review dispatch and the `/speccy-review` skill
body.


Fan out five reviewer-* sub-agents in parallel against the resolved
task, one per persona. Default fan-out: `reviewer-business`,
`reviewer-tests`, `reviewer-security`, `reviewer-style`,
`reviewer-correctness`. Two additional personas
(`reviewer-architecture`, `reviewer-docs`) are off the default
fan-out and are invoked explicitly when an architectural or
documentation risk is suspected.

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

Invoke the `Task` tool five times **in one message** (parallel
dispatch) with `subagent_type: "reviewer-business"`,
`subagent_type: "reviewer-tests"`,
`subagent_type: "reviewer-security"`,
`subagent_type: "reviewer-style"`, and
`subagent_type: "reviewer-correctness"`. Each persona's agent
definition at `.claude/agents/reviewer-<persona>.md` carries the
host-native dispatch metadata.

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
  line where `<model>` is the model segment sourced per the
  "Sourcing your recorded identity" rule — the host's in-context
  identifier transcribed verbatim in hyphen form (e.g.
  `claude-opus-4-8[1m]`), never a dotted form or a configured alias.
  When the host states no resolved identifier in-context, use the
  documented fallback string
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


After the write settles, increment the per-task retry counter if
the task flipped back to `pending` (this is what feeds the
5-round stop condition below). Then re-query
`speccy next SPEC-NNNN --json`.

The `speccy-review` skill remains independently invocable as
`/speccy-review`; this orchestrator's review
dispatch shares the same fan-out contract via the partial above so
behaviour stays in sync across invocation paths.

## Vet dispatch

Run the `speccy-vet` skill body **inline in this orchestrator
session** (do NOT wrap it in a single general-purpose sub-agent —
that wrapper would need to spawn the vet-reviewer /
vet-implementer / vet-simplifier leaves across up to three rounds,
and sub-agents cannot spawn sub-agents). The vet-phases grammar
lives canonically in the `/speccy-vet` skill body
(which includes the `modules/skills/partials/vet-phases.md`
partial); this site carries only a pointer summary so the two
invocation paths stay in sync without duplicating the grammar.

**Vet phases.** Phase 0 bootstraps the journal; Phase 1 runs drift
review with an autonomous fix-and-retry loop; Phase 2 runs the
simplifier polish pass; Phase 3 writes the final `<gate>` block. Run
in order; see `.claude/skills/speccy-vet/SKILL.md` § Phase N for the full grammar.

After the vet workflow appends its `<gate>` block and surfaces a
verdict to this orchestrator session, react as follows:

- `verdict="pass"` → write a one-line summary plus the round and
  simplifier counters, then re-query
  `speccy next SPEC-NNNN --json`. The next iteration will observe
  `next_action.kind == "ship"` and route to the [Ship
  dispatch](#ship-dispatch) section below.
- `verdict="fail"` → surface the drift summary and one-line
  suggested next step. Stop the outer loop. The user decides how
  to address it (`/speccy-amend`, manual edits,
  etc.).

## Ship dispatch

The `ship` kind is emitted by the CLI after a fresh passing
vet-gate artifact lands and `REPORT.md` is absent, so the vet
workflow has already completed and the only remaining step is user
confirmation before opening a PR.

Ask the user via `AskUserQuestion` whether to invoke
`/speccy-ship` now. Ship opens a PR — irreversible —
so this confirmation is always explicit; never auto-ship.

- On confirm: spawn a `speccy-ship` sub-agent.
  Invoke the `Task` tool with `subagent_type: "speccy-ship"`. The
  sub-agent definition at `.claude/agents/speccy-ship.md` carries
  the host-native dispatch metadata.
- On decline: stop the outer loop.

## Stop conditions

- `ship` dispatch declined by the user → stop the outer loop.
- Same `task_id` flips back to `pending` after review for
  **5 rounds in a row** → stop. The implementer is stuck on this
  task. Surface the journal path
  (`.speccy/specs/NNNN-slug/journal/T-NNN.md`) so the user can
  read the blockers and decide whether to decompose
  (`/speccy-amend` + `/speccy-decompose`),
  pick a different model, or intervene by hand. Track per-task
  retry counts in memory across loop iterations; the budget of 5
  is the orchestrator's only per-task retry bound.
- A dispatched sub-agent errors out → stop and surface the error.
- `next_action.kind` is not one of `work`, `review`, `vet`, `ship`,
  `decompose` → stop and report.
- User interrupts → stop on the next loop boundary.

## Status reporting

Before each dispatch, write one short status line so the user can
follow the loop without reading sub-agent transcripts:

```
SPEC-NNNN → work T-003
SPEC-NNNN → review T-003
SPEC-NNNN → work T-003 (retry 2/5 after blocking review)
SPEC-NNNN → vet
SPEC-NNNN → ready to ship — confirm before proceeding?
```

Round numbers, per-persona verdicts, holistic drift findings, and
simplifier candidate details live inside sub-agent contexts and in
the per-task journals at
`.speccy/specs/NNNN-slug/journal/T-NNN.md` (and, for the holistic
gate, in `.speccy/specs/NNNN-slug/journal/VET.md`). Don't duplicate
them in the status line.

## Non-goals

- This skill does not run `speccy verify`, write `REPORT.md`, or
  open a PR. Those belong to `/speccy-ship`, invoked
  after confirmation.
- The drift-fix loop and simplifier polish are **defined** in
  `/speccy-vet` and re-described above only at the
  dispatch-shape level. The skill body of
  `/speccy-vet` remains the source of truth for
  Phase 0–3 semantics — phase grammar bugs and `<gate>` block shape
  bugs get fixed there, not here. The orchestrator inlines the
  fan-out only because sub-agents cannot spawn sub-agents.
- This skill does not pick a different persona fan-out for review,
  retry blocked tasks with a different model, or split tasks
  automatically. Those are judgment calls; surfacing the stuck
  state to the user is the right move.
