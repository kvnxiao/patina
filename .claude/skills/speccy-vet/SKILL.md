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

Sub-agents cannot spawn sub-agents, so the fan-out runs inline in the
top-level session.
This skill's drift-fix loop fans out `vet-reviewer` /
`vet-implementer` / `vet-simplifier` sub-agents across multiple
rounds, so it must run in the top-level session — either a human
invocation
(`/speccy-vet SPEC-NNNN`) or the
`/speccy-orchestrate` outer loop inlining this body at
its `ship` dispatch. The leaf sub-agents each return one short verdict
block as their final message; only those flow back into the running
session.

## What this skill writes and commits

This skill writes to VET.md and lets implementer / simplifier
sub-agents modify the working tree. It **does not commit
anything**. Per Speccy's atomic-landing convention,
`/speccy-ship` is the committer — it bundles all
uncommitted changes from the loop (per-task journal updates,
holistic changes, SPEC.md status flip, REPORT.md) into one commit
at PR time. VET.md, being under
`.speccy/specs/NNNN-slug/journal/`, ships alongside the per-task
journal files.

If a human invokes this skill directly (outside the orchestrator),
they will see uncommitted changes in `git status` after exit and
can decide whether to commit, revert, or continue editing.

## Holistic journal

This skill maintains a single per-SPEC journal file at
the `paths.vet_journal` file resolved in Phase 0 below. The journal is
the **persistent state** of the
holistic loop:

1. **Round-to-round communication** — round N's reviewer reads it
   to walk round N-1's findings and verify the implementer's claims
   against the current diff. Without it, every round re-derives
   from scratch and wastes the round budget.
2. **Audit trail** — after the skill exits, the human (or
   `/speccy-ship`) can read it to see what the loop
   caught and how.

### Single-writer rule

All VET writes go through `speccy journal append`; never edit the
file by hand. The CLI's per-file append lock serializes the parallel
appends: the vet sub-agents append their own `<drift-review>` /
`<holistic-fix>` / `<simplifier-scan>` / `<simplifier-apply>` blocks,
and this skill's session appends the terminal `<gate>` block (it is
the sole author of `<gate>` and, under the orchestrator, of git
commits, but it does not transcribe sub-agent blocks).

### File format

The CLI creates and stamps all of VET.md — frontmatter (`spec`,
`generated_at`), each `## Invocation N — <date>` section, and every
block's `date`/`round` attributes; the skill never writes any of it
by hand. The `round` attribute is **per-invocation**; the CLI resets
it to 1 at the start of each invocation section. `generated_at` is
the file-creation timestamp and is never rewritten.

If a prior invocation crashed mid-loop, its section is left as-is —
the audit trail records what happened. The next append opens a fresh
section.

## Loop


Round budget: **3 rounds per invocation** for drift fixing. Each
round is expensive (full SPEC re-read + diff re-analysis +
implementer pass), so the budget of 3 is intentionally tighter
than the per-task implementer retry budget.

### Phase 0 — bootstrap

Open the spec-scoped context bundle that every vet phase uses. The CLI owns
VET.md's frontmatter and invocation sectioning, so this phase does not
hand-bootstrap the file or write an `## Invocation N` heading — the first
`speccy journal append` for this invocation does both.

1. **Spec context.** Run:

   ```bash
   speccy context SPEC-NNNN --json
   ```

   If the command exits non-zero, surface the stderr line and return
   `fail`. If `non_completed_tasks` is non-empty, return `fail` with
   the listed task ids and states — this is a pre-ship gate, not a
   mid-loop check.

   Keep these fields in the running session:
   - `paths.spec_md`, `paths.tasks_md`, `paths.vet_journal` for
     targeted reads and status messages.
   - `diff_command` for every vet leaf prompt; it is the
     working-tree diff command and includes uncommitted holistic
     changes.
   - `vet_journal.latest_invocation` / `prior_invocations` for
     round context and audit history.

