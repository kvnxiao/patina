---
name: speccy-work
description: Implements one Speccy task per invocation. Invoke via /agent speccy-work for the pinned execution path defined in this file's frontmatter.
model: opus[1m]
effort: high
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
journal carried in the `speccy context` bundle's journal section and
switches modes accordingly — the caller does not pass a flag. A first-attempt
task (no journal, or no `<blockers>` matching the highest
`<implementer>` round) runs today's recipe unchanged. A retry-shape
task (the latest `<blockers>` round equals the highest
`<implementer>` round) amends the existing WIP in the working tree
in place and appends a new `<implementer>` block (the CLI stamps the
incremented round).

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

**Entry precondition (SPEC-0045 REQ-002, extended by SPEC-0047 REQ-002 / REQ-003):** before any Task dispatch, (i) resolve the target task per step 1, then open the per-task context read with a single `speccy context SPEC-NNNN/T-NNN --json` call (the bundle carries the task entry, its covering requirements and scenarios, the latest-round journal blocks inline with prior rounds as an attributes-only index, the sibling index, the file paths, and a suggested merge-base diff command); (ii) apply the retry-shape rule summarized at step 2 (canonical statement at `.claude/speccy-references/retry-shape.md`) against the bundle's journal section rather than a separate file read, (iii) run `git status --porcelain`. **First-attempt shape** with non-empty stdout exits the skill (surface dirty paths on stderr); empty stdout proceeds with the first-attempt branch (today's SPEC-0045/REQ-002 behaviour). **Retry shape** proceeds with the retry branch regardless of stdout — the dirty paths are the prior pass's WIP that the retry implementer amends in place; no dirty-paths surface is written. If `speccy next --json` then returns `next_action.kind == "reconcile"`, dispatch the reconcile pass per the **Reconcile policy** below rather than the normal implementer flow.

**Reconcile policy.** When `speccy next --json` (in either per-spec
or workspace form) returns `next_action.kind == "reconcile"`, iterate
`consistency.drifts[]` and apply the table action per entry, then
re-query before proceeding. See
`.claude/speccy-references/reconcile-policy.md` for the full
policy table and the three properties the dispatch holds by
construction (autonomous / rollback-biased / idempotent).


1. Resolve the target task.

   - If a `SPEC-NNNN/T-NNN` selector was passed, that is the target.
   - Otherwise, query the CLI in workspace form (no SPEC selector
     is known on this no-selector invocation path — we must walk
     the active tree to find the next implementable task):

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
     with `next_action.kind == "work"`, exit and report
     that no implementable tasks remain. Otherwise, construct
     the disambiguated `<spec>/<task>` form from the JSON's `spec_id`
     and `next_action.task_id` fields (the bare task ID is
     ambiguous across specs — every spec has its own `T-001`).

     Exit-code-stop contract: once SPEC-NNNN is resolved, any
     subsequent per-spec query (`speccy next SPEC-NNNN --json`) that
     exits non-zero means the SPEC has reached a terminal state —
     halt and surface the stderr line. Only parse JSON when exit
     code is 0.


2. Apply the REQ-001 retry-shape rule summarized immediately below
   against the journal section of the bundle returned by the
   `speccy context` call in step 1 (no separate journal-file read).
   The rule inspects only that journal content — it makes no further
   git, `speccy next`, or other CLI call. Compute the result
   (first-attempt shape or retry shape); the rest of the recipe
   branches on this result.

   **Retry shape.** A task is in retry shape iff its journal contains
both an `<implementer>` element and a `<blockers>` element whose
`round` attribute matches the highest implementer round. Otherwise
it's first-attempt shape — the strict clean-tree gate applies. See
`.claude/speccy-references/retry-shape.md` for the full rule
statement, read-only scope, worked examples, and the
"implementer awaiting review" edge case.

