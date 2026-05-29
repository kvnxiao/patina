---
name: speccy-amend
description: 'Orchestrate a mid-loop SPEC change — surgically edit SPEC.md with a Changelog row, reconcile TASKS.md, and re-record the spec hash so the SPEC and task list are back in sync. Use when the user says "amend SPEC-NNNN", "the requirements shifted", or when speccy reports the SPEC and tasks are out of sync. Requires: an existing `SPEC.md`. If no `SPEC.md` exists yet → prefer speccy-plan to draft one. Do NOT trigger for cosmetic edits to SPEC.md that do not change Requirements — direct edits are fine.'
---

# /speccy-amend

Orchestrates a mid-loop SPEC change. Edits SPEC.md surgically via the
plan-amend prompt, then reconciles TASKS.md via the tasks-amend prompt,
then re-records the spec hash.

## When to use

When `speccy status` reports `TSK-003: spec hash mismatch`, or when the
user signals that intent has shifted mid-loop. Use this rather than
manually editing SPEC.md so that the Changelog row and TASKS.md
reconciliation are not forgotten.

## Steps

1. Read the existing SPEC's current location and state:

   ```bash
   speccy status SPEC-NNNN --json
   ```

   The JSON's `spec_md_path` field names the SPEC.md file to edit.
2. Edit SPEC.md surgically (including its `<requirement>` /
   `<scenario>` element blocks if requirements changed); append a
   `## Changelog` row explaining *why* the amendment was needed.
   If editing `## Open Questions`, use the alpha-prefix format:
   `- [ ] a.`, `- [ ] b.`, ..., `- [ ] z.`. Preserve existing ordinals
   (do not renumber on amend); allocate the next free letter when
   appending a new question. Reaching `z.` signals an over-scoped
   session — 26 open questions is a scope smell, not a format
   limitation.
3. **Self-review pass.** Run this pass exactly once after writing the
   SPEC.md diff and appending the Changelog row. Do not re-check after
   applying fixes.

   <!-- Note: the amend self-review below is an independent copy.
        The parallel copy for plan lives in speccy-plan.md (per DEC-001 /
        OQ-b: two independent copies, no shared partial). -->

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

   **The eight check properties:**

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

   - **Changelog row presence.** The `## Changelog` section contains
     a new row explaining *why* this amendment was needed. A missing
     or empty Changelog entry is a mechanical issue; fix it inline
     before handing off to TASKS.md reconciliation.

   - **Surgical-diff shape.** Only the requirements and sections
     directly affected by the triggering intent shift were edited.
     A diff that rewrites unrelated requirements, re-words stable
     prose, or restructures sections that the amendment did not
     touch is out-of-scope and should be reverted inline.

4. Reconcile TASKS.md: preserve `state="completed"` tasks unless the
   SPEC change invalidated them (those flip their `state` back to
   `pending`, and the amendment appends a
   `<blockers date="..." round="N">spec amended; ...</blockers>`
   element to `.speccy/specs/NNNN-slug/journal/T-NNN.md` — the
   per-task journal file sibling to `SPEC.md` and `TASKS.md` — rather
   than into the `<task>` body in TASKS.md, which now unconditionally
   rejects journal elements); add new `<task>` elements for newly
   added requirements; remove `<task>` elements for dropped
   requirements.

   The `<blockers>` element has two required attributes: `date` (the
   amendment timestamp, ISO 8601 UTC) and `round`. Pick the `round`
   value by reading the existing journal:

   - If the task already has a journal file with rounds up to N
     (i.e. the highest `round="N"` across its existing
     `<implementer>` / `<review>` / `<blockers>` blocks), use
     `round="N"` matching the round of the implementer attempt the
     amend invalidates. The next implementer attempt will continue
     at round `N+1` when it writes its own `<implementer>` block.
   - If the task has no prior journal file (it was completed in a
     single round without prior blockers and the journal was never
     created, or the journal exists but has no rounds yet), use
     `round="1"`. If the journal file does not exist, create it
     with the standard frontmatter (`spec`, `task`, `generated_at`)
     before appending. In practice this branch is unreachable for
     amendment-driven blockers — a task being amend-invalidated is
     already `state="completed"` and therefore has ≥1 prior
     `<implementer>` round in its journal — so the `round="1"`
     fallback is informational.

   Canonical journal `<blockers>` shape: `.claude/speccy-references/journal-blockers.md`.

   The `<blockers>` body is an amendment-driven blocker directive:
   name what changed in SPEC and what the next implementer attempt
   must address. The `completed` → `pending` state flip on the
   affected task is part of the same amendment turn.
5. Record the new spec hash:

   ```bash
   speccy lock SPEC-NNNN
   ```

6. Re-run `speccy status` to confirm `TSK-003` cleared.

### Loop exit criteria

This recipe is a single pass, not a loop -- but step 6 is the gate. If
the lint still fires, repeat from step 1 (something was missed).

Suggest the next step: `/speccy-work SPEC-NNNN` to pick up any tasks
that flipped back to `state="pending"`.
