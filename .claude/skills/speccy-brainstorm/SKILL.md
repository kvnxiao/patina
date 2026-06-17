---
name: speccy-brainstorm
description: Atomize a fuzzy ask into first-principle requirements before any SPEC.md is written. Walks the user through a Socratic exchange (one question at a time, splitting an over-scoped ask first), then presents a single framing brief - a restated ask, atomized requirements, the recommended framing with its rejected alternatives, and a decisions table the user answers by key. Stops at a hard gate until the user explicitly approves the framing. Use when the user has a fuzzy idea, says "help me brainstorm", "help me think about", "let's brainstorm", "can we brainstorm", "I want to spec out X but I'm not sure where to start", or before invoking speccy-plan on an unclear ask. Requires no preconditions. Do NOT trigger when the user has named the slice and the scope is clear - go straight to speccy-plan.
---

# /speccy-brainstorm

Atomizes a fuzzy ask into first-principle requirements **before** any
SPEC.md is written. Walks the user through a Socratic exchange:
explore project context (and split an over-scoped ask before refining
it), ask clarifying questions one at a time, then present a single
**framing brief** — restated ask, atomized requirements, the framing
you recommend with the alternatives you rejected, and one decisions
table the user answers by key. Stop at a hard gate until the user
approves. Only then does the agent invoke
`/speccy-plan` to write SPEC.md.

The output is **ephemeral chat**: salient outputs flow into SPEC.md's
existing sections when `/speccy-plan` runs next (see
"Routing" below). The one exception is a deliberately deferred
future-spec candidate, appended to `.speccy/BACKLOG.md` at step 7.

## When to use

- Before drafting a new SPEC slice, whenever the ask is fuzzy or the
  framing has not yet been agreed in the chat. If the user already
  has a sharp, named slice with clear scope, skip brainstorming and
  go straight to `/speccy-plan`.
- Optionally before an amendment, when the intent shift is itself
  fuzzy. The amendment path rarely needs it — the existing SPEC locks
  the framing — so reach for it on judgment.

## Hard gate

**Do NOT invoke `/speccy-plan` and do NOT write
SPEC.md until the user has approved the framing.** Brainstorming
catches framing drift while edits are still cheap (pre-Requirement);
skipping the gate defeats it. If the user explicitly says "skip the
brainstorm, just write the SPEC", treat that as approval to bypass and
invoke `/speccy-plan` directly — but never bypass on
your own judgment.

## Steps

1. **Explore project context.** The `AGENTS.md` product north star is
   already in your context — lean on it to frame the ask. If the ask
   might overlap with an existing slice, query the workspace index:

   ```bash
   speccy status --json
   ```

   When the ask touches existing code, invoke the `plan-explorer`
   subagent to trace the relevant feature through its entry points,
   call flows, and architecture layers. Its grounding report is
   **ephemeral**: fold the salient findings into your clarifying
   questions now, and route them into SPEC.md's existing sections when
   `/speccy-plan` runs next (see "Routing"). Never write
   it to a standalone `*.md` report file.

   Then read `.speccy/BACKLOG.md` if present, and fold its entries into
   the framings you propose at step 3 — a deferred candidate may be
   exactly the slice the user is now reaching for. Absence is normal
   and silent: a missing file is not an error.

   Don't dump the context back at the user — use it to ground your
   clarifying questions.

   **Scope check (early exit).** Before refining detail, judge whether
   the ask is one slice or several. If it describes multiple
   independent subsystems, say so now and help the user decompose it:
   what the independent pieces are, how they relate, what order to
   build them. Brainstorm only the first slice through the steps
   below; record each deferred sub-slice as a future-spec candidate at
   step 7. One SPEC stays one coherent slice — refining questions
   against a multi-subsystem ask wastes the exchange.

2. **Ask clarifying questions, one at a time.** One question per
   message. Default to multiple-choice: when the answer space is
   enumerable, offer the options through the host's structured-choice
   affordance if it has one, else as a short lettered list in prose —
   a concrete menu is faster to answer and pins the answer space. Name
   the option you recommend and give a one-line reason inline in its
   description: with the host affordance, put it first and tag it
   recommended; in the prose fallback, append `(recommended: why)` to
   that letter. Recommend, don't decide — the user still picks.
   Reserve open-ended questions for genuinely open answer spaces.
   Don't batch three questions into one message — that nudges the user
   toward shallow answers. Keep asking until you can fill the framing
   brief in step 3 without inventing details.