3. Branch on the rule result.

   **First-attempt branch.** Proceed with the recipe below
   (steps 4–10) unchanged: flip state to `in-progress`, read
   scenarios, run the bounded reuse survey, implement from scratch,
   self-review, run the hygiene gate, flip to `in-review`, append the
   round-1 `<implementer>` block via `speccy journal append`.

   **Retry branch.** Enter retry mode:

   - Read the latest-round `<implementer>` block inlined in the
     bundle's journal section to understand the prior pass's stated
     `Completed` work.
   - Read the latest-round `<blockers>` block (the one whose `round`
     matches the highest `<implementer>` round, also inlined) from
     that same journal section for the specific feedback to address.
   - Prior rounds are not inlined: the bundle lists them as an
     attributes-only index (`round`, `block`, `persona`, `verdict`).
     If a prior round's prose is needed — e.g. a persona blocking
     across rounds — drill in explicitly with `speccy journal show
     SPEC-NNNN/T-NNN --round N [--block <type>]`.
   - Amend the existing WIP in the working tree to address the
     blockers. Do not run `git restore`, `git clean`, or
     `git checkout` against the dirty paths; do not rewrite the
     touched files from scratch; do not reset state. The dirty
     tree is the prior pass's WIP — iterate on it in place.
   - Flip state to `in-progress` via `speccy task transition
     SPEC-NNNN/T-NNN --to in-progress` and continue through the same
     hygiene gate and `speccy task transition … --to in-review` flip
     the first-attempt branch uses (the SPEC-0045/REQ-001 hygiene gate
     runs unchanged). Never edit the `state` attribute in TASKS.md
     directly.
   - Append the next `<implementer>` block via `speccy journal
     append` (step 9); the CLI derives and stamps the incremented
     round. The retry-mode `Completed` field describes the amend
     (what changed this round in response to the blockers), not a
     restatement of the cumulative task work.

4. Flip the target task's `state` from `pending` to `in-progress`
   through the transition command — never by editing the `state`
   attribute in TASKS.md directly:

   ```bash
   speccy task transition SPEC-NNNN/T-NNN --to in-progress
   ```

   The CLI enforces the legal state graph and rewrites the `state`
   attribute byte-surgically; an illegal edge or unresolved selector
   exits non-zero with the file untouched.

5. Read the task scenarios to understand what must be implemented
   from the bundle's covering-requirements section returned by the
   `speccy context` call in step 1 — its `requirements` carry each
   covered REQ's `<done-when>`, `<behavior>`, and `<scenario>`
   blocks. No separate entry read of SPEC.md, TASKS.md, or `speccy
   check` is needed here.

6. Bounded reuse survey. Before writing any code, survey the
   task-relevant area and classify the code you are about to add into
   reuse-as-is / extend / write-fresh, so reuse is a design input
   rather than a post-hoc cleanup. Scope the survey to the task's
   area — its covered REQs, the suggested-files hint, and the
   immediate module / neighbouring files — and **not** the whole repo.
   Let the survey inform what you write in the next step.

   ## Reuse survey (implementer: survey-and-build)

Before writing any code, survey the task-relevant area and decide,
for the code you are about to add, whether to reuse, extend, or write
fresh. Reuse is a design input here, not a post-hoc cleanup: you
classify what already exists *before* you commit to a shape, so you
build on it instead of laying down a parallel implementation that a
later review round has to unwind.

**Bounded to the task's area.** Map only the area the task touches:
its covered REQs, the suggested-files hint in the task body, and the
immediate module plus its neighbouring files. This is explicitly
**not** a whole-repo scan — reusable code far outside the task's area
is out of scope by design, and hunting for it is wasted budget.

**The three tiers.** Classify the relevant existing code you find,
and for each thing you decide to add, place it in one tier:

- **Reuse-as-is.** An existing symbol already does what you need —
  call it. Name the specific existing symbol (function, type,
  constant, helper) you are reusing.
- **Extend.** An existing symbol nearly does what you need and should
  grow to cover your case rather than be duplicated. Name the
  specific existing symbol you are extending.
- **Write-fresh.** Nothing existing fits, so you write something new.
  Name the search that came up empty (what you looked for and where),
  so the absence is auditable rather than assumed.

**Round semantics.**

- The **full area-map** is round-1 only. Re-run it on a retry round
  *only* when a reuse-related blocker was raised against the prior
  round; a retry that addresses a non-reuse blocker does not re-survey
  the area.
- The **per-symbol floor** is round-agnostic. For every new top-level
  symbol the implementation introduces — in any round — name the
  existing thing it reuses or extends, or the search that found
  nothing. A retry that adds a new top-level symbol still owes this
  per-symbol accounting even when the full area-map is not re-run.


7. Implement the task. Write tests first, then code. Run the
   project's own test command (`cargo test`, `pnpm test`, etc.)
   locally. Use `speccy check SPEC-NNNN/T-NNN` to re-read the
   scenarios being satisfied (it renders them, it does not run
   them).