The invocation number `N` for this run is whatever the CLI assigns
on the first append — you do not pick it or write the heading. After
the first block of this invocation lands (the round-1
`<drift-review>` the vet-reviewer appends, or the Phase 3 `<gate>` on
a Phase 0 early-exit), read it back with `speccy journal show
SPEC-NNNN --json` to learn the invocation number for the return
contract's `invocation` field.

### Phase 1 — drift review and fix

Repeat for up to 3 rounds per invocation. The running session owns
the round counter and the working-tree snapshots; sub-agents own the
substantive review and fix work **and append their own blocks** to
VET.md via `speccy journal append` (the vet-reviewer appends
`<drift-review>`, the vet-implementer appends `<holistic-fix>`). The
CLI's per-file lock serializes those appends and stamps `date`,
`round`, and the invocation section — the running session never
transcribes a sub-agent's block and never edits VET.md with
file-editing tools.

**Protect the journal from rollback.** Sub-agents append VET blocks
before the caller decides whether to keep or revert their code
changes. Snapshot code with `git stash push --include-untracked`, but
never `git stash pop`; rollback must leave Speccy's journal directories
untouched. Rationale:
`.claude/skills/speccy-vet/references/vet-journal-safe-rollback.md`.

This is the **journal-safe revert sequence**. Every rollback in
Phases 1 and 2 runs these four commands verbatim:

```bash
# Revert code to the pre-sub-agent snapshot; never touch the journal.
git restore -- ':!.speccy/specs/*/journal/'
git clean -fd -e '.speccy/specs/*/journal/'
git checkout 'stash@{0}' -- ':!.speccy/specs/*/journal/'
git stash drop
```

1. **Spawn the drift reviewer sub-agent.** Prompt:

   > Holistic drift review for `SPEC-NNNN`, round `R`.
   >
   > Run `speccy context SPEC-NNNN --json`. Use
   > `paths.spec_md`, `paths.tasks_md`, `paths.vet_journal`,
   > `vet_journal.latest_invocation`, and `diff_command` from that
   > bundle.
   >
   > Follow the focus, round-2+ scrutiny, and verdict-return
   > contract in your agent file. Append your `<drift-review>` block
   > to VET.md via `speccy journal append` and return a single thin
   > `<verdict>` element as your final message.

   Substitute `SPEC-NNNN` and `R` with the resolved values. The
   sub-agent appends its own
   `<drift-review>` block; do not transcribe it. Read the thin
   `<verdict>` it returns to decide the next step.

   Invoke the `Task` tool with `subagent_type: "vet-reviewer"`.
   The sub-agent definition at `.claude/agents/vet-reviewer.md`
   carries the host-native dispatch metadata (model pin, effort
   level).

2. **If `verdict="pass"`** → the vet-reviewer already appended its
   `<drift-review>` block. Exit the loop and go to Phase 2.

3. **If `verdict="blocking"` and no rounds remain** → the
   `<drift-review>` block is already in VET.md (the reviewer
   appended it), so the trail is complete. Return a `fail` verdict.

