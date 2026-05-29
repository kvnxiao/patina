---
name: speccy-brainstorm
description: Atomize a fuzzy ask into first-principle requirements before any SPEC.md is written. Walks the user through a Socratic exchange (one question at a time, 2-3 alternative framings with trade-offs, silent assumptions, open questions) and stops at a hard gate until the user explicitly approves the framing. Use when the user has a fuzzy idea, says "help me brainstorm", "help me think about", "let's brainstorm", "I want to spec out X but I'm not sure where to start", or before invoking speccy-plan on an unclear ask. Requires no preconditions. Do NOT trigger when the user has named the slice and the scope is clear - go straight to speccy-plan.
---

# /speccy-brainstorm

Atomizes a fuzzy ask into first-principle requirements **before** any
SPEC.md is written. Walks the user through a Socratic exchange:
explore project context, ask clarifying questions one at a time,
propose 2-3 alternative framings with trade-offs, surface silent
assumptions and open questions, then stop and wait for explicit user
approval. Only after the user approves the framing does the agent
invoke `/speccy-plan` to write SPEC.md.

The output of this skill is **ephemeral chat**: nothing is written to
disk. Salient outputs flow into SPEC.md's existing sections when
`/speccy-plan` runs next (see "Routing" below).
Inspired by the obra/superpowers brainstorming skill, trimmed to
Speccy's stay-small principles.

## When to use

- Before drafting a new SPEC slice, whenever the ask is fuzzy or the
  framing has not yet been agreed in the chat. If the user already
  has a sharp, named slice with clear scope, skip brainstorming and
  go straight to `/speccy-plan`.
- Optionally before an amendment, when the intent shift behind the
  amendment is itself fuzzy. In practice the amendment path rarely
  needs brainstorming because the framing is locked in by the
  existing SPEC; the skill is framing-agnostic but reaches for it on
  judgment.

## Hard gate

**Do NOT invoke `/speccy-plan` and do NOT write
SPEC.md until the user has approved the framing.** This is a hard
gate. The whole point of brainstorming is to catch framing drift
while edits are still cheap (pre-Requirement); skipping the gate
defeats the skill. If the user explicitly says "skip the brainstorm,
just write the SPEC", treat that as approval to bypass and invoke
`/speccy-plan` directly — but do not bypass on your
own judgment.

## Steps

1. **Explore project context.** Read `AGENTS.md` (the host harness
   auto-loads it; re-read on demand via your Read primitive). If the
   ask might overlap with an existing slice, query the workspace index
   to see what specs exist:

   ```bash
   speccy status --json
   ```

   Don't dump the context back at the user — use it to ground your
   clarifying questions.

2. **Ask clarifying questions, one at a time.** One question per
   message. Multiple-choice questions are preferred when the answer
   space is enumerable; open-ended is fine when it isn't. Don't
   batch three questions into one message — that nudges the user
   toward shallow answers. Keep asking until you can produce the
   four artifacts in step 3 without inventing details.

3. **Produce four artifacts.** Once you can answer the user's intent
   in your own words, present these four artifacts in one message:

   1. **Restated ask, atomized.** Restate the user's ask in your own
      words, broken into atomic, first-principle requirements. Each
      requirement should be one sentence describing one observable
      outcome. If you find yourself writing "and" or "also" inside a
      single requirement, split it. This is the slice's
      pre-Requirement skeleton — when `/speccy-plan`
      runs next, each bullet here typically becomes a `<requirement>`
      block in SPEC.md.

      **Collapse-parallels heuristic.** When N restated-ask
      requirements differ only by one noun, you MAY express them as one
      requirement with an enumerated sub-list to reduce reader cognitive
      load. For example, if R1-R6 all read "the X self-review verifies
      Y" (differing only in X), collapse to a single requirement with
      six sub-bullets a-f rather than repeating the identical sentence
      structure six times. This is agent discretion — there is no
      enforced threshold. If collapsing does not improve readability,
      keep the requirements atomic.

   2. **2-3 alternative framings.** List 2-3 alternative framings of
      the ask, with trade-offs. The `2-3` is soft guidance — scale
      to the ask's complexity. A trivial ask may surface zero
      alternatives; a load-bearing architecture ask may need four.
      For each framing, include:
      - a one-sentence sketch of what the SPEC would look like under
        that framing; and
      - the reason you rejected it in favor of the chosen framing.

   3. **Silent assumptions.** List the assumptions you would
      otherwise bake into the SPEC without naming. Pick the
      assumptions that, if wrong, would change the SPEC shape rather
      than mechanical filler ("the project will use Rust" on a Rust
      project is true but useless).

   4. **Open questions.** List the open questions whose answers
      would change the SPEC shape. Use the alpha-prefix format:
      `- [ ] a.`, `- [ ] b.`, ..., `- [ ] z.`, assigning the next
      free letter to each question. This format matches the PRD
      template's `## Open Questions` section so the output is
      copy-pasteable into the eventual SPEC.md without reformatting.
      Reaching `z.` signals an over-scoped session — 26 open
      questions is a scope smell, not a format limitation.

