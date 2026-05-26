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
                  └── ship   → run the speccy-vet skill body inline
                                in this session, spawning
                                vet-reviewer / vet-implementer /
                                vet-simplifier leaf sub-agents directly
                                (drift-fix loop bounded to 3 rounds,
                                 then one simplifier polish pass)
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
reconcile pass per the shared partial inlined immediately below
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

<!-- Shared partial: reconcile-policy. Source: .claude/speccy-references/reconcile-policy.md -->
# Reconcile policy: shared partial

This file is the single source of truth for Speccy's reconcile
dispatch policy. It is inlined verbatim into three skill body files
via the existing shared-partial convention:

- `.claude/skills/speccy-orchestrate/SKILL.md`
- `.claude/skills/speccy-work/SKILL.md`
- `.claude/skills/speccy-review/SKILL.md`

Each inlined site is bounded by marker comments naming this partial:

```
<!-- Shared partial: reconcile-policy. Source: .claude/speccy-references/reconcile-policy.md -->
<partial content>
<!-- End shared partial: reconcile-policy. -->
```

When this file changes, all three inlined copies must be re-synced.

---

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

| `kind` | `severity` | Action |
|---|---|---|
| `commit_without_state` | `auto_fixable` | Edit TASKS.md: flip the task's `state` attribute to `completed` (deterministic write). |
| `state_completed_no_commit` (dirty tree, `details.working_tree_dirty == true`) | `blocking` | Run `git add -A` followed by `git commit` using the REQ-004 message format (title `[SPEC-NNNN/T-NNN]: <task title>`; body extracted from the latest `<implementer>` block's `Completed` field in `journal/T-NNN.md`; `Co-Authored-By` trailer per host). |
| `state_completed_no_commit` (clean tree, `details.working_tree_dirty == false`) | `blocking` | Edit TASKS.md: roll the task's `state` back to `in-review`. Journal file is preserved intact as evidence for the next reviewer round. |
| `state_in_progress_orphaned` | `blocking` | Run `git restore .` and `git clean -fd` to discard the partial implementer work, then edit TASKS.md to flip the task's `state` to `pending`. The orchestrator's per-task retry budget will redo the work. |
| `state_in_progress_clean` (`details.working_tree_dirty == false`) | `blocking` | Edit TASKS.md: roll the task's `state` back to `pending`. No git mutation — the tree is already clean, so there is no partial work to discard. The orchestrator's per-task retry budget will redo the work. |
| `journal_xml_malformed` | `blocking` | Truncate the journal file at `details.journal_path` to `details.last_well_formed_byte_offset` bytes. Reset the corresponding TASKS.md `state` to whatever the truncated journal implies (if the last well-formed element is `<implementer>`, state goes to `in-review`; if a closing `<review>` block survived and all four personas passed, the per-task journal already reflects a passing round and state may flip to `completed` via the standard commit step). |

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
   on a clean tree produces no commit; re-running a TASKS.md state
   flip when the state is already at the target value is a no-op;
   truncating a journal at its current length is a no-op.
   Successive session crashes during reconciliation converge to the
   same eventual state.

## Extending the enum

Adding a new drift kind to the consistency enum requires changes in
exactly two places:

1. **CLI detection.** Add the new variant to the `DriftKind` enum in
   the Rust source (`speccy-core/src/consistency.rs` or its
   equivalent under the current module layout) and implement the
   deterministic detection logic that emits drift entries of the
   new kind in `speccy next --json`'s `consistency.drifts[]` array.
   Detection must be read-only: no mutating git commands, no
   side-effecting writes to TASKS.md or the journal.

2. **Policy table.** Add a row to the policy table in this partial
   naming the new kind, its `severity`, and the deterministic
   action the calling skill takes when it encounters the kind in
   `consistency.drifts[]`. Then re-sync all three inlined copies in
   the skill body files listed at the top of this partial.

No other site needs to change. The CLI knows what it *detected*;
this partial knows what to *do*. Future hosts (Codex, others) that
consume the consistency block reuse this partial unchanged.

<!-- End shared partial: reconcile-policy. -->

1. Resolve the spec directory from `speccy next SPEC-NNNN --json`:
   take the `spec_md_path` field and strip the trailing `/SPEC.md`
   to get `<spec-dir>`. If the command exits non-zero, surface the
   stderr line and stop — the SPEC is in a terminal state. If the
   command reports the spec is unknown, stop and report.

2. If `next_action.kind == "reconcile"`, run the reconcile pass per
   the shared partial above, then re-query and continue. Otherwise
   proceed directly to the dispatch loop.

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

**Clean-tree precondition (REQ-002).** Before spawning the
`speccy-work` sub-agent, run `git status --porcelain` in the
orchestrator's running session. If stdout is non-empty, **halt the
outer loop** and surface the dirty paths to the user — no
`speccy-work` sub-agent is spawned, and the loop does not advance.
Empty stdout permits the dispatch below. This check fires in the
orchestrator's session (not delegated to the sub-agent) because the
outer loop's invariant — every dispatched implementer turn begins on
a clean tree — must be enforced by the loop owner. The `speccy-work`
skill body re-runs the same check defensively at its own entry as a
second-level guard.

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


After the write settles, increment the per-task retry counter if
the task flipped back to `pending` (this is what feeds the
5-round stop condition below). Then re-query
`speccy next SPEC-NNNN --json`.

The `speccy-review` skill remains independently invocable as
`/speccy-review`; this orchestrator's review
dispatch shares the same fan-out contract via the partial above so
behaviour stays in sync across invocation paths.

## Ship dispatch

Run the `speccy-vet` skill body **inline in this orchestrator
session** (do NOT wrap it in a single general-purpose sub-agent —
that wrapper would need to spawn the vet-reviewer /
vet-implementer / vet-simplifier leaves across up to three rounds,
and sub-agents cannot spawn sub-agents). The shared partial below
is the single source of truth, included by both this
orchestrator's ship dispatch and the `/speccy-vet`
skill body.


Round budget: **3 rounds per invocation** for drift fixing. Each
round is expensive (full SPEC re-read + diff re-analysis +
implementer pass), so the budget of 3 is intentionally tighter
than the per-task implementer retry budget.

### Phase 0 — bootstrap

Resolve the three values that sub-agent prompts need, then open a
new invocation section in VET.md.

1. **Spec directory.** Run:

   ```bash
   speccy next SPEC-NNNN --json
   ```

   The `spec_md_path` field (e.g.,
   `.speccy/specs/NNNN-slug/SPEC.md`) gives the absolute path to
   `SPEC.md`; strip the trailing `/SPEC.md` to get `<spec-dir>`
   (e.g., `.speccy/specs/NNNN-slug/`). If the command exits
   non-zero, the SPEC has reached a terminal state — surface the
   stderr line and return `fail`. Only parse the JSON envelope
   when exit code is 0. If the spec is unknown, return `fail`
   immediately.

   Also verify every task in this spec is at `state="completed"`
   (read `<spec-dir>/TASKS.md`). If any task is `pending`,
   `in-progress`, or `in-review`, return `fail` — this is a
   pre-ship gate, not a mid-loop check.

2. **Diff baseline ref.** Run:

   ```bash
   git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's@^refs/remotes/origin/@@'
   ```

   Use the output as `<base-ref>`. If empty (no remote, detached
   HEAD), fall back to `main`. Sub-agent prompts will pass this in
   for `git diff <base-ref>` — that command compares the **working
   tree** against the ref, including uncommitted changes, which is
   essential because the drift-implementer leaves changes
   uncommitted between rounds.

3. **Journal bootstrap and new invocation section.** The journal
   is at `<spec-dir>/journal/VET.md`.

   - If the file does not exist, create it with the YAML
     frontmatter (`spec`, `generated_at`).
   - Scan the file for `^## Invocation (\d+)` headers, take the
     max, and add 1 to get the new invocation number `N`. If no
     prior headers exist, `N = 1`.
   - Append a new section header:

     ```markdown

     ## Invocation N — <ISO8601 timestamp>
     ```

     (Blank line above the heading for markdown readability.) Do
     not modify prior sections, even if a prior invocation
     crashed mid-loop.

### Phase 1 — drift review and fix

Repeat for up to 3 rounds per invocation. The running session owns
the round counter, the working-tree snapshots, and the VET.md
writes; sub-agents own the substantive review and fix work.

**Defer-write pattern.** Hold returned verdict blocks in memory
across each round and write to VET.md only **after** the
snapshot-keep-vs-revert decision. Writing earlier would put VET.md
changes inside the snapshot, and a stuck-revert would erase the
audit trail. The journal is the durable record of *what the loop
did*; it must survive any rollback the loop performs.

1. **Spawn the drift reviewer sub-agent.** Prompt:

   > Holistic drift review for `SPEC-NNNN`, invocation `N`, round
   > `R`.
   >
   > Resolved paths:
   > - Spec directory: `<spec-dir>` (use this for `SPEC.md`,
   >   `TASKS.md`, mission file if any, and the journal at
   >   `<spec-dir>/journal/VET.md`).
   > - Diff baseline: `<base-ref>` (run `git diff <base-ref>` —
   >   that captures the working tree including uncommitted
   >   changes, which the implementer leaves between rounds).
   >
   > Follow the focus, round-2+ scrutiny, and verdict-return
   > contract in your agent file. Return a single `<drift-review>`
   > block as your final message.

   Substitute `SPEC-NNNN`, `N`, `R`, `<spec-dir>`, and `<base-ref>`
   with the resolved values. Hold the returned `<drift-review>`
   block in memory; do not write to VET.md yet.

   Invoke the `Task` tool with `subagent_type: "vet-reviewer"`.
   The sub-agent definition at `.claude/agents/vet-reviewer.md`
   carries the host-native dispatch metadata (model pin, effort
   level).

2. **If `verdict="pass"`** → append the held `<drift-review>`
   block to `<spec-dir>/journal/VET.md` under the current
   invocation section. Exit the loop and go to Phase 2.

3. **If `verdict="blocking"` and no rounds remain** → append the
   held `<drift-review>` block to VET.md (so the trail is
   complete) and return a `fail` verdict.

4. **Otherwise** (`verdict="blocking"` with budget remaining):

   a. **Snapshot the working tree** before the implementer call,
      so the running session can revert on `stuck` without losing
      the VET.md writes:

      ```bash
      git stash push --include-untracked -m "speccy-holistic-pre-implementer-<spec>-inv<N>-r<R>"
      git stash apply
      ```

      The `push` saves all uncommitted state and clears the working
      tree to HEAD; the `apply` restores the working tree so the
      implementer has the current implementation to work on. The
      stash stays available as the rollback target.

   b. **Spawn the drift-implementer sub-agent.** Prompt:

      > Holistic drift fix for `SPEC-NNNN`, invocation `N`, round
      > `R`.
      >
      > Resolved paths:
      > - Spec directory: `<spec-dir>`.
      > - Diff baseline: `<base-ref>` (use `git diff <base-ref>`
      >   to see the existing implementation; leave your changes
      >   uncommitted — the next reviewer reads the same command
      >   and will pick them up).
      >
      > The running session will revert your changes if you
      > return `verdict="stuck"`. Do not manage rollback yourself.
      >
      > Follow the scope, hygiene-gate, and verdict-return
      > contract in your agent file. Return a single
      > `<holistic-fix>` block as your final message.
      >
      > Drift findings (the held `<drift-review>` block):
      >
      > [paste the held `<drift-review>` block verbatim]

      Hold the returned `<holistic-fix>` block in memory.

      Invoke the `Task` tool with `subagent_type: "vet-implementer"`.
      The sub-agent definition at
      `.claude/agents/vet-implementer.md` carries the host-native
      dispatch metadata.

   c. **Resolve the snapshot based on the implementer's verdict**:

      - **`addressed` or `blocking`**: keep the implementer's
        edits.

        ```bash
        git stash drop
        ```

        Then append **both** the held `<drift-review>` block and
        the held `<holistic-fix>` block to VET.md under the
        current invocation section (drift-review first, then
        fix). Decrement the round counter and go back to step 1.
        The next reviewer reads the journal you just appended and
        verifies the implementer's claims against the now-updated
        diff.

      - **`stuck`**: revert the implementer's edits, then preserve
        the audit trail:

        ```bash
        git restore .
        git clean -fd
        git stash pop
        ```

        `git restore .` undoes implementer edits to tracked
        files; `git clean -fd` removes any new files the
        implementer added; `git stash pop` restores the
        pre-implementer snapshot. Now append both held blocks to
        VET.md under the current invocation section — the write
        happens **after** the revert, so it survives. Return a
        `fail` verdict.

      - **Sub-agent error or missing/malformed `<holistic-fix>`**:
        treat as `stuck`. Revert as above. Append the held
        `<drift-review>` block and a synthesized
        `<holistic-fix verdict="stuck">` block describing the
        sub-agent failure. Return `fail`.

### Phase 2 — simplifier polish pass

Drift is now `pass`. Run one polish pass for code quality. This
phase does not affect the verdict (a revert still yields
`verdict="pass"`); it only sets the `simplifier="..."` field on
the return block.

1. **Spawn the simplifier scan sub-agent.** Prompt:

   > Identify simplification candidates in the diff for
   > `SPEC-NNNN`. Run `git diff <base-ref>` to see all changes
   > (working tree included). **Report only — do NOT modify
   > files.** Skip anything that would change behavior, weaken
   > invariants, or trip project conventions in `AGENTS.md` and
   > project-local rule files.
   >
   > Return your verdict as your final message:
   >
   > ```
   > <simplifier-scan verdict="clean|candidates">
   > <one-line summary>
   > [optional bullets, each with file:line + proposed change]
   > </simplifier-scan>
   > ```

   Invoke the `Task` tool with
   `subagent_type: "vet-simplifier"`.

   The scan makes no modifications, so no defer-write is needed
   — **append the returned `<simplifier-scan>` block to VET.md
   immediately** (under the current invocation section). The
   block is part of the audit trail whether or not an apply step
   follows.

2. If `verdict="clean"` → record `simplifier="clean"` for the
   return block and go to Phase 3.

3. If `verdict="candidates"`:

   a. **Snapshot the working tree** before the apply, so the
      running session owns the rollback. The simplifier
      sub-agent cannot reliably roll back itself — `git
      checkout` doesn't undo new files and `git clean -fd` is
      dangerous if scoped wrong. Owning the rollback here bounds
      the blast radius.

      ```bash
      git stash push --include-untracked -m "speccy-holistic-pre-simplifier-<spec>-<invocation>"
      git stash apply
      ```

      The first command saves a snapshot of all uncommitted state
      (tracked + untracked) and clears the working tree to HEAD;
      the second restores the working tree so the simplifier sees
      the drift-fix changes. The stash remains as the rollback
      target.

   b. **Spawn the simplifier apply sub-agent.** Prompt:

      > Apply the simplification candidates listed below.
      > Preserve all functionality. After applying, run the
      > standard hygiene suite per `AGENTS.md` (the project's
      > four standard hygiene gates).
      >
      > **If any hygiene step fails, do NOT attempt to revert
      > yourself.** Return `verdict="blocking"` with a one-line
      > description of what failed; the caller owns the rollback.
      >
      > Candidates to apply:
      >
      > [paste the `<simplifier-scan>` block from step 1 verbatim]
      >
      > Return your final message:
      >
      > ```
      > <simplifier-apply verdict="applied|blocking">
      > <one-line summary>
      > </simplifier-apply>
      > ```

      Invoke the `Task` tool with
      `subagent_type: "vet-simplifier"`.

      Hold the returned `<simplifier-apply>` block in memory; do
      not write to VET.md yet (same defer-write pattern as Phase 1
      — write after the revert decision so the audit trail
      survives any rollback).

   c. Resolve the snapshot based on the verdict, then transcribe:

      - **`applied`** (hygiene green): `git stash drop` — discard
        the snapshot, keep the simplifications. Append the held
        `<simplifier-apply>` block to VET.md under the current
        invocation section. Record `simplifier="applied"` for the
        return block.

      - **`blocking`** (hygiene failed), sub-agent error, or
        missing/malformed verdict: roll back.

        ```bash
        git restore .
        git clean -fd
        git stash pop
        ```

        `git restore .` wipes simplifier changes from tracked
        files. `git clean -fd` removes untracked files (including
        any new files the simplifier created). `git stash pop`
        restores the pre-simplifier snapshot. Then append the
        held `<simplifier-apply>` block to VET.md under the
        current invocation section (synthesize a placeholder if
        the sub-agent returned nothing parseable). Record
        `simplifier="reverted"`.

### Phase 3 — write `<gate>` block

**Every** exit path — Phase 0 integrity failures, Phase 1
round-budget exhaustion, Phase 1 `stuck` reverts, Phase 2
completion (pass or revert), and the success path — appends
exactly one `<gate>` block to `<spec-dir>/journal/VET.md` under
the current `## Invocation N` section, **before** surfacing the
verdict to the caller.

If Phase 0 failed before opening the invocation section (for
example, the spec is unknown so `<spec-dir>` was never resolved
or the journal file does not exist yet), bootstrap the file and
section per Phase 0 step 3, then append the `<gate>` block. The
on-disk gate record exists regardless of where the early exit
fired.

The `<gate>` block is appended **after** any `<drift-review>`,
`<holistic-fix>`, `<simplifier-scan>`, and `<simplifier-apply>`
blocks already written for the current invocation. It is the
**last** element in the section.

Block shape:

```
<gate verdict="passed|failed" tasks_hash="<lowercase-hex-sha256>" date="<ISO8601>">
<one-line human-readable summary of the invocation outcome>
</gate>
```

Attribute rules:

- `verdict` — `passed` when the surfaced verdict will be
  `verdict="pass"`; `failed` when it will be `verdict="fail"`
  (including every Phase 0 early-exit path).
- `tasks_hash` — lowercase hex SHA-256 of the byte contents of
  `<spec-dir>/TASKS.md` read **immediately before** appending this
  block. Compute via:

  ```bash
  sha256sum <spec-dir>/TASKS.md | awk '{print $1}'
  ```

  PowerShell equivalent on Windows:

  ```powershell
  (Get-FileHash -Algorithm SHA256 <spec-dir>/TASKS.md).Hash.ToLower()
  ```

- `date` — ISO8601 datetime with seconds and timezone designator,
  e.g. `2026-05-22T14:30:00Z`.

The block body is a single line summarising what happened
(examples: `"Drift cleared on round 2; simplifier applied;
clean."`, `"Phase 0 integrity check failed: task T-003 not
completed."`, `"Drift round budget exhausted at round 3 without a
pass."`).

`speccy next` reads the most recent `<gate>` block's `verdict`
and `tasks_hash` to decide whether the SPEC is freshly vetted; a
`passed` gate whose `tasks_hash` no longer matches the on-disk
TASKS.md forces a re-vet. That is the contract this block exists
to satisfy.


After the vet workflow appends its `<gate>` block and surfaces a
verdict to this orchestrator session, react as follows:

- `verdict="pass"` → write a one-line summary plus the round and
  simplifier counters, then **ask the user** whether to invoke
  `/speccy-ship`. Only after explicit confirmation,
  spawn a `speccy-ship` sub-agent. Ship opens a PR; never
  auto-ship.
- `verdict="fail"` → surface the drift summary and one-line
  suggested next step. Stop the outer loop. The user decides how
  to address it (`/speccy-amend`, manual edits,
  etc.).

## Stop conditions

- `verdict="pass"` from the holistic gate → ask the user before
  invoking ship.
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
- `next_action.kind` is not one of `work`, `review`, `ship`,
  `decompose` → stop and report.
- User interrupts → stop on the next loop boundary.

## Status reporting

Before each dispatch, write one short status line so the user can
follow the loop without reading sub-agent transcripts:

```
SPEC-NNNN → work T-003
SPEC-NNNN → review T-003
SPEC-NNNN → work T-003 (retry 2/5 after blocking review)
SPEC-NNNN → holistic gate
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
