---
name: speccy-vet
description: 'Run a holistic SPEC-vs-implementation review with an autonomous drift-fix retry loop and a simplifier polish pass, for one Speccy SPEC at the pre-ship boundary. Use when the user says "holistic gate SPEC-NNNN", "speccy-vet SPEC-NNNN", "check for drift before shipping", "run the final defense on SPEC-NNNN", or when speccy-orchestrate reaches the ship boundary and delegates here. Fans out a SPEC drift reviewer and (after drift clears) a simplifier candidate scan, dispatches implementer sub-agents to fix any drift, and returns a single verdict block to its caller. Requires: a SPEC-NNNN whose tasks are all state="completed". Do NOT trigger for per-task review — prefer speccy-review for single-task review.'
---

# /speccy-vet

Final defense mechanism against SPEC drift at the pre-ship boundary.

The per-task implementer/reviewer cycle keeps each task honest
against its own scenarios. It does not catch drift that only appears
at the whole-SPEC level: requirements satisfied by no task, behavior
the diff introduces that the SPEC never authorized, or per-task
code that doesn't add up to the SPEC as a unit. This skill is the
final check before the PR opens.

This skill is **autonomous up to the ship point**. It fans out
review and implementer sub-agents, loops on drift, applies a
simplifier polish pass, and returns a single short verdict to its
caller. The caller (typically `/speccy-orchestrate`
at the `ship` boundary, but a human can invoke this directly) is
the one that gates the actual PR opening.

## When to use

- Every task in `SPEC-NNNN` is at `state="completed"` and the next
  step is to confirm the diff matches the SPEC as a unit before
  invoking `/speccy-ship`.
- A SPEC was amended mid-implementation and the human wants a final
  defense pass against drift before opening the PR.
- The `/speccy-orchestrate` outer loop reaches the
  `ship` boundary and delegates here.

Do not invoke this skill for per-task review (use
`/speccy-review`) or while tasks remain at
`pending` / `in-progress` / `in-review` (the skill returns `fail`
immediately if any task is non-completed — this is a pre-ship gate,
not a mid-loop check).

## Argument

```
/speccy-vet SPEC-NNNN
```

The `SPEC-NNNN` argument is required. The SPEC's tasks must all be
at `state="completed"` — this is a pre-ship gate, not a mid-loop
check. If any task is not completed, return a `fail` verdict
immediately with that as the reason.

## Why this skill runs in a top-level session

The drift-fix loop fans out additional sub-agents over multiple
rounds (`vet-reviewer`, `vet-implementer`, `vet-simplifier`).
Sub-agents cannot spawn sub-agents, so this skill must run in a
context that **is** the top-level session — either:

- A human invocation (`/speccy-vet SPEC-NNNN`), where
  the host CLI session itself runs the skill body, or
- The `/speccy-orchestrate` outer loop, which
  inlines this skill body into its own session at the `ship`
  dispatch (it cannot delegate to a wrapper sub-agent that would
  then try to spawn the leaves).

In both cases the leaf sub-agents (reviewer / implementer /
simplifier) return one short verdict block as their final message;
only those final messages flow back into the running session.

## What this skill writes and commits

This skill writes to VET.md and lets implementer / simplifier
sub-agents modify the working tree. It **does not commit
anything**. Per Speccy's atomic-landing convention,
`/speccy-ship` is the committer — it bundles all
uncommitted changes from the loop (per-task journal updates,
holistic changes, SPEC.md status flip, REPORT.md) into one commit
at PR time. VET.md, being under
`.speccy/specs/<spec-dir>/journal/`, ships alongside the per-task
journal files.

If a human invokes this skill directly (outside the orchestrator),
they will see uncommitted changes in `git status` after exit and
can decide whether to commit, revert, or continue editing.

## Holistic journal

This skill maintains a single per-SPEC journal file at
`<spec-dir>/journal/VET.md` (`<spec-dir>` is resolved in
Phase 0 below). The journal is the **persistent state** of the
holistic loop:

