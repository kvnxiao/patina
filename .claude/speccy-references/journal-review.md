# Worked-instance reference: per-task journal `<review>` block

Canonical shape of a `<review>` block inside a per-task journal file,
continuing the `SPEC-0042` widget-render-timeout worked instance from the
sibling reference files in this directory. Illustrative example — substitute
your own ids and values.

A real journal file lives at
`.speccy/specs/NNNN-slug/journal/T-NNN.md`. Each reviewer persona
appends its own `<review>` block via `speccy journal append
SPEC-NNNN/T-NNN --block review --persona <self> --verdict <v> --model
<self>` with its findings on stdin, then returns a thin verdict to
the orchestrator (see `verdict-return-contract.md`). The worked
example below shows what the CLI produces — the persona supplies only
the body, `--persona`, `--verdict`, and `--model`.

---

## Where `<review>` sits in the journal

After an implementer turn flips the task to `state="in-review"`, the
reviewer-orchestrator fans out one persona per reviewer; each persona
appends its own `<review>` block. The CLI's per-file lock serializes
the concurrent appends, so the blocks land without interleaving.

In this worked instance, round-1 review of `T-001` **blocks**:
reviewer-tests and reviewer-correctness both catch an off-by-one (the
value parser rejects the inclusive maximum 600000 that REQ-001 requires
accepted); business, security, and style pass. The orchestrator
synthesises a `<blockers>` block (see `journal-blockers.md`) and flips
the task back to `state="pending"`. The round-2 implementer fixes the
range bound, and the round-2 fan-out passes.

```markdown
---
spec: SPEC-0042
task: T-001
generated_at: 2026-05-21T19:45:00Z
---

<implementer date="2026-05-21T19:45:00Z" model="claude-opus-4-8[1m]/low" round="1">
... (implementer body — see journal-implementer.md)
</implementer>

<review persona="business" verdict="pass" model="claude-sonnet-4-6[1m]/medium" date="2026-05-21T20:30:00Z" round="1">
Scope is clean. The timeout knob stays a CLI affordance — the
`widget-core` library crate gains no timeout parameter (per the SPEC's
non-goal), and no retry-on-timeout behaviour leaked in. Exit code 124
matches DEC-001's GNU `timeout(1)` compatibility goal, and the stderr
line uses the configured budget (per DEC-002) rather than measured
elapsed time, preserving the deterministic-message property the CI
integration test depends on. I leave the range-boundary verification
to reviewer-tests.
</review>

<review persona="tests" verdict="blocking" model="claude-opus-4-8[1m]/low" date="2026-05-21T20:35:00Z" round="1">
Blocking on a coverage gap that hides a requirement violation. The
round-1 unit tests check the range parser rejects 0 and 600001 and
accepts 1 and 30000, but never exercise the inclusive maximum 600000.
Running `widget render --timeout-ms 600000 fixtures/trivial.gv` by hand
returns clap exit 2 (`--timeout-ms must be between 1 and 600000 (got
600000)`), where REQ-001's done-when requires 600000 to parse
successfully. The red-then-green trail in
`.speccy/specs/0042-widget-render-timeout/evidence/T-001.md` is honest
for the three CHK scenarios it backs (CHK-001 / CHK-003 / CHK-004) and
the hygiene gate is green — but the missing boundary case is exactly
the gap the roll call exists to surface. Fix: accept 600000 and add a
regression test pinning the boundary.
</review>

<review persona="correctness" verdict="blocking" model="claude-opus-4-8[1m]/low" date="2026-05-21T20:38:00Z" round="1">
Same root cause, stated as a logic error. The value parser declares
`.range(1..600000)` — an exclusive Rust range whose end value 600000 is
excluded — so the inclusive bound REQ-001 documents (`1..=600000`) is
off by one at the top. The fix is `.range(1..=600000)`. The exit-124
abort path, the `RenderError::TimedOut { budget_ms }` mapping, and the
DEC-002 budget-value stderr formatting are all correct; the defect is
isolated to the range literal in `widget-cli/src/args.rs`.
</review>

<review persona="security" verdict="pass" model="claude-sonnet-4-6[1m]/medium" date="2026-05-21T20:40:00Z" round="1">
No security-relevant changes. The `--timeout-ms` flag accepts only
`u32` values via the `clap` range parser; no arbitrary string
handling, no file-path expansion, no subprocess invocation. The
`RenderError::TimedOut { budget_ms }` variant carries a single `u32`
field with no user-controlled prose, eliminating any log-injection
vector through the error message.
</review>

<review persona="style" verdict="pass" model="claude-sonnet-4-6[1m]/medium" date="2026-05-21T20:42:00Z" round="1">
Diff stays within the suggested files. `RenderArgs::timeout_ms` follows
the existing `clap` field naming convention (snake_case, no `arg_`
prefix). The new `RenderError::TimedOut` variant docstring explains the
`budget_ms` field semantics (configured value, not elapsed). No
`unwrap()` or `expect()` introduced in production code; no `#[allow(...)]`
directives added.
</review>

