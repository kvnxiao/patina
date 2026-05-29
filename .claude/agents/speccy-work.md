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

The agent automatically detects retry shape from the per-task
journal at `<spec-dir>/journal/T-NNN.md` and switches modes
accordingly — the caller does not pass a flag. A first-attempt
task (no journal, or no `<blockers>` matching the highest
`<implementer>` round) runs today's recipe unchanged. A retry-shape
task (the latest `<blockers>` round equals the highest
`<implementer>` round) amends the existing WIP in the working tree
in place and appends `<implementer round="N+1">`.

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

**Entry precondition (SPEC-0045 REQ-002, extended by SPEC-0047 REQ-002 / REQ-003):** before any Task dispatch, (i) resolve the target task per step 1, (ii) read `<spec-dir>/journal/T-NNN.md` (if it exists) and apply the retry-shape rule summarized at step 2 (canonical statement at `.claude/speccy-references/retry-shape.md`), (iii) run `git status --porcelain`. **First-attempt shape** with non-empty stdout exits the skill (surface dirty paths on stderr); empty stdout proceeds with the first-attempt branch (today's SPEC-0045/REQ-002 behaviour). **Retry shape** proceeds with the retry branch regardless of stdout — the dirty paths are the prior pass's WIP that the retry implementer amends in place; no dirty-paths surface is written. If `speccy next --json` then returns `next_action.kind == "reconcile"`, dispatch the reconcile pass per the **Reconcile policy** summary below (canonical policy at `.claude/speccy-references/reconcile-policy.md`) rather than the normal implementer flow.

**Reconcile policy.** When `speccy next --json` returns `next_action.kind == "reconcile"`, iterate `consistency.drifts[]` and apply the table action per entry, then re-query before proceeding. See `.claude/speccy-references/reconcile-policy.md` for the full policy table.

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

2. Read `<spec-dir>/journal/T-NNN.md` (if it exists) and apply the
   REQ-001 retry-shape rule summarized immediately below. The rule
   reads only the journal file — no git, no `speccy next`, no
   other CLI subcommand. Compute the result (first-attempt shape
   or retry shape); the rest of the recipe branches on this
   result.

   **Retry shape.** A task is in retry shape iff its journal
   contains both an `<implementer>` element and a `<blockers>`
   element whose `round` attribute matches the highest implementer
   round. Otherwise it's first-attempt shape — the strict
   clean-tree gate applies. See
   `.claude/speccy-references/retry-shape.md` for the full rule
   statement, read-only scope, worked examples, and the
   "implementer awaiting review" edge case.

3. Branch on the rule result.

   **First-attempt branch.** Proceed with the recipe below
   (steps 4–9) unchanged: flip state to `in-progress`, read
   scenarios, implement from scratch, self-review, run the hygiene
   gate, flip to `in-review`, append `<implementer round="1">`.

   **Retry branch.** Enter retry mode:

   - Read the most recent `<implementer>` block in the journal to
     understand the prior pass's stated `Completed` work.
   - Read the latest `<blockers>` block (the one whose `round`
     matches the highest `<implementer>` round) for the specific
     feedback to address.
   - Amend the existing WIP in the working tree to address the
     blockers. Do not run `git restore`, `git clean`, or
     `git checkout` against the dirty paths; do not rewrite the
     touched files from scratch; do not reset state. The dirty
     tree is the prior pass's WIP — iterate on it in place.
   - Flip state to `in-progress` and continue through the same
     hygiene gate and `in-review` flip the first-attempt branch
     uses (the SPEC-0045/REQ-001 hygiene gate runs unchanged).
   - Append the next `<implementer round="N+1">` block where `N`
     is the highest prior `<implementer>` round in the journal,
     monotonically incremented by exactly 1. The retry-mode
     `Completed` field describes the amend (what changed this
     round in response to the blockers), not a restatement of
     the cumulative task work.

4. Flip the target task's `state` from `pending` to `in-progress`
   by editing TASKS.md.

5. Read the task scenarios to understand what must be implemented:

   ```bash
   speccy check SPEC-NNNN/T-003
   ```

6. Implement the task. Write tests first, then code. Run the
   project's own test command (`cargo test`, `pnpm test`, etc.)
   locally. Use `speccy check SPEC-NNNN/T-NNN` to re-read the
   scenarios being satisfied (it renders them, it does not run
   them).

7. Self-review before handoff. Immediately after implementation and
   **before** the exit transition's `in-review` flip, re-read your
   own diff through the reviewers' lens and fix what you find in
   place. This is the cheap place to catch drift: a fix here is a
   few edits in a diff you already have open, whereas the same drift
   caught at review is a full bounce-and-respawn round. Address the
   findings now; do not defer them to the reviewers.

   **Reviewer north-star map.** Each of the four review personas is
   chasing one outcome. Hold your diff to all four — this is what
   "good" looks like, not how the reviewers hunt for problems:

   - **Business.** The change does what the task's covered REQs
     actually ask for — no more, no less. Every changed line traces
     to a requirement; nothing in scope is left undone and nothing
     out of scope sneaks in.
   - **Tests.** Tests drive real behaviour and assert the specific
     contract under test, and the evidence is honest and complete —
     each covered CHK is accounted for, red/green pairs show what
     they claim, and nothing is glossed.
   - **Security.** Inputs are validated, errors are handled rather
     than swallowed, and the change introduces no unsafe shortcut
     (no panicking on attacker-influenced input, no secret or unsafe
     default left in).
   - **Style.** The diff reads as though the surrounding code's
     author wrote it and re-applies the project's own conventions.
     For the specifics, work the shared convention-drift checklist
     below.

   ## Convention-drift checklist

Re-read your own diff against the existing codebase and the project's
own conventions before handing off. These are the recurring categories
where mechanical and convention drift slips through a green hygiene
gate yet still costs a later review round. Catching them here — in the
diff you already have open — is far cheaper than a bounce-and-respawn.

- **Reuse over reinvent.** Before adding a new helper, type, or
  utility, check whether one already exists — including a few
  directories away, where it is easy to miss. Call the existing one
  rather than introducing a parallel implementation.

- **Match local conventions.** Make the diff read as though the
  surrounding code's author wrote it: follow the established naming,
  error-handling, and import-ordering patterns of the files you touch.
  If the neighbouring code propagates errors one way and yours does
  another, or your imports fight the project's formatter, align with
  what is already there.

- **Docs match code.** Any comment, docstring, or documentation you
  add or touch must describe what the code actually does. Stale or
  aspirational prose that no longer matches the behaviour is drift.

- **No false complexity.** Do not add abstraction, indirection, or
  configurability the change does not require. In particular, do not
  split a function into pieces that push the file past its own
  existing complexity ceiling — keep the shape consistent with how the
  rest of the file is structured.

- **Re-apply the project's own hard rules.** Whatever invariants the
  project's conventions declare, hold your diff to them. Two recurring
  traps:
  - **No vacuous or constant-copy tests.** A test must gate a real
    invariant. A test that re-asserts a hard-coded copy of a
    production constant, or only checks that something exists or is
    non-empty, cannot fail in any interesting way — derive a real
    property or drop it.
  - **Suppressions carry a justification.** Every lint or warning
    suppression you add must state why it is there, never a bare
    silencer.


8. Exit transition. **Hygiene gate (REQ-001):** before flipping `state` from `in-progress` to `in-review`, run the four standard hygiene gates in sequence — `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo +nightly fmt --all --check`, `cargo deny check`. Any non-zero exit refuses the flip and keeps the task at `in-progress`; on all zeros, proceed with the flip and record one line per gate naming its exit code in the appended `<implementer>` block's `Hygiene checks` field. When the implementation is done, flip the task's
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
     `model="claude-opus-4-8[1m]/low"`,
     `model="claude-opus-4-8[1m]/medium"`). Hosts without an effort
     knob omit the suffix entirely (e.g. `model="claude-opus-4-8"`).
     The slash-suffix is a documented convention; the parser
     validates `model` is non-empty but does not enforce suffix
     membership.
   - `round` — a monotonic positive integer starting at 1.
     Increment by exactly 1 on each post-blocker retry attempt. The
     first implementer turn on a task is `round="1"`; if a review
     round blocks and the task flips back to `pending`, the next
     implementer attempt writes `round="2"`, and so on. Do not skip
     values; do not reset.

   ## Sourcing your recorded identity