1. **Round-to-round communication** — round N's reviewer reads it
   to walk round N-1's findings and verify the implementer's claims
   against the current diff. Without it, every round re-derives
   from scratch and wastes the round budget.
2. **Audit trail** — after the skill exits, the human (or
   `/speccy-ship`) can read it to see what the loop
   caught and how.

### Single-writer rule

This skill's session is the **only writer** to VET.md.
Reviewer and implementer sub-agents never write to it — they return
verdict blocks via their final message; this skill transcribes
them. Sub-agents writing in parallel would race; single-writer
prevents that.

### File format

YAML frontmatter created at first-ever invocation, then one
`## Invocation N — <date>` section per skill invocation, with
round blocks appended within the current section:

```markdown
---
spec: SPEC-NNNN
generated_at: 2026-05-21T22:00:00Z
---

## Invocation 1 — 2026-05-21T22:00:00Z

<drift-review verdict="blocking" round="1" date="..." model="...">
...
</drift-review>

<holistic-fix verdict="addressed" round="1" date="..." model="...">
...
</holistic-fix>

<drift-review verdict="pass" round="2" date="..." model="...">
...
</drift-review>

## Invocation 2 — 2026-05-22T...

<drift-review verdict="..." round="1" ...>
...
</drift-review>
```

The `round` attribute is **per-invocation**. Round numbers reset to
1 at the start of each invocation. `generated_at` in the frontmatter
is the file-creation timestamp and is never rewritten.

If a prior invocation crashed mid-loop, its section is left as-is —
the audit trail records what happened. The new invocation starts
clean with its own section.

## Loop

Shared with the `/speccy-orchestrate` ship dispatch
— both this skill body and that dispatch step include the same
partial below so Phase 0 / 1 / 2 / 3 have a single source of truth.


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


## Return contract

Return exactly one block as the final message of this skill's
session:

```
<orchestrator-verdict verdict="pass|fail" invocation="N" rounds="R" simplifier="clean|applied|reverted|skipped">
<one-line summary of the holistic outcome>
[if fail: one-line suggested next step (amend SPEC vs new task vs manual fix)]
</orchestrator-verdict>
```

Field reference:

- `verdict` — `pass` if drift cleared within budget; `fail`
  otherwise.
- `invocation` — the invocation number for this run, matching the
  VET.md section header.
- `rounds` — how many drift-fix rounds were consumed (0 to 3).
- `simplifier` — `clean` if no candidates were found; `applied` if
  candidates applied + hygiene green; `reverted` if applied but
  rolled back due to hygiene failure; `skipped` if Phase 2 didn't
  run (drift never cleared).

The caller consumes only this final block; the inner blocks live
in VET.md and the sub-agent contexts that produced them.

## Stop conditions

- Drift round budget exhausted (3 rounds in this invocation)
  without a `pass` from the drift reviewer → return `fail`.
- Drift-fix implementer returns `verdict="stuck"` → return `fail`
  immediately.
- Any sub-agent errors or returns a malformed verdict → return
  `fail` with the error in the one-line summary.
- Phase 0 finds the spec is unknown or has incomplete tasks →
  return `fail` immediately.

## When to invoke directly

A human can run `/speccy-vet SPEC-NNNN`
by hand:

- Before ever invoking `/speccy-ship`, as a
  final-defense check on a SPEC implemented manually.
- After amending a SPEC and re-running
  `/speccy-work` on the affected tasks, to confirm
  the patched implementation still adheres to the SPEC
  holistically. (Each direct invocation gets its own section in
  VET.md.)

The skill behaves identically whether invoked by the orchestrator
or by a human — only the caller of the verdict differs.

## Next step after exit

When the gate returns `verdict="pass"` and REPORT.md is absent for
the SPEC, suggest `/speccy-ship SPEC-NNNN` (rendered as
`/speccy-ship SPEC-NNNN` in the shared body) as the
next reasonable step — the SPEC is freshly vetted and ready to be
committed and PR'd. When the gate returns `verdict="fail"`, the
final block's one-line suggested next step takes precedence over
ship.
