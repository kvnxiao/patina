---
name: speccy-brainstorm
description: Atomize a fuzzy ask into first-principle requirements before any SPEC.md is written. Walks the user through a Socratic exchange (one question at a time, 2-3 alternative framings with trade-offs, silent assumptions, open questions) and stops at a hard gate until the user explicitly approves the framing. Use when the user has a fuzzy idea, says "help me brainstorm", "help me think about", "let's brainstorm", "can we brainstorm", "I want to spec out X but I'm not sure where to start", or before invoking speccy-plan on an unclear ask. Requires no preconditions. Do NOT trigger when the user has named the slice and the scope is clear - go straight to speccy-plan.
---

# /speccy-brainstorm

Atomizes a fuzzy ask into first-principle requirements **before** any
SPEC.md is written. Walks the user through a Socratic exchange:
explore project context, ask clarifying questions one at a time,
propose 2-3 alternative framings with trade-offs, surface silent
assumptions and open questions, then stop and wait for explicit user
approval. Only after the user approves the framing does the agent
invoke `/speccy-plan` to write SPEC.md.

The output of this skill is **ephemeral chat**: salient outputs flow
into SPEC.md's existing sections when `/speccy-plan`
runs next (see "Routing" below). The one exception is a deliberately
deferred future-spec candidate, which the skill appends to
`.speccy/BACKLOG.md` at step 7. Inspired by the obra/superpowers
brainstorming skill, trimmed to Speccy's stay-small principles.

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

   When the ask touches existing code, invoke the `plan-explorer`
   subagent to trace the relevant feature through its entry points,
   call flows, and architecture layers. Its grounding report is
   **ephemeral**: do NOT write it to a new `*.md` artifact file. Fold
   the salient findings into your clarifying questions now, and route
   them into SPEC.md's existing sections (Summary prose and
   `<requirement>` grounding) when `/speccy-plan` runs
   next (see "Routing" below) — never into a standalone report file.

   Then read the backlog as candidate input. When `.speccy/BACKLOG.md`
   is present, read it and fold its entries into the candidate framings
   you propose at step 3 — a deferred candidate may be exactly the slice
   the user is now reaching for. Absence is normal and silent: a missing
   file is not an error, proceed without comment.

   Don't dump the context back at the user — use it to ground your
   clarifying questions.

2. **Ask clarifying questions, one at a time.** One question per
   message. Multiple-choice questions are preferred when the answer
   space is enumerable; open-ended is fine when it isn't. Don't
   batch three questions into one message — that nudges the user
   toward shallow answers. Keep asking until you can produce the
   four artifacts in step 3 without inventing details.

3. **Produce four artifacts.** Once you can answer the user's intent
   in your own words, present these four artifacts in one message.
   Stay at the "what / why" level throughout — brainstorming is about
   the SPEC shape, not the code; resist diving into "how".

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

7. **Record a future-spec candidate, if one was deliberately deferred.**
   When the Socratic exchange settled on cutting a piece of scope worth
   its OWN later spec, append a candidate with provenance
   `SPEC-NNNN, brainstorm`, per the reference below. This is the one
   disk write the brainstorm itself performs; everything else stays
   ephemeral chat. Brainstorm writes no SPEC and never promotes a
   candidate, so it never strikes a backlog entry — promotion is the
   writing skill's job. Brainstorm itself never commits; the writing
   skill it hands off to at step 8 (`/speccy-plan` for a
   new SPEC, `/speccy-amend` for an amendment) sweeps this
   dirty `.speccy/BACKLOG.md` into its commit, so the candidate is
   recorded durably rather than left as an uncommitted working-tree
   change.

## Backlog ledger entry shape

The repo's future-spec register lives at `.speccy/BACKLOG.md` — a user-owned,
git-tracked file, sibling to `MEMORY.md` and distinct from it. `speccy init`,
`speccy init --force`, and reeject never create, enumerate, or overwrite it, so
learned content survives speccy CLI updates. Its **absence is normal and
silent**: a missing or malformed file produces no `speccy verify` error or
warning, and the CLI never reads it. The backlog is a flat, unordered list of
candidate specs — ideas worth their own SPEC later, not deferrals within a spec
already in flight.