3. **Present the framing brief.** Once you can state the user's
   intent in your own words, present it as ONE cohesive brief — not a
   set of parallel lists. Stay at the "what / why" level throughout —
   brainstorming is about the SPEC shape, not the code; resist diving
   into "how". The brief has four sections, in order:

   1. **What you're asking for.** A short narrative (2-4 sentences)
      restating the ask in your own words — the cohesive "what and
      why". This grounds the `## Summary` prose when
      `/speccy-plan` runs next.

   2. **Requirements.** The ask broken into atomic, first-principle
      requirements, numbered. Each is one observable outcome; if you
      write "and"/"also" inside one, split it. Each typically becomes
      a `<requirement>` block in SPEC.md.

      **Collapse-parallels heuristic.** When N requirements differ
      only by one noun, you MAY express them as one requirement with
      an enumerated sub-list to reduce reader load — e.g. six
      "the X self-review verifies Y" lines (differing only in X)
      collapse to one requirement with sub-bullets a-f rather than
      repeating the identical sentence six times. Agent discretion, no
      enforced threshold; keep them atomic if collapsing doesn't
      improve readability.

   3. **How I'd frame it.** One short paragraph naming the framing you
      recommend and why, then a compact "considered & rejected" table
      — one row per alternative, with the reason you rejected it.
      Scale the row count to the ask: a trivial ask may have zero
      alternatives, a load-bearing architecture ask may need four.
      Rows must be structurally distinct framings, not paraphrases of
      the same choice.

   4. **Decisions I need from you.** ONE table merging every fork that
      would change the SPEC shape. Each row carries an alpha key
      (`a.`, `b.`, ...), the question, and your proposed default — or
      `?` when you genuinely have no default and need the user to
      decide. A row with a default is a silent assumption made
      visible; a `?` row is an open question — still name the approach
      you lean toward, and why, in the question text, but leave the
      default `?` so the user actively chooses instead of inheriting a
      silent assumption. Close the table with:
      "Reply 'go' to take the defaults, or override any row by key."
      Reaching key `z.` signals an over-scoped session — a scope
      smell, not a format limit.

4. **Pre-check pass.** Before presenting the brief, run this internal
   review pass exactly once. Do not re-check after it is presented.

   This pre-check fires on every brainstorm invocation — including
   when brainstorm is used as a front-end to amendment flows (since
   amendments routed through brainstorm are still brainstorm sessions).

   **Mechanical/semantic split.** Mechanical issues are
   string-matchable from the draft brief: `TBD`/`TODO` strings,
   "and"/"also" inside a single requirement, untouched `<...>`
   template placeholders, missing alpha keys on decision-table rows.
   Fix mechanical issues inline by revising the draft before
   presenting it — do not mention the fix in chat. If judging requires
   reading semantics, it is semantic.

   Semantic issues surface in chat as a preamble block immediately
   before the brief, using this fixed-format template:

   Opening line (verbatim, unchanged):
   `**Self-review caught the following before presenting the brief:**`

   Then one bullet per semantic issue:
   `- {issue}` (one-line description)

   Closing line (verbatim, unchanged):
   `Proceeding with the framing brief below.`

   If the pre-check finds no issues, omit the preamble block entirely
   and present the brief directly.

   **The four check properties:**

   - **Atomized requirements.** Each requirement describes one
     observable outcome; no single requirement contains "and"/"also"
     multi-outcome wording. Mechanical fix: split before presenting.

   - **Structurally distinct framings.** The rows of the "considered &
     rejected" table are genuinely distinct framings — not paraphrases
     of the same choice. Two that differ only in wording, not in
     structural consequence, should be merged or replaced.

   - **Load-bearing defaults.** Each defaulted decision row would, if
     its default is wrong, change the SPEC shape. A row whose answer
     is trivially true given the project context (e.g. "use Rust" on a
     Rust project) is filler — drop it.

   - **Shape-changing open rows.** Each `?` decision row must be
     answerable in a way that changes the SPEC shape. A question whose
     answer doesn't affect the requirements, summary, or framing is
     not load-bearing and should be dropped.

5. **Stop and wait.** After presenting the brief, **stop and wait**
   for the user to confirm, redirect, or answer the decisions table.
   Do not move on. Do not write SPEC.md. Do not invoke
   `/speccy-plan`. The hard gate above applies until
   the user has responded with explicit approval.

6. **Iterate if needed.** If the user redirects or answers a decision
   row, fold the response back into the brief and re-present. A
   resolved `?` row becomes a defaulted row carrying their answer.
   Continue until the user explicitly approves the framing.

7. **Record a future-spec candidate, if one was deliberately deferred.**
   When the exchange cut a piece of scope worth its OWN later spec,
   append a candidate with provenance `SPEC-NNNN, brainstorm`, per the
   reference below. This is brainstorm's only disk write. Brainstorm
   appends but never promotes or strikes an entry (the writing skill
   owns that), and never commits: the step-8 hand-off
   (`/speccy-plan` or `/speccy-amend`)
   sweeps the dirty `.speccy/BACKLOG.md` into its commit.

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

## Routing brainstorm outputs into SPEC.md

When `/speccy-plan` runs after the brainstorm, fold
the brief's sections into the PRD template's existing sections — do
not invent a new SPEC.md section for brainstorm output:

- **What you're asking for** grounds the `## Summary` prose. Fold it
  in as a designed artifact; don't paste the brainstorm wording
  verbatim.
- Each **requirement** typically becomes a `<requirement>` block
  under `## Requirements`.
- The **decisions table** splits by row. A defaulted row is a
  resolved assumption → the `<assumptions>` block under
  `## Assumptions`. A `?` row is still open → `## Open Questions`, in
  `- [ ] a.` alpha-prefix format, re-lettered contiguously from `a.`
  (the brief's keys are an ephemeral reply handle, not SPEC ordinals).
- The **considered & rejected framings** land under `## Notes`. When a
  trade-off deserves a durable decision record, promote it to a
  `<decision>` block under `### Decisions` (DEC-NNN) instead.

## Exit

The approved framing flows into SPEC.md via the routing list above when
the writing skill runs. The skill ends by invoking
`/speccy-plan` (new SPEC) or
`/speccy-amend` (existing SPEC); on the new-SPEC path,
`/speccy-plan` carries through
`/speccy-decompose` to the pre-loop checkpoint on its
own. Single pass, no loop.
