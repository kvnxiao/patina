# Worked-instance reference: per-task journal `<blockers>` block

Canonical shape of a `<blockers>` block inside a per-task journal file,
continuing the `SPEC-0042` widget-render-timeout worked instance. The block
below is the synthesised round-1 blocker set that flips `T-001` back to
`state="pending"` after review. Illustrative example — substitute your own ids
and values.

A real journal file lives at
`.speccy/specs/NNNN-slug/journal/T-NNN.md`. The orchestrator authors
the `<blockers>` body (it stays orchestrator-authored semantic
judgment) and lands it via `speccy journal append SPEC-NNNN/T-NNN
--block blockers` with the body on stdin — written either by
`/speccy-review` (when one or more persona verdicts return
`blocking`) or by `/speccy-amend` (when a SPEC amendment invalidates
an already-completed task). The CLI stamps `date` and `round`.

---

## Where `<blockers>` sits in the journal

A `<blockers>` block is appended after all `<review>` blocks of a
round and before the next round's `<implementer>` block. The
`round` attribute names the round of the prior `<implementer>`
attempt the blockers describe — the round just blocked by review,
or the round of the prior completed attempt invalidated by an
amendment. Example: a round-1 review fan-out blocks →
`<blockers round="1">` → round-2 `<implementer>` retry.

```markdown
<implementer date="2026-05-21T19:45:00Z" model="claude-opus-4-8[1m]/low" round="1">
... (implementer body — see journal-implementer.md)
</implementer>

<review persona="tests" verdict="blocking" model="claude-opus-4-8[1m]/low" date="2026-05-21T20:35:00Z" round="1">
... (reviewer body — see journal-review.md)
</review>

<review persona="correctness" verdict="blocking" model="claude-opus-4-8[1m]/low" date="2026-05-21T20:38:00Z" round="1">
... (reviewer body)
</review>

<blockers date="2026-05-21T20:45:00Z" round="1">
Two personas blocked in round 1 (tests, correctness); business,
security, and style passed. One root cause — fix it before respawning
the review fan-out.

1. **Range parser rejects the inclusive maximum 600000 (tests,
   correctness).** The round-1 value parser in
   `widget-cli/src/args.rs:42` declares `.range(1..600000)` — an
   exclusive Rust range whose end value 600000 is excluded. Running
   `widget render --timeout-ms 600000 fixtures/trivial.gv` returns clap
   exit 2 with `--timeout-ms must be between 1 and 600000 (got 600000)`,
   where REQ-001's done-when requires 600000 to parse successfully (it
   is the documented inclusive upper bound). Change `.range(1..600000)`
   to `.range(1..=600000)`. Add a regression unit test
   `range_parser_accepts_600000`: the round-1 suite tested 0 / 600001
   rejection and 1 / 30000 acceptance but never the 600000 boundary,
   which is why the off-by-one cleared a green hygiene gate.

The rest of the floor is clean and must not regress on the retry: the
`--timeout-ms` flag is present with default 30000, exit 124 fires on
overshoot via `RenderError::TimedOut`, and the stderr line already
names the configured budget value rather than measured elapsed time
(per DEC-002). Only the upper-bound range literal and its missing
boundary test must change for T-001 to satisfy REQ-001.
</blockers>

<implementer date="2026-05-21T21:30:00Z" model="claude-opus-4-8[1m]/low" round="2">
... (round-2 implementer body addressing the blocker)
</implementer>
```

---

## Attributes on `<blockers>`

Both are present on every block, and both are CLI-stamped — the
orchestrator authors only the body, not these attributes.

- `date` — **CLI-stamped.** Full ISO8601 date-time with seconds and
  timezone designator, set to UTC now at append time. Do not supply
  or compute it.
- `round` — **CLI-derived.** The round of the implementer attempt the
  blockers describe (the round just blocked by review, or the round
  of the prior completed attempt invalidated by amendment); the CLI
  attaches the `<blockers>` to the current round. A round-1 blocker
  carries `round="1"`; a round-2 blocker carries `round="2"`. The
  next `<implementer>` append increments the round by exactly 1.

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
  floor that should not regress) from what must change (what the
  retry must land). This keeps the retry implementer from
  accidentally undoing already-good work.

## Amendment-driven blockers

`/speccy-amend` writes a `<blockers>` block when a SPEC amendment
changes a requirement that an already-completed task covered. The
shape is identical; the body prose names the SPEC.md changelog row
and the specific requirement diff that invalidated the prior
completion, plus the fix the next implementer turn must land.