4. **Pre-check pass.** Before presenting the four artifacts to the
   user, run this internal review pass exactly once. Do not re-check
   after the artifacts are presented.

   This pre-check fires on every brainstorm invocation — including
   when brainstorm is used as a front-end to amendment flows (since
   amendments routed through brainstorm are still brainstorm sessions).

   <!-- Note: the brainstorm pre-check below is an independent copy.
        The parallel copies for plan and amend live in speccy-plan.md
        and speccy-amend.md respectively (per DEC-001 / OQ-b: two
        independent copies, no shared partial). -->

   **Mechanical/semantic split.** Mechanical issues are
   string-matchable from the draft artifacts: `TBD`/`TODO` strings,
   "and"/"also" inside a single restated requirement, untouched
   `<...>` template placeholders, missing alpha-prefix ordinals in
   the open questions list. Fix mechanical issues inline by revising
   the draft artifacts before presenting them — do not mention the
   fix in chat. If judging requires reading semantics, it is semantic.

   Semantic issues surface in chat as a preamble block immediately
   before the four artifacts, using this fixed-format template:

   Opening line (verbatim, unchanged):
   `**Self-review caught the following before presenting artifacts:**`

   Then one bullet per semantic issue:
   `- {issue}` (one-line description)

   Closing line (verbatim, unchanged):
   `Proceeding with the four artifacts below.`

   If the pre-check finds no issues, omit the preamble block entirely
   and present the four artifacts directly.

   **The four check properties:**

   - **Atomized restated requirements.** Each restated-ask
     requirement describes one observable outcome and no single
     requirement contains "and"/"also" multi-outcome wording.
     Mechanical fix: split before presenting.

   - **Structurally distinct framings.** The alternative framings in
     artifact 2 are genuinely distinct framings — not paraphrases of
     the same choice. Two framings that differ only in wording, not
     in structural consequence, should be merged or replaced.

   - **Load-bearing assumptions.** Each silent assumption in artifact
     3 would, if wrong, change the SPEC shape. Assumptions that are
     trivially true given the project context (e.g., "the project
     uses Rust" on a Rust project) are filler — remove them.

   - **Shape-changing open questions.** Each open question in artifact
     4 must be answerable in a way that changes the SPEC shape. A
     question whose answer doesn't affect the requirements, summary,
     or assumptions is not load-bearing and should be dropped.

5. **Stop and wait.** After presenting the four artifacts, **stop
   and wait** for the user to confirm, redirect, or answer the open
   questions. Do not move on. Do not write SPEC.md. Do not invoke
   `/speccy-plan`. The hard gate above applies until
   the user has responded with explicit approval.

6. **Iterate if needed.** If the user redirects or answers an open
   question, fold the response back into the four artifacts and
   re-present. Continue until the user explicitly approves the
   framing.

7. **Invoke the writing skill.** Once the user has approved, invoke
   the right skill for the path:

   - For a **new SPEC**, invoke `/speccy-plan`, which
     queries `speccy vacancy --json` to allocate the next SPEC ID and
     writes a new SPEC.md following the PRD template.

   - For an **amendment** to an existing SPEC, invoke
     `/speccy-amend` (not `/speccy-plan`
     directly). `/speccy-amend` orchestrates the full
     amendment loop — SPEC.md edit, Changelog row, TASKS.md reconcile,
     and spec-hash re-record — so the brainstormed amendment doesn't
     drop the reconciliation steps and produce hash drift.

   The brainstorm chat is ephemeral — nothing was written to disk
   during steps 1-6. The salient outputs flow into SPEC.md via the
   routing list below when the writing prompt runs.

## Routing brainstorm outputs into SPEC.md

When `/speccy-plan` runs after the brainstorm, fold
the four artifacts into the PRD template's existing sections — do
not invent a new SPEC.md section for brainstorm output:

- The **restated ask** informs the `## Summary` prose. Fold it in
  as a designed artifact; do not paste the brainstorm wording
  verbatim. Each atomized requirement typically becomes a
  `<requirement>` block under `## Requirements`.
- The **silent assumptions** land in the `<assumptions>` element
  block under `## Assumptions`.
- The **open questions** land under `## Open Questions` in the same
  `- [ ] a.` alpha-prefix format the brainstorm produced — copy-paste
  directly without reformatting.
- The **rejected alternative framings** land under `## Notes`. When
  a trade-off is load-bearing enough to deserve a durable decision
  record, promote it to a `<decision>` element block under
  `### Decisions` (DEC-NNN) instead.

## Key principles

- **One question at a time.** Don't overwhelm the user with batched
  questions. Multiple-choice is preferred when answers are
  enumerable.
- **Scale to complexity.** The `2-3` alternative-framings count is
  soft guidance. Trivial asks surface fewer artifacts; load-bearing
  asks surface more.
- **Atomize ruthlessly.** If a requirement reads "do X and Y", split
  it. Each requirement should describe one observable outcome.
- **No premature implementation.** Brainstorming is about the SPEC
  shape, not the code. Stay at the "what / why" level; resist
  diving into "how".
- **Hard gate before SPEC.md.** Do not invoke
  `/speccy-plan` until the user has explicitly
  approved the framing.

This recipe does not loop. After invoking
`/speccy-plan` (new SPEC) or
`/speccy-amend` (existing SPEC), the next step is
`/speccy-decompose SPEC-NNNN` (decompose into TASKS.md).