8. Self-review before handoff. Immediately after implementation and
   **before** the exit transition's `in-review` flip, re-read your
   own diff through the reviewers' lens and fix what you find in
   place. This is the cheap place to catch drift: a fix here is a
   few edits in a diff you already have open, whereas the same drift
   caught at review is a full bounce-and-respawn round. Address the
   findings now; do not defer them to the reviewers.

   **Reviewer north-star map.** Hold your diff to all four review
   outcomes:

   - **Business.** Every changed line traces to a covered REQ — no
     more, no less.
   - **Tests.** Tests drive real behaviour, each covered CHK is
     accounted for, and the evidence is honest.
   - **Security.** Inputs validated, errors handled not swallowed, no
     unsafe shortcut or leaked secret.
   - **Style.** Reads as the surrounding author wrote it; see the
     convention-drift checklist below.

   ## Convention-drift checklist

Re-read your own diff against the existing codebase and the project's
own conventions before handing off. These are the recurring categories
where mechanical and convention drift slips through a green hygiene
gate yet still costs a later review round. Catching them here — in the
diff you already have open — is far cheaper than a bounce-and-respawn.

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


9. Exit transition. **Hygiene gate (REQ-001):** before flipping `state` from `in-progress` to `in-review`, run the four standard hygiene gates in sequence — `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo +nightly fmt --all --check`, `cargo deny check`. Any non-zero exit refuses the flip and keeps the task at `in-progress`; on all zeros, proceed with the flip and record one line per gate naming its exit code in the appended `<implementer>` block's `Hygiene checks` field. When the implementation is done, flip the task's
   `state` from `in-progress` to `in-review` through the transition
   command — never by editing the `state` attribute in TASKS.md
   directly:

   ```bash
   speccy task transition SPEC-NNNN/T-NNN --to in-review
   ```

   Then append one `<implementer>` block to the per-task
   journal via `speccy journal append`. Do NOT inline an
   `<implementer-note>` inside the `<task>` body in TASKS.md — the
   parser rejects that element. The journal file is the canonical
   home for implementer handoff prose.

   Append via the CLI. Pipe the seven-field body on stdin:

   ```bash
   speccy journal append SPEC-NNNN/T-NNN --block implementer \
     --model <your-model> <<'EOF'
   - Reuse survey: ...
   - Completed: ...
   - Undone: ...
   - Hygiene checks: ...
   - Evidence: ...
   - Discovered issues: ...
   - Procedural compliance: ...
   EOF
   ```

   The CLI owns the appended block's `date`, `round`, and open/close
tags, plus the journal's frontmatter and sectioning. **Do not
compute, supply, or hand-author any of them** — there is no override
flag; the body you pipe on stdin is the inner text only. Validation
runs before any write, so a malformed body leaves the journal
byte-identical.


   `--model` is required and validated non-empty.

   ## Sourcing your recorded identity

Build the `model="..."` value from two independently sourced parts;
never infer either from the skill-pack name, the persona name, or an
inherited environment variable.

- **Model segment** — the resolved long-form identifier your host
  states in-context (e.g. `claude-opus-4-8[1m]`), transcribed
  verbatim: keep the host's version punctuation (`claude-opus-4-8`,
  never `claude-opus-4.8`), never substitute a configured alias.
  When the host states no resolved identifier in-context, fall back
  to the `model:` value in your own agent definition file.
- **Effort suffix** — when the host exposes a reasoning-effort knob,
  read it from your own definition file (`effort:` on Claude Code,
  `model_reasoning_effort` on Codex) and append it as a slash-suffix
  (e.g. `claude-opus-4-8[1m]/low`); never read it from a runtime
  env override. A host with no effort knob omits the suffix
  entirely.


   Canonical journal `<implementer>` shape: `.claude/speccy-references/journal-implementer.md`.

   Canonical evidence file shape: `.claude/speccy-references/evidence.md`.

   Body content. Use the seven-field handoff template the implementer
   prompt supplies (`Reuse survey`, `Completed`, `Undone`,
   `Hygiene checks`, `Evidence`, `Discovered issues`,
   `Procedural compliance`). The
   `Evidence` field must include a CHK-by-CHK roll call labelling
   each CHK under the task's covered REQs as `demonstrated`,
   `hygiene`, or `judgment-only` -- see the canonical reference for
   the format and what each label means (the canonical reference
   carries a full worked roll-call example).

   The CLI validates the block before any write, so a malformed body
   never lands and no re-read is needed; confirm `speccy next --json`
   reports no consistency drift.

10. Exit. Do not continue to the next task. If the caller wants
   another task, the caller invokes this skill again.

After exit, the next reasonable step depends on TASKS.md state: if
any task is `state="in-review"`, suggest
`/speccy-review SPEC-NNNN`. If all tasks are
`state="completed"`, suggest `/speccy-vet SPEC-NNNN`.
