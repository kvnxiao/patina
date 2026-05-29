---
name: Phase implementation
about: Track a phase of an existing SPEC
title: "[SPEC <NNNN>] Phase <N>: <phase title>"
labels: ["phase", "spec-<NNNN>"]
assignees: []
---

## SPEC reference

- **SPEC:** `specs/<NNNN>-<slug>/SPEC.md`
- **Phase:** Phase `<N>` — `<title>`
- **STATUS:** `specs/<NNNN>-<slug>/STATUS.md`

## Goals (copied from SPEC)

<!-- Paste the phase's "Goals" bullets verbatim. -->

## Dependencies

- [ ] Phase X complete
- [ ] Phase Y complete

If any are not yet ✅ Complete, this issue should be 📋 Ready only after they are.

## Deliverables (copied from SPEC)

<!-- Paste the phase's "Deliverables" bullets verbatim. -->

## Validation criteria (copied from SPEC)

<!-- Paste the phase's "Validation Criteria" bullets verbatim. -->

- [ ] ✅ <criterion 1>
- [ ] ✅ <criterion 2>
- [ ] ✅ <criterion 3>

## Implementer notes

<!-- Anything specific to this implementation attempt. Useful when:
     - A phase has been attempted before and rolled back.
     - There are open questions in the SPEC that need resolving first.
     - Manual validation steps need scheduling. -->

## Definition of done

- [ ] All deliverables exist on the branch.
- [ ] All validation criteria pass in CI.
- [ ] Documentation under `docs/` updated.
- [ ] Manual validation checklist (if any) signed off.
- [ ] PR reviewed and merged.
- [ ] STATUS.md updated to ✅ Complete with PR number and date.
