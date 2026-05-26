---
name: speccy-work
description: Implements one Speccy task per invocation. Invoke via /agent speccy-work for the pinned execution path defined in this file's frontmatter.
model: opus[1m]
effort: low
---

# /speccy-work

Implements one task per invocation and exits. With an optional
`[SPEC-NNNN/T-NNN]` selector argument, the session implements that
specific task. Without an argument, the session resolves the next
implementable task via `speccy next --json` (workspace form — used here
because no SPEC is known on the no-selector path) and implements that one.
Task state lives in the `state` attribute on each `<task>` XML element
in TASKS.md.

This is a single-task primitive. It does not iterate over the task
list; composition across tasks belongs to a caller (a human at the
terminal, the `/loop` skill, or a future orchestrator).

## When to use

- With a selector (`/speccy-work SPEC-NNNN/T-003`):
  when the next task to implement is already known — e.g., a retry
  after `/speccy-review` flipped a task back to
  `state="pending"`.
- Without an argument: when picking up wherever `TASKS.md` left
  off. The session implements one task and exits.

`/speccy-decompose` must have written `TASKS.md` and the
spec hash must have been committed before this skill runs.

## What to consider

- Re-read the task's `covers="REQ-NNN"` REQ's `<done-when>`
  (acceptance criteria) and `<behavior>` (test selection) elements
  before writing tests — not the surrounding prose alone.
- The suggested-files hint in the task body is advisory and may be
  stale; verify each path before editing.
- Are you about to add a feature flag, abstraction layer, or
  configurability the SPEC did not ask for? Stop and reconsider —
  scope creep is a blocker for the reviewer round.

## Steps

**Entry precondition (REQ-002):** before any Task dispatch, run `git status --porcelain`; non-empty stdout exits the skill (surface dirty paths on stderr), empty stdout proceeds. If `speccy next --json` then returns `next_action.kind == "reconcile"`, dispatch the reconcile pass per the shared reconcile-policy partial inlined into the calling SKILL.md body rather than the normal implementer flow.

1. Resolve the target task.

   - If a `SPEC-NNNN/T-NNN` selector was passed, that is the target.
   - Otherwise, query the CLI in workspace form (no SPEC selector
     is known on this no-selector invocation path — we must walk
     the active tree to find an implementable task):

     ```bash
     # workspace form: no SPEC-NNNN known yet; scan the active tree.
     speccy next --json
     ```

     Workspace-form exit-code-stop contract: exit code 2 with a
     top-level `reason="no_active_specs"` field in the JSON envelope
     means the workspace has no active specs at all (fresh repo, or
     every spec has shipped or been archived). Exit gracefully and
     surface the reason; do not treat the non-zero exit as a CLI
     error.

     On exit code 0, if the resulting `specs` array has no entry
     with `next_action.kind == "work"`, exit and report that no
     implementable tasks remain. Otherwise, construct the
     disambiguated `<spec>/<task>` form from the JSON's `spec_id`
     and `next_action.task_id` fields (the bare task ID is
     ambiguous across specs — every spec has its own `T-001`).

     Exit-code-stop contract: once SPEC-NNNN is resolved, any
     subsequent per-spec query (`speccy next SPEC-NNNN --json`) that
     exits non-zero means the SPEC has reached a terminal state —
     halt and surface the stderr line. Only parse JSON when exit
     code is 0.

2. Flip the target task's `state` from `pending` to `in-progress`
   by editing TASKS.md.

3. Read the task scenarios to understand what must be implemented:

   ```bash
   speccy check SPEC-NNNN/T-003
   ```

4. Implement the task. Write tests first, then code. Run the
   project's own test command (`cargo test`, `pnpm test`, etc.)
   locally. Use `speccy check SPEC-NNNN/T-NNN` to re-read the
   scenarios being satisfied (it renders them, it does not run
   them).

