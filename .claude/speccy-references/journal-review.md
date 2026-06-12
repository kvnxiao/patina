# Worked-instance reference: per-task journal `<review>` block

This file shows the canonical shape of a `<review>`
block inside a per-task journal file. The example continues the
SPEC-NNNN widget-render-timeout scenario from the sibling reference
files in this directory.

A real journal file lives at
`.speccy/specs/NNNN-slug/journal/T-NNN.md`. Each reviewer persona
appends its own `<review>` block via `speccy journal append
SPEC-NNNN/T-NNN --block review --persona <self> --verdict <v> --model
<self>` with its findings on stdin, then returns a thin verdict to
the orchestrator (see `verdict_return_contract.md`). The worked
example below shows what the CLI produces — the persona supplies only
the body, `--persona`, `--verdict`, and `--model`.

---

## Where `<review>` sits in the journal

After an implementer turn flips the task to `state="in-review"`, the
reviewer-orchestrator fans out one persona per reviewer; each persona
appends its own `<review>` block. The CLI's per-file lock serializes
the concurrent appends, so the blocks land without interleaving. A
journal file with one round of review looks like:

```markdown
---
spec: SPEC-NNNN
task: T-001
generated_at: 2026-05-21T19:45:00Z
---

<implementer date="2026-05-21T19:45:00Z" model="claude-opus-4-8[1m]/low" round="1">
... (implementer body — see journal-implementer.md)
</implementer>

<review persona="business" verdict="pass" model="claude-sonnet-4-6[1m]/medium" date="2026-05-21T20:30:00Z" round="1">
The `--timeout-ms` flag satisfies REQ-001 and REQ-002 as written.
The 30000ms default and 1..=600000 range match the SPEC's
guardrail values verbatim; the stderr line uses the configured
budget (per DEC-002) rather than measured elapsed time, which
preserves the deterministic-message property the CI integration
test depends on. Exit code 124 matches DEC-001's GNU `timeout(1)`
compatibility goal. No business-scope drift: the library crate
remains free of the timeout knob (per the SPEC's non-goal), and
no retry-on-timeout behaviour leaked in.
</review>

<review persona="tests" verdict="pass" model="claude-opus-4-8[1m]/low" date="2026-05-21T20:35:00Z" round="1">
Red-then-green paper trail in
`.speccy/specs/NNNN-widget-render-timeout/evidence/T-001.md`
records three scenarios with concrete pre-edit and post-edit
command output. Pre-edit baseline for scenario 1 uses GNU
`timeout(1)` as the external abort signal — distinguishing the
124 from the wrapper versus the 124 from the binary — which is
the right shape for "the abort signal moved from external to
internal". Scenario 2 captures both boundary rejections (0 and
600001) with full stderr text. Hygiene gates `cargo test`,
`cargo clippy`, `cargo +nightly fmt --check`, `cargo deny check`
all exited 0 in the recorded run.
</review>

<review persona="security" verdict="pass" model="claude-sonnet-4-6[1m]/medium" date="2026-05-21T20:40:00Z" round="1">
No security-relevant changes. The `--timeout-ms` flag accepts only
`u32` values via the `clap` range parser; no arbitrary string
handling, no file-path expansion, no subprocess invocation. The
new `RenderError::TimedOut { budget_ms }` variant carries a single
`u32` field with no user-controlled prose, eliminating any
log-injection vector through the error message.
</review>

<review persona="style" verdict="pass" model="claude-sonnet-4-6[1m]/medium" date="2026-05-21T20:42:00Z" round="1">
Diff stays within the suggested files. `RenderArgs::timeout_ms`
follows the existing `clap` field naming convention (snake_case,
no `arg_` prefix). The new `RenderError::TimedOut` variant docstring
explains the `budget_ms` field semantics (configured value, not
elapsed). No `unwrap()` or `expect()` introduced in production
code; the budget arithmetic uses `Duration::from_millis` with the
range-parser-bounded input so overflow is unreachable. No
`#[allow(...)]` directives added.
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
