# Worked-instance reference: per-task journal `<blockers>` block

This file shows the canonical shape of a `<blockers>`
block inside a per-task journal file. The example shows a synthesised
blocker set that would flip a task back to `state="pending"` after
review.

A real journal file lives at
`.speccy/specs/NNNN-slug/journal/T-NNN.md`. The `<blockers>` block is
written either by `/speccy-review` (when one or more persona verdicts
return `blocking`) or by `/speccy-amend` (when a SPEC amendment
invalidates an already-completed task).

---

## Where `<blockers>` sits in the journal

A `<blockers>` block is appended after all `<review>` blocks of a
round and before the next round's `<implementer>` block. The
`round` attribute names the round the implementer should retry as
(i.e. the round AFTER the one being blocked). Example: a round-1
review fan-out blocks → `<blockers round="2">` → round-2
`<implementer>` retry.

```markdown
<review persona="business" verdict="blocking" model="claude-sonnet-4.6[1m]/medium" date="2026-05-21T20:30:00Z" round="1">
... (reviewer body — see journal-review.md)
</review>

<review persona="tests" verdict="blocking" model="claude-opus-4.7[1m]/low" date="2026-05-21T20:35:00Z" round="1">
... (reviewer body)
</review>

<blockers date="2026-05-21T20:45:00Z" round="2">
Two blockers landed in round 1 (business, tests). Security and style
passed. Fix both before respawning the review fan-out.

1. **Stderr message uses elapsed time, not configured budget
   (business).** The implementer's round-1 patch writes
   `widget render: aborted after 511ms (timeout reached)` where the
   `511` is `Instant::elapsed().as_millis()` of the actual abort,
   not the configured `budget_ms`. DEC-002 in SPEC-NNNN explicitly
   requires the configured-budget form so integration tests can grep
   for an exact string. The fix is to format the message from
   `self.budget.as_millis()` (the stored field), not from the
   elapsed-time local at the call site.

2. **Range parser accepts 600001 due to off-by-one on the upper
   bound (tests).** Reviewer ran
   `cargo run -- render --timeout-ms 600001 fixtures/cycle.gv` and
   observed exit 124 with a 600001ms elapsed wait, not exit 2. The
   `clap` `range` builder in `widget-cli/src/args.rs:42` reads
   `.range(1..600001)` (exclusive upper bound) where REQ-001
   requires `1..=600000` inclusive. Change `.range(1..600001)` to
   `.range(1..=600000)`. Add a regression unit test
   `range_parser_rejects_600001` exercising the exact boundary.

The mechanical floor (`--timeout-ms` flag present, default 30000,
exit 124 on overshoot, `RenderError::TimedOut` variant added) is
clean and reusable for the retry. The semantic half (deterministic
stderr message + correct upper bound) must land for T-001 to
satisfy REQ-001 and REQ-002.
</blockers>

<implementer date="2026-05-21T21:30:00Z" model="claude-opus-4.7[1m]/low" round="2">
... (round-2 implementer body addressing both blockers)
</implementer>
```

---

## Required attributes on `<blockers>`

Both are required; there are no optional attributes.

- `date` — full ISO8601 date-time with seconds and timezone
  designator marking when the orchestrator synthesised the blocker
  set.
- `round` — the round the implementer should retry as (positive
  integer, equal to the round of the blocking reviews plus 1). A
  round-1 blocker carries `round="2"`; a round-2 blocker carries
  `round="3"`.

## Body content conventions

The body is free-form prose with a small structural shape:

- A one-line preamble naming which personas blocked and which
  passed, so the next implementer can scan persona signal at a
  glance.
- A numbered list of blockers. Each blocker carries:
  - A short bold-prefix title naming the failure and (in
    parentheses) the persona(s) that flagged it.
  - The concrete observation: what the reviewer ran or saw, what
    SPEC clause or DEC the observation contradicts, and the
    expected behaviour.
  - The proposed fix at the level of detail a downstream
    implementer can execute against — file paths, function names,
    test names where applicable.
- A closing paragraph distinguishing what is already correct (the
  mechanical floor that should not regress) from what must change
  (the semantic half the retry must land). This keeps the retry
  implementer from accidentally undoing already-good work.

## Amendment-driven blockers

`/speccy-amend` writes a `<blockers>` block when a SPEC amendment
changes a requirement that an already-completed task covered. The
shape is identical; the body prose names the SPEC.md changelog row
and the specific requirement diff that invalidated the prior
completion, plus the fix the next implementer turn must land.