5. Exit transition. **Hygiene gate (REQ-001):** before flipping `state` from `in-progress` to `in-review`, run the four standard hygiene gates in sequence — `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo +nightly fmt --all --check`, `cargo deny check`. Any non-zero exit refuses the flip and keeps the task at `in-progress`; on all zeros, proceed with the flip and record one line per gate naming its exit code in the appended `<implementer>` block's `Hygiene checks` field. When the implementation is done, flip the task's
   `state="..."` attribute from `in-progress` to `in-review` in
   TASKS.md, then append one `<implementer>` block to the per-task
   journal file at `.speccy/specs/NNNN-slug/journal/T-NNN.md` (a
   sibling of `SPEC.md` and `TASKS.md`). Do NOT inline an
   `<implementer-note>` inside the `<task>` body in TASKS.md — the
   parser rejects that element. The journal file is the canonical
   home for implementer handoff prose.

   File creation. If `journal/T-NNN.md` does not yet exist (round 1,
   first implementer attempt on the task), create it with YAML
   frontmatter declaring exactly three fields, then the
   `<implementer>` block beneath.

   Canonical journal `<implementer>` shape: `references/journal-implementer.md`.

   Canonical evidence file shape: `.claude/speccy-references/evidence.md`.

   `generated_at` is the ISO8601 timestamp at file creation; do not
   rewrite it on later appends. On subsequent rounds, append the new
   `<implementer>` block after the existing journal contents — do
   not modify earlier blocks.

   Required attributes on `<implementer>`. All three are required;
   there are no optional attributes:

   - `date` — full ISO8601 date-time with seconds and timezone
     designator (e.g. `2026-05-21T18:00:00Z` or
     `2026-05-21T18:00:00+00:00`).
   - `model` — the model identity that ran the implementer turn. A
     slash-suffix encodes effort / reasoning-intensity when the host
     harness exposes that knob (e.g.
     `model="claude-opus-4.7[1m]/low"`,
     `model="claude-opus-4.7[1m]/medium"`). Hosts without an effort
     knob omit the suffix entirely (e.g. `model="claude-opus-4.7"`).
     The slash-suffix is a documented convention; the parser
     validates `model` is non-empty but does not enforce suffix
     membership.
   - `round` — a monotonic positive integer starting at 1.
     Increment by exactly 1 on each post-blocker retry attempt. The
     first implementer turn on a task is `round="1"`; if a review
     round blocks and the task flips back to `pending`, the next
     implementer attempt writes `round="2"`, and so on. Do not skip
     values; do not reset.

   Body content. Use the six-field handoff template the implementer
   prompt supplies (`Completed`, `Undone`, `Hygiene checks`,
   `Evidence`, `Discovered issues`, `Procedural compliance`). The
   `Evidence` field must include a CHK-by-CHK roll call labelling
   each CHK under the task's covered REQs as `demonstrated`,
   `hygiene`, or `judgment-only` -- see the canonical reference for
   the format and what each label means.

   Minimal Evidence roll-call shape -- substitute real CHK ids,
   paths, and test names; the canonical reference carries the full
   worked example:

   ```
   - Evidence: paper trail at `.speccy/specs/NNNN-slug/evidence/T-NNN.md`.
     Roll call for CHKs under REQ-NNN:
     - CHK-NNN (one-line CHK description): demonstrated →
       evidence Scenario N covers <what the red/green pair shows>.
     - CHK-NNN (one-line CHK description): hygiene →
       `<test_name>` in `<file:path>` covers it under the project
       test command.
     - CHK-NNN (one-line CHK description): judgment-only →
       no scriptable proof; reviewer-business / reviewer-style
       judges on the diff.
   ```

6. Exit. Do not continue to the next task. If the caller wants
   another task, the caller invokes this skill again.

After exit, the next reasonable step depends on TASKS.md state: if
any task is `state="in-review"`, suggest
`/speccy-review SPEC-NNNN`. If all tasks are
`state="completed"`, suggest `/speccy-vet SPEC-NNNN`.