<blockers date="2026-05-21T20:45:00Z" round="1">
... (synthesised blocker — see journal-blockers.md)
</blockers>

<implementer date="2026-05-21T21:30:00Z" model="claude-opus-4-8[1m]/low" round="2">
... (round-2 fix — see journal-implementer.md)
</implementer>

<review persona="tests" verdict="pass" model="claude-opus-4-8[1m]/low" date="2026-05-21T21:55:00Z" round="2">
Round-2 fix verified. `widget render --timeout-ms 600000` now parses
and proceeds (exit 0 on the trivial fixture); `--timeout-ms 600001`
still exits 2. The new `range_parser_accepts_600000` regression test
pins the boundary, and the rest of the suite is unchanged-green. The
round-1 blocker is resolved; business, security, and style re-passed on
the unchanged surface.
</review>
```

---

## Attributes on `<review>`

All five are present on every block. Three are supplied by the
persona as `journal append` flags; two are CLI-stamped.

- `persona` — supplied as `--persona`. Must be one of the seven
  registered reviewer personas: `business`, `tests`, `security`,
  `style`, `correctness` (the default fan-out) plus the off-default
  `architecture` and `docs`. The CLI enforces this closed set —
  `journal append --block review` rejects any unregistered persona
  name with a hard error.
- `verdict` — supplied as `--verdict`. `pass` or `blocking`; the CLI
  enforces the closed set.
- `model` — supplied as `--model`. The model identity that ran the
  reviewer turn. Same slash-suffix convention as `<implementer>`
  (e.g. `claude-sonnet-4-6[1m]/medium`).
- `date` — **CLI-stamped.** Full ISO8601 date-time with seconds and
  timezone designator, set to UTC now at append time. Do not supply
  or compute it.
- `round` — **CLI-derived.** Matches the `<implementer>` block this
  review evaluates; the CLI attaches the `<review>` to the current
  round. Round-1 reviews evaluate the round-1 implementer; round-2
  reviews evaluate the round-2 implementer. Do not supply or compute
  it.

## Review body content

The body of a `<review>` block is free-form prose from the
persona's point of view, with two soft conventions:

- A `pass` verdict states what the persona checked and what it
  observed; it does not need a long justification, but it should
  name the concrete artifact properties that satisfied the
  persona's contract (so a downstream reader can audit the review
  itself).
- A `blocking` verdict states the blocker concretely: what was
  expected, what was observed, what the persona believes is the
  required fix. The blocker prose then becomes input for the
  orchestrator's `<blockers>` synthesis (see
  `journal-blockers.md`).

## Multiple rounds

When a `blocking` verdict appears in round 1, the orchestrator
synthesises a `<blockers>` block and flips the task back to
`state="pending"`. The next implementer turn lands as
`round="2"`; reviewers fanning out a second time write
`round="2"` `<review>` blocks. Same monotonic-no-skip rule as
`<implementer>`.
