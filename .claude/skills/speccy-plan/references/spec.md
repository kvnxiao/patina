# Worked-instance reference: `SPEC.md`

This file shows the canonical shape of a Speccy
`SPEC.md`. The example values describe a small, plausible feature
(adding a `--timeout` flag to a hypothetical `widget render` CLI). The
shape is what matters; the prose is illustration, not load-bearing.

A real `SPEC.md` lives at `.speccy/specs/NNNN-slug/SPEC.md` and is
parsed by `speccy verify` against the `SPC-*`, `REQ-*`, `QST-*` lint
families.

---

```markdown
---
id: SPEC-NNNN
slug: widget-render-timeout
title: Widget render timeout flag — `widget render` accepts `--timeout-ms` and aborts long renders
status: in-progress
created: 2026-05-21
supersedes: []
---

# SPEC-NNNN: Widget render timeout flag — `widget render` accepts `--timeout-ms` and aborts long renders

## Summary

`widget render` today blocks indefinitely when the input graph contains
a cycle no upstream pre-pass caught. A user running the CLI from a shell
script has no signal to break the loop; the process must be killed
externally. This SPEC adds a `--timeout-ms <N>` flag (default 30000,
range 1..=600000) that aborts the render with exit code 124 and a stderr
message naming the elapsed milliseconds when the render exceeds the
budget.

## Goals

<goals>
- `widget render --timeout-ms 500` aborts a render that exceeds 500ms
  with exit code 124 and stderr line
  `widget render: aborted after 500ms (timeout reached)`.
- Default timeout when the flag is omitted is 30000 milliseconds.
- Flag values outside the inclusive range `1..=600000` produce
  argument-parse exit code 2 with a stderr line naming the rejected
  value and the allowed range.
- Successful renders below the timeout exit 0 with no stderr noise
  attributable to the timeout machinery.
</goals>

## Non-goals

<non-goals>
- No per-node timeout. The flag bounds the total wall-clock of the
  render call, not individual node computations.
- No cancellation API exposed in the library crate. The timeout is a
  CLI affordance only; library callers continue to handle their own
  cancellation.
- No retry-on-timeout. A timed-out render exits non-zero and stops.
</non-goals>

## User Stories

<user-stories>
- As a CI script author, I want `widget render` to abort with a
  predictable non-zero exit when a render exceeds a budget, so my
  pipeline times out cleanly rather than hanging the runner.
- As an interactive CLI user, I want the default 30-second timeout to
  protect me from accidentally launching an unbounded render on a
  malformed graph.
</user-stories>

## Requirements

<requirement id="REQ-001">
### REQ-001: `--timeout-ms <N>` flag parsed with `clap` value parser

The CLI accepts `--timeout-ms <N>` where `N` is parsed as `u32`.
Values outside `1..=600000` are rejected at argument-parse time
(`clap` exit code 2) with a stderr line naming the rejected value
and the allowed range.

<done-when>
- `widget render --timeout-ms 0` exits 2; stderr contains
  `--timeout-ms must be between 1 and 600000 (got 0)`.
- `widget render --timeout-ms 600001` exits 2; stderr contains the
  same range message with `got 600001`.
- `widget render --timeout-ms 30000` parses successfully and
  proceeds to render.
- Omitting the flag defaults to 30000.
</done-when>

<behavior>
- Given the CLI is invoked with `--timeout-ms 0`, when argument
  parsing runs, then the parse fails before any render work begins.
- Given the CLI is invoked with no `--timeout-ms`, when argument
  parsing completes, then the effective timeout used by the render
  loop is 30000.
</behavior>

<scenario id="CHK-001">
Given a built `widget` binary at HEAD after this SPEC lands,
when `widget render --timeout-ms 0 fixtures/cycle.gv` runs,
then the process exits 2 and stderr matches
`--timeout-ms must be between 1 and 600000 (got 0)`.
</scenario>

<scenario id="CHK-002">
Given the same binary,
when `widget render fixtures/trivial.gv` runs (no flag),
then the effective timeout used internally is 30000ms; verified by
the existing `render_timeout_observed` unit test reading the
effective value via the `--print-config` debug flag.
</scenario>
</requirement>

<requirement id="REQ-002">
### REQ-002: Render aborts with exit 124 when wall-clock budget exceeded

When the render exceeds the configured timeout, the process exits
124 with stderr line
`widget render: aborted after <N>ms (timeout reached)` where `<N>`
is the budget value, not the actual elapsed time.

<done-when>
- A render exceeding the budget by ≥10ms exits 124 within budget +
  100ms (measured wall-clock).
- The stderr line uses the configured budget value, not actual
  elapsed milliseconds (deterministic message regardless of
  scheduler jitter).
- No partial output is written to stdout on timeout (the process
  aborts before any successful render bytes flush).
</done-when>

<behavior>
- Given a cyclic graph fixture and `--timeout-ms 500`, when
  `widget render` runs, then the process exits 124 and the stderr
  line names `500ms`.
- Given a trivial acyclic fixture and `--timeout-ms 60000`, when
  `widget render` runs, then the process exits 0 and the stderr
  carries no timeout message.
</behavior>

<scenario id="CHK-003">
Given a built `widget` binary at HEAD,
when `widget render --timeout-ms 500 fixtures/cycle.gv` runs,
then the process exits 124, stderr contains
`widget render: aborted after 500ms (timeout reached)`, and the
wall-clock elapsed is between 500ms and 600ms inclusive.
</scenario>

<scenario id="CHK-004">
Given the same binary,
when `widget render --timeout-ms 60000 fixtures/trivial.gv` runs,
then the process exits 0 and stderr contains no substring
`aborted after`.
</scenario>
</requirement>

## Decisions

<decision id="DEC-001">
Exit code 124 (matching GNU `timeout(1)`) is preferred over a
project-specific code so that shell pipelines composing `widget` with
other tools observe a predictable timeout signal.
</decision>

<decision id="DEC-002">
The stderr line uses the configured budget value rather than actual
elapsed time so that integration tests can grep for an exact string
without flake-prone wall-clock arithmetic.
</decision>

## Notes

The 600000ms (10-minute) upper bound is a defensive guardrail against
typo'd values like `--timeout-ms 3000000`. If a real workload needs a
longer budget, raising the cap is a follow-up SPEC, not a config
knob.
```

---

## Shape invariants the lint suite enforces

- YAML frontmatter declares `id`, `slug`, `title`, `status`, `created`.
- `## Summary` opens the body with prose context.
- `<goals>` and `<non-goals>` are bullet-list blocks under their
  respective headings.
- Each `<requirement id="REQ-NNN">` carries `### REQ-NNN: <one-line
  title>`, then prose, then `<done-when>`, then `<behavior>`, then
  one or more `<scenario id="CHK-NNN">` blocks.
- Scenario blocks use Given/When/Then prose; the `id` attribute is
  the proof handle `speccy check` and `REPORT.md` reference.
- Unresolved placeholder substrings (the conventional unfinished-draft
  markers and angle-bracket ellipses) do not appear anywhere in a real
  SPEC.md; example values are concrete and load-bearing.