### When to append a candidate

A producing phase appends an entry here only when it deliberately cuts a piece
of scope worth its OWN later spec — "not this spec, but its own SPEC later."
Self-create the file with the header below (copied verbatim) on first append,
then record the cut in the four-field shape below.

Distinguish the two kinds of cut. A future-spec candidate goes here. A cut that
is merely out of the current spec's scope is a spec-local Non-goal — it belongs
in that SPEC's `## Non-goals`, not the backlog.

### The file header

When the file self-creates on first append, the producing skill copies in this
preamble verbatim so the lifecycle stays legible to the next reader:

```markdown
# Speccy backlog — future-spec candidates

> User-owned, git-tracked, never created or overwritten by `speccy init`,
> `speccy init --force`, or reeject. Absence is normal and silent; the CLI
> never reads this file. Distinct from `MEMORY.md` (durable loop conventions)
> and from spec-local deferred surfaces (`## Non-goals`, deferred decisions,
> deferred coverage): each entry below should become its OWN spec. Promotion
> retires an entry by deletion. See
> `resources/modules/references/backlog-ledger.md` for the entry shape.
```

### The four-field entry shape

Every entry carries the same four fields, one line per field:

- **Title** — the prospective spec named in a phrase.
- **What & why** — what the spec would deliver plus the value it carries: the
  case for building it.
- **Deferred-because** — why it is not being built now: out of the current
  slice, needs infrastructure that does not exist yet, or blocked on some
  named prerequisite.
- **Provenance** — the originating spec and phase that surfaced the candidate,
  e.g. `SPEC-NNNN, ship` or `SPEC-NNNN, plan`, or `manual` for a hand-added
  entry.

### Authoring discipline

- **Terse.** One phrase per field. The backlog is a working list scanned at
  plan time, not a design document; a candidate that needs a paragraph to
  justify wants its own brainstorm, not a longer backlog line.

- **Provenance must resolve to a real spec and phase**, never a fabricated one
  — or `manual` when added by hand. Honest provenance is what lets a reader
  trace a candidate back to the moment it surfaced.

- **Promotion strikes the entry by deletion.** When a candidate becomes its own
  SPEC, delete its line; the promotion trail lives in git history and the new
  SPEC's own provenance. The backlog reads as current candidates only, never a
  tombstone field.

- **Many entries from one spec's loop is a focus smell.** The per-spec add rate
  is itself feedback: a single spec spawning a long tail of backlog entries
  signals the slice was drawn too wide or the work kept discovering adjacent
  scope. This is a signal to weigh, not an enforced threshold — nothing gates
  on it.

### Worked example

The placeholders below are illustrative — substitute your own values.

```markdown
- Title: Cross-repo spec linking.
- What & why: let a SPEC in one repo reference requirements in another so a
  shared contract has one source of truth; removes the copy-paste drift between
  the two repos that share the protocol.
- Deferred-because: needs a cross-repo resolution surface that does not exist
  yet — out of the current single-repo slice.
- Provenance: SPEC-0042, ship.
```


8. **Invoke the writing skill.** Once the user has approved, invoke
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

   The brainstorm chat is ephemeral — the only disk write is the
   backlog entry from step 7, if a future-spec candidate was deferred.
   The salient outputs flow into SPEC.md via the routing list below
   when the writing prompt runs.

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

## Exit

Brainstorm's output is the framing the user approved, which flows into SPEC.md
via the routing list above when the writing skill runs; its only disk write is
a deferred future-spec candidate appended to `.speccy/BACKLOG.md` at step 7. The
skill ends by invoking `/speccy-plan` (new SPEC)
or `/speccy-amend` (existing SPEC); the step after that is
`/speccy-decompose SPEC-NNNN` (decompose into TASKS.md). Single
pass, no loop.