When you record your own identity in a `model="..."` attribute, build
the value from two independently sourced parts: the model segment and
the optional effort suffix. Do not infer either from the skill-pack
name, the persona name, or an inherited environment variable.

- **Model segment — from the host's in-context identifier, verbatim.**
  Use the resolved long-form model identifier your host states
  in-context (for example, a host line such as
  `The exact model ID is claude-opus-4-8[1m]`). Transcribe it exactly,
  preserving version punctuation as the host writes it — keep the
  hyphen form (`claude-opus-4-8`), never normalise it to a dotted form
  (`claude-opus-4.8`), and never substitute a configured alias. Where a
  host states no resolved identifier in-context, fall back to the
  `model:` value in your own agent definition file.

- **Effort suffix — from your own definition file.** When your host
  exposes a reasoning-effort knob, read the effort from your own
  sub-agent definition file (`effort:` on Claude Code,
  `model_reasoning_effort` on Codex) and append it as a slash-suffix
  (e.g. `claude-opus-4-8[1m]/low`). Never derive the effort from
  `CLAUDE_EFFORT` or any other inherited environment variable: a
  sub-agent pinned to a low effort that is dispatched from a
  higher-effort parent session still records its own definition-file
  effort. A host with no effort knob omits the suffix entirely.

- **Override limitation.** The `CLAUDE_CODE_EFFORT_LEVEL` runtime
  override is deliberately not read. A run that sets it still records
  the effort declared in the agent definition file, not the override
  value.


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

9. Exit. Do not continue to the next task. If the caller wants
   another task, the caller invokes this skill again.

After exit, the next reasonable step depends on TASKS.md state: if
any task is `state="in-review"`, suggest
`/speccy-review SPEC-NNNN`. If all tasks are
`state="completed"`, suggest `/speccy-vet SPEC-NNNN`.
