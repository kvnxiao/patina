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

**Why fan-outs run inline in this skill's session.**
Sub-agents cannot spawn sub-agents, so the fan-out runs inline in the
top-level session.
The fan-outs in `speccy-review` (five reviewer personas) and
`speccy-vet` (reviewer + implementer + simplifier across up to three
rounds) cannot be delegated to a wrapper sub-agent — it would fail to
spawn its leaves. This orchestrator therefore follows those two skill
bodies **inline in its own session** and spawns the leaf sub-agents
directly; only `speccy-work` (which never fans out) is delegated.
Later sections reference this as "Why fan-outs run inline".

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
                  ├── review → fan out reviewer-* sub-agents
                  │             in parallel from this session
                  │             (each self-appends its <review> via
                  │              `speccy journal append`; orchestrator
                  │              flips state via `speccy task transition`)
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

The orchestrator owns the outer loop, the counters, and the review
consolidation / snapshot management; only the leaf work lives in
spawned sub-agents. All lifecycle writes the orchestrator performs go
through the CLI verbs (`task transition`, `journal append`, `journal
show`); it never edits TASKS.md `state` attributes or journal files
with file-editing tools.

## Context discipline

`speccy-work` is delegated to one sub-agent; `speccy-review` and
`speccy-vet` run inline (per "Why fan-outs run inline" above), with
this orchestrator spawning their leaf sub-agents directly. Each leaf
sub-agent returns one short verdict block — only those final messages,
not the per-persona reasoning, flow back into the orchestrator's
context.

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

**Reconcile policy.** When `speccy next --json` (in either per-spec
or workspace form) returns `next_action.kind == "reconcile"`, iterate
`consistency.drifts[]` and apply the table action per entry, then
re-query before proceeding. See
`.claude/speccy-references/reconcile-policy.md` for the full
policy table and the three properties the dispatch holds by
construction (autonomous / rollback-biased / idempotent).


1. Resolve the spec directory from `speccy next SPEC-NNNN --json`:
   take the `spec_md_path` field and strip the trailing `/SPEC.md`
   to get `<spec-dir>`. If the command exits non-zero, surface the
   stderr line and stop — the SPEC is in a terminal state. If the
   command reports the spec is unknown, stop and report.

2. If that first query returned `next_action.kind == "reconcile"`
   (the CLI's consistency check flags drift from a prior crashed
   session), run the reconcile pass per the **Reconcile policy**
   above, then re-query; enter the dispatch loop only when
   `consistency.status == "ok"`. The pass is autonomous — no
   `AskUserQuestion`, no "press enter to continue" surface, anywhere
   in the dispatch path — and it owns every drift kind in the CLI's
   reconcile enum, including `state_in_progress_orphaned`
   (dirty tree) and `state_in_progress_clean` (clean tree), so the
   orchestrator never scans TASKS.md for `state="in-progress"`
   itself. Each subsequent loop iteration's
   `speccy next SPEC-NNNN --json` (step 1 of the [Loop](#loop)
   section below) re-runs the same check: if a per-task operation
   leaves drift, the next iteration catches it and dispatches the
   reconcile pass before continuing to the normal `work` / `review`
   / `ship` dispatch.

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
   returns; or the reviewer-* personas have all returned, the
   orchestrator has verified completeness via `speccy journal show`
   and flipped state via `speccy task transition`; or the inline vet
   workflow has appended its `<gate>` block via `speccy journal
   append`), re-query `speccy next SPEC-NNNN --json` and loop from
   step 1.

## Work dispatch

**Clean-tree precondition.** Before spawning the `speccy-work` sub-agent, the
orchestrator runs the gate in its own session: resolve the target task from
`next_action.task_id`, read `<spec-dir>/journal/T-NNN.md` (if present) and
classify its shape via the retry-shape rule below, then:

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


For this orchestrator, "stop without dispatching" means halting the outer loop
and surfacing the dirty paths — no `speccy-work` sub-agent is spawned and the
loop does not advance. The retry case is the one named by the
`SPEC-NNNN → work T-NNN (retry 2/5 after blocking review)` status line below.

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

Run the `speccy-review` fan-out **inline in this orchestrator
session** (see "Why fan-outs run inline" above). The fan-out
grammar lives canonically in the `/speccy-review`
skill body; this site carries only a pointer summary so the two
invocation paths stay in sync without duplicating the grammar.

**Review fan-out.** Spawn the five reviewer personas in parallel
(each appends its own `<review>` block via `speccy journal append`
and returns a thin `<verdict>`); verify the round is complete via
`speccy journal show`; flip state via `speccy task transition`
(`completed` on all-pass, `pending` on any blocking verdict); on a
blocking round append one consolidated `<blockers>` block via
`speccy journal append`; on a passing round perform the atomic
commit. See
`.claude/skills/speccy-review/SKILL.md` § "Run the
persona fan-out and consolidation" for the full grammar (the spawn
prompt, the completeness / state-flip / consolidation steps, and
the commit recipe).

After the write settles, increment the per-task retry counter if
the task flipped back to `pending` (this is what feeds the
5-round stop condition below). Then re-query
`speccy next SPEC-NNNN --json`.

## Vet dispatch

Run the `speccy-vet` skill body **inline in this orchestrator
session** (see "Why fan-outs run inline" above). The vet-phases grammar
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
SPEC-NNNN → work T-NNN
SPEC-NNNN → review T-NNN
SPEC-NNNN → work T-NNN (retry 2/5 after blocking review)
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
  bugs get fixed there, not here.
- This skill does not pick a different persona fan-out for review,
  retry blocked tasks with a different model, or split tasks
  automatically. Those are judgment calls; surfacing the stuck
  state to the user is the right move.