4. **Otherwise** (`verdict="blocking"` with budget remaining):

   a. **Snapshot the working tree** before the implementer call,
      so the running session can revert on `stuck`. The snapshot
      captures code state; the reviewer's just-appended
      `<drift-review>` block in VET.md survives because the revert
      scopes out the journal directory (see "Protect the journal
      from rollback" above):

      ```bash
      git stash push --include-untracked -m "speccy-holistic-pre-implementer-<spec>-inv<N>-r<R>"
      git stash apply
      ```

      The `push` snapshots all uncommitted state and clears it to
      HEAD; the `apply` restores the working tree so the implementer
      has the current implementation to work on. The stash stays
      available as the rollback target (see "Protect the journal from
      rollback" above).

   b. **Spawn the drift-implementer sub-agent.** Prompt:

      > Holistic drift fix for `SPEC-NNNN`, round `R`.
      >
      > Run `speccy context SPEC-NNNN --json`. Use its
      > `diff_command` to see the existing implementation; leave
      > your changes uncommitted — the next reviewer reads the same
      > command and will pick them up.
      >
      > The running session will revert your code changes if you
      > return `verdict="stuck"`. Do not manage rollback yourself.
      >
      > Follow the scope, hygiene-gate, and verdict-return
      > contract in your agent file. Append your `<holistic-fix>`
      > block to VET.md via `speccy journal append` and return a
      > single thin `<verdict>` element as your final message.
      >
      > The drift findings to address are in the most recent
      > `<drift-review>` block in VET.md; read them with
      > `speccy journal show SPEC-NNNN --block drift-review
      > --round latest`.

      The sub-agent appends its own `<holistic-fix>` block; do not
      transcribe it. Read the thin `<verdict>` it returns.

      Invoke the `Task` tool with `subagent_type: "vet-implementer"`.
      The sub-agent definition at
      `.claude/agents/vet-implementer.md` carries the host-native
      dispatch metadata.

   c. **Resolve the snapshot based on the implementer's verdict.**
      The `<drift-review>` and `<holistic-fix>` blocks are already
      in VET.md (each sub-agent appended its own), so this step only
      keeps or reverts the **code** changes — it never writes
      blocks:

      - **`addressed` or `blocking`**: keep the implementer's
        edits.

        ```bash
        git stash drop
        ```

        Decrement the round counter and go back to step 1. The next
        reviewer reads the journal the sub-agents appended and
        verifies the implementer's claims against the now-updated
        diff.

      - **`stuck`**: run the journal-safe revert sequence (see
        "Protect the journal from rollback" above). The
        `<drift-review>` and `<holistic-fix>` blocks in VET.md stay
        intact. Return a `fail` verdict.

      - **Sub-agent error or missing/malformed `<verdict>`**: treat
        as `stuck`. Revert the code as above. The vet-implementer
        appends its `<holistic-fix>` block (including the `stuck`
        case) as part of its own run; if it errored before
        appending, append a synthesized `<holistic-fix>` recording
        the sub-agent failure via `speccy journal append SPEC-NNNN
        --block holistic-fix --verdict stuck --model <orchestrator-model>`
        so the trail is complete. Return `fail`.

### Phase 2 — simplifier polish pass

Drift is now `pass`. Run one polish pass for code quality. This
phase does not affect the verdict (a revert still yields
`verdict="pass"`); it only sets the `simplifier="..."` field on
the return block.

1. **Spawn the simplifier scan sub-agent.** Prompt:

   > Identify simplification candidates in the diff for
   > `SPEC-NNNN`. Run `speccy context SPEC-NNNN --json` and use
   > its `diff_command` to see all changes (working tree included).
   > **Report only — do NOT modify files.** Skip anything that
   > would change behavior, weaken
   > invariants, or trip project conventions in `AGENTS.md` and
   > project-local rule files.
   >
   > Append your `<simplifier-scan>` block to VET.md via `speccy
   > journal append` and return a single thin `<verdict>` element as
   > your final message.

   Invoke the `Task` tool with
   `subagent_type: "vet-simplifier"`.

   The scan sub-agent appends its own `<simplifier-scan>` block; do
   not transcribe it. The scan makes no code modifications, so it is
   part of the audit trail whether or not an apply step follows. Read
   the thin `<verdict>` it returns.

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

      The first command snapshots uncommitted state and clears it to
      HEAD; the second restores the working tree so the simplifier
      sees the drift-fix changes. The stash remains as the rollback
      target (see "Protect the journal from rollback" above).

   b. **Spawn the simplifier apply sub-agent.** Prompt:

      > Apply the simplification candidates listed below.
      > Preserve all functionality. After applying, run the
      > project's hygiene gates as defined in its `AGENTS.md`.
      >
      > **If any hygiene step fails, do NOT attempt to revert
      > yourself.** Return `verdict="blocking"` with a one-line
      > description of what failed; the caller owns the rollback.
      >
      > The candidates to apply are in the most recent
      > `<simplifier-scan>` block in VET.md; read them with
      > `speccy journal show SPEC-NNNN --block simplifier-scan`.
      >
      > Append your `<simplifier-apply>` block to VET.md via `speccy
      > journal append` and return a single thin `<verdict>` element
      > as your final message.

      Invoke the `Task` tool with
      `subagent_type: "vet-simplifier"`.

      The apply sub-agent appends its own `<simplifier-apply>` block;
      do not transcribe it. Read the thin `<verdict>` it returns.

   c. Resolve the snapshot based on the verdict. The
      `<simplifier-apply>` block is already in VET.md (the sub-agent
      appended it), so this step only keeps or reverts **code**:

      - **`applied`** (hygiene green): `git stash drop` — discard
        the snapshot, keep the simplifications. Record
        `simplifier="applied"` for the return block.

      - **`blocking`** (hygiene failed), sub-agent error, or
        missing/malformed verdict: run the journal-safe revert
        sequence (see "Protect the journal from rollback" above). The
        `<simplifier-apply>` block in VET.md stays intact. If the
        sub-agent errored before appending, append a synthesized
        `<simplifier-apply>` recording the failure via `speccy
        journal append SPEC-NNNN --block simplifier-apply
        --verdict blocking` so the trail is complete. Record
        `simplifier="reverted"`.

### Phase 3 — append the `<gate>` block via the CLI

**Every** exit path — Phase 0 integrity failures, Phase 1
round-budget exhaustion, Phase 1 `stuck` reverts, Phase 2
completion (pass or revert), and the success path — appends
exactly one `<gate>` block to `paths.vet_journal` via
`speccy journal append --block gate`, **before** surfacing the
verdict to the caller. This is the running session's own write (the
gate is not authored by a sub-agent), and it goes through the CLI
verb — never by editing VET.md with file-editing tools.

```bash
speccy journal append SPEC-NNNN --block gate --verdict <passed|failed> <<'EOF'
<one-line human-readable summary of the invocation outcome>
EOF
```

The CLI owns everything environment-derivable and the gate's
placement:

- It computes `tasks_hash` as the lowercase hex SHA-256 of the
  sibling TASKS.md read at append time — **do not compute or supply
  a hash**, and there is no `sha256sum` / `Get-FileHash` step.
- It manages invocation sectioning: the gate lands as the **last**
  element of the current open invocation section, after any
  `<drift-review>`, `<holistic-fix>`, `<simplifier-scan>`, and
  `<simplifier-apply>` blocks the sub-agents appended. If no section
  is open yet (a Phase 0 early exit fired before any block landed,
  so VET.md is absent or its last section is gate-terminated), the
  same append creates the file with frontmatter and opens a fresh
  `## Invocation N` section before writing the gate. **Do not
  hand-bootstrap frontmatter or write an `## Invocation N`
  heading** — the append does both. The on-disk gate record exists
  regardless of where the early exit fired.

You supply only `--verdict` and the one-line body:

- `--verdict` — `passed` when the surfaced verdict will be
  `verdict="pass"`; `failed` when it will be `verdict="fail"`
  (including every Phase 0 early-exit path).
- body — a single line summarising what happened (examples: `"Drift
  cleared on round 2; simplifier applied; clean."`, `"Phase 0
  integrity check failed: task T-NNN not completed."`, `"Drift round
  budget exhausted at round 3 without a pass."`).

Validation runs before any write; a malformed body or an attempt to
add a second gate to a gate-terminated section leaves VET.md
byte-identical.

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

A human can run `/speccy-vet SPEC-NNNN` by hand (each
direct invocation gets its own section in VET.md). The skill behaves
identically whether invoked by the orchestrator or by a human — only
the caller of the verdict differs.

## Next step after exit

When the gate returns `verdict="pass"` and REPORT.md is absent for
the SPEC, suggest `/speccy-ship SPEC-NNNN` (rendered as
`/speccy-ship SPEC-NNNN` in the shared body) as the
next reasonable step — the SPEC is freshly vetted and ready to be
committed and PR'd. When the gate returns `verdict="fail"`, the
final block's one-line suggested next step takes precedence over
ship.
