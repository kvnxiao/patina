# Worked-instance reference: `REPORT.md`

Canonical shape of a Speccy `REPORT.md`, continuing the `SPEC-0042`
widget-render-timeout worked instance from `spec.md` / `tasks.md` in this
directory. Illustrative example — substitute your own ids and values.

A real `REPORT.md` lives at `.speccy/specs/NNNN-slug/REPORT.md` and is
parsed by `speccy verify` against the `RPT-*` lint family.

---

```markdown
---
spec: SPEC-0042
outcome: implemented
generated_at: 2026-05-22T08:15:00Z
---

# REPORT: SPEC-0042 Widget render timeout flag — `widget render` accepts `--timeout-ms` and aborts long renders

<report spec="SPEC-0042">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002">
T-001 added `timeout_ms: u32` to `widget-cli/src/args.rs` with a
`clap` range value parser and a `default_value = "30000"`. The parser
rejects 0 and 600001 at argument-parse time with the documented stderr
message naming the rejected value and the allowed range, and accepts
the inclusive upper bound 600000. Default behaviour was verified via
the existing `--print-config` debug flag asserting the effective
timeout reads back as 30000 when the flag is omitted. The round-1 patch
used an exclusive `.range(1..600000)` that wrongly rejected the valid
max 600000; reviewer-tests and reviewer-correctness blocked, and round
2 switched to the inclusive `.range(1..=600000)` with a
`range_parser_accepts_600000` regression test. Retry count: 1.
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-003 CHK-004">
T-001 threaded the budget into `widget-core::render::Renderer` as a
`Duration` field; the render loop polls `Instant::elapsed` once per
iteration and returns `RenderError::TimedOut { budget_ms }` on
overshoot. The CLI maps that variant to `std::process::exit(124)`
after writing `widget render: aborted after <budget_ms>ms (timeout
reached)` to stderr — the configured budget, not measured elapsed, per
DEC-002. The integration test against `fixtures/cycle.gv` observed
wall-clock elapsed of 511ms +/- 8ms across 20 runs, inside the SPEC's
500..600ms acceptance window. Successful renders against
`fixtures/trivial.gv` exited 0 with no timeout-attributable stderr
output. Retry count: 0.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-005 CHK-006">
T-002 documented the timeout contract in `widget render --help` (naming
`default: 30000`, the range `1..=600000`, and exit code `124`) and in
the README's "Render command" section under a new "Timeout"
sub-heading. CHK-005 is demonstrated by an integration test asserting
the three `--help` substrings; CHK-006 (the README prose reads clearly
and matches `--help`) is judgment-only and was confirmed by
reviewer-business and reviewer-style on the diff. Retry count: 0.
</coverage>

</report>

## Notes

The 600000ms upper bound surfaced one design question during review:
whether the bound should be a CLI-only guardrail or also enforced in
`widget-core` library callers. DEC-003 in SPEC-0042 settled this as
CLI-only; library callers continue to pass any `Duration` they want.
The integration test suite carries one explicit library-caller test
passing `Duration::from_secs(3600)` to confirm the library layer
does not duplicate the bound.
```

---

## Shape invariants the lint suite enforces

- YAML frontmatter declares `spec`, `outcome`, `generated_at`.
  `outcome` is typically `implemented`, `partial`, or `abandoned`.
- A `# REPORT: SPEC-NNNN ...` heading opens the body.
- The `<report>` root element carries the required `spec="SPEC-NNNN"`
  attribute; the report-root lint flags its absence.
- One `<coverage>` element per requirement in the SPEC, in
  numerical order. Required attributes: `req="REQ-NNN"`,
  `result="<verdict>"`, `scenarios="CHK-NNN CHK-NNN ..."`
  (space-separated list of scenario ids that prove the coverage
  claim).
- `result` is one of `satisfied`, `partial`, `deferred`. A requirement
  dropped by amendment is removed from `SPEC.md` rather than carried as
  a coverage row, so there is no `not-applicable` result.
- Coverage body prose is free-form but ends with a `Retry count:
  <N>` line naming how many implementer rounds the requirement
  took to land (zero on a clean first pass).
- Unresolved placeholder substrings (the conventional unfinished-draft
  markers and angle-bracket ellipses) do not appear anywhere in a real
  REPORT.md.
