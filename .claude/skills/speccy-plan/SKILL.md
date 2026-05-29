---
name: speccy-plan
description: 'Draft a new Speccy SPEC from the `AGENTS.md` product north star. Use when the user wants to "write a spec", "draft a SPEC", "spec out X", or "plan a new feature with speccy". Requires: `.speccy/` and `AGENTS.md`. If `.speccy/` is absent → prefer speccy-init. Do NOT trigger on fuzzy asks lacking concrete scope — prefer speccy-brainstorm first to atomize the ask.'
---

# /speccy-plan

Drafts a new `SPEC.md` from the `AGENTS.md` product north star. The
host harness auto-loads `AGENTS.md` (which carries the project-wide
product north star); this recipe walks the agent through writing
SPEC.md. Top-level intent surfaces (`<goals>`, `<non-goals>`,
`<user-stories>`, optional `<assumptions>`) and per-requirement
sub-sections (`<done-when>`, `<behavior>`, `<scenario>`) live as raw
XML element blocks inside SPEC.md itself.

## When to use

When starting a new spec slice. If the ask is still fuzzy, run
`/speccy-brainstorm` first to atomize the intent —
this skill writes SPEC.md in a single pass and assumes the framing
is already agreed.

## What to consider

- Bounded scope. One SPEC must answer one product question; refuse
  to bundle unrelated work. If the scope is too large to be tested
  end-to-end within one PR, split it.
- Decisions hidden inside requirement prose belong in `### Decisions`
  instead. Keep `<requirement>` bodies focused on observable behaviour
  and lift any architectural commitment into a `### Decisions` block.

## Steps

1. Query the next available ID:

   ```bash
   speccy vacancy --json
   ```

   The JSON's `next_spec_id` field is the allocated `SPEC-NNNN` ID.
   Decide placement: flat (`.speccy/specs/NNNN-slug/`) or under an
   existing mission folder (`.speccy/specs/[focus]/NNNN-slug/`).
   Do not invent a new mission folder for a single spec.

2. Write SPEC.md following the PRD template.

   When the slice touches existing code, invoke the `plan-explorer`
   subagent before/while drafting to trace the relevant feature
   through its entry points, call flows, and architecture layers.
   Fold its grounding into the `## Summary` prose and the
   `<requirement>` blocks. The explorer's report is **ephemeral**: do
   NOT persist it to a new `*.md` artifact file — its only durable home
   is the existing SPEC.md sections above.

   Canonical SPEC.md shape: `references/spec.md`.

   If the brainstorm output
   contains collapsed requirements (one requirement with an enumerated
   sub-list), you MAY expand each sub-bullet to its own atomic
   `<requirement>` block (when atomicity adds reviewer-fan-out value)
   or keep them grouped under one `<requirement>` with a `<done-when>`
   bullet list (when cohesive grouping serves the SPEC better). Agent
   discretion; neither choice is surfaced as a self-review issue.

3. **Self-review pass.** Run this pass exactly once after writing
   SPEC.md. Do not re-check after applying fixes.

   **Mechanical/semantic split.** Mechanical issues are
   string-matchable from the SPEC.md text: `TBD`/`TODO` strings,
   "and"/"also" inside `<requirement>` blocks, untouched `<...>`
   template placeholders, missing alpha-prefix ordinals in
   `## Open Questions`. Fix mechanical issues inline by editing
   SPEC.md — do not write anything to `## Open Questions` or to
   chat. If judging requires reading semantics, it is semantic.

   Semantic issues surface as a row appended to `## Open Questions`
   using this fixed template string verbatim:

   `- [ ] {ordinal}. **Self-review caught:** {issue}`

   where `{ordinal}` is the next free alpha-prefix letter continuing
   any existing sequence, and `{issue}` is a one-line description of
   the problem. Do not substitute freeform prose.

   **The six check properties:**

   - **Routing fidelity.** Brainstorm artifacts landed in the
     correct SPEC.md sections: restated ask → Summary +
     Requirements; assumptions → `<assumptions>`; open questions →
     `## Open Questions`; rejected framings → `## Notes` or
     `<decision>` blocks. This check applies only when brainstorm
     ran for this SPEC. When brainstorm was skipped, scope-traces
     alone covers the equivalent verification against the user's
     stated ask.

   - **Atomization.** No `<requirement>` body contains "and"/"also"
     multi-outcome wording that implies two distinct verifiable
     outcomes in one requirement. A requirement that bundles two
     outcomes should be split.

   - **Scope-traces.** Every `<requirement>` traces to a brainstorm
     artifact or to the user's explicitly stated ask. Requirements
     that appeared without a visible source in the approved framing
     are scope creep.

   - **Internal consistency.** No contradictions exist across the
     goals, non-goals, requirements, and assumptions sections. A
     goal that a non-goal denies, or a requirement that violates an
     assumption, is an internal contradiction.

   - **Placeholder leakage.** No `TBD`, `TODO`, or untouched
     `<...>` template-placeholder strings remain in SPEC.md.
     These are mechanical and should be fixed inline, not surfaced.

   - **Ambiguity.** No `<requirement>` wording is interpretable in
     two materially different ways that would lead to different
     implementations. If the requirement is ambiguous, surface it
     as a semantic issue.

   <!-- Note: the plan self-review above is an independent copy.
        The parallel copy for amend lives in speccy-amend.md. -->

4. Surface any material questions inline in `## Open Questions` using
   the alpha-prefix format: `- [ ] a.`, `- [ ] b.`, ..., `- [ ] z.`.
   Each question gets the next free letter in sequence. If the section
   already exists, preserve existing ordinals and allocate the next free
   letter for any new question added (no renumbering). Reaching `z.`
   signals an over-scoped session — 26 open questions is a scope smell,
   not a format limitation.

5. Suggest the next step: `/speccy-decompose SPEC-NNNN` to
   decompose into `TASKS.md`.

This recipe does not loop.
