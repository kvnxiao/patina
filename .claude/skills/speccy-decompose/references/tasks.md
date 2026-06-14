# Worked-instance reference: `TASKS.md`

Canonical shape of a Speccy `TASKS.md`, continuing the `SPEC-0042`
widget-render-timeout worked instance from `spec.md` in this directory.
Illustrative example — substitute your own ids and values.

A real `TASKS.md` lives at `.speccy/specs/NNNN-slug/TASKS.md` and is
parsed by `speccy verify` against the `TSK-*` lint family.

---

```markdown
---
spec: SPEC-0042
spec_hash_at_generation: a1b2c3d4e5f60718293a4b5c6d7e8f9001122334455667788990aabbccddeeff
generated_at: 2026-05-21T19:30:00Z
---
# Tasks: SPEC-0042 Widget render timeout flag — `widget render` accepts `--timeout-ms` and aborts long renders

<task id="T-001" state="pending" covers="REQ-001 REQ-002">
## Add `--timeout-ms` flag and wire it into the render entrypoint

Extend the `clap`-derived CLI struct in `widget-cli/src/args.rs` with
a `timeout_ms: u32` field carrying a range value parser
(`clap::value_parser!(u32).range(1..=600000)`) and a default of
`30000`. Thread the value into `widget-core::render::Renderer::new`
as a `Duration`. Inside the render loop, poll the elapsed wall-clock
against the budget once per iteration; on overshoot, return
`RenderError::TimedOut { budget_ms }` from `Renderer::render` and
have the CLI map that variant to exit code 124 with the stderr line
`widget render: aborted after <budget_ms>ms (timeout reached)`.

Add unit tests for the value parser (rejects 0 and 600001, accepts
1 / 30000 / 600000) and an integration test driving `widget render`
against `fixtures/cycle.gv` with `--timeout-ms 500` asserting exit
code 124 and the stderr line.

<task-scenarios>
Given the binary built at HEAD after this task,
when `widget render --timeout-ms 0 fixtures/cycle.gv` runs,
then the process exits 2 with stderr matching
`--timeout-ms must be between 1 and 600000 (got 0)`.

Given the same binary,
when `widget render --timeout-ms 500 fixtures/cycle.gv` runs,
then the process exits 124, stderr contains
`widget render: aborted after 500ms (timeout reached)`, and the
wall-clock elapsed is between 500ms and 600ms inclusive.

Given the same binary,
when `widget render --timeout-ms 60000 fixtures/trivial.gv` runs,
then the process exits 0 and stderr is empty.

Suggested files: `widget-cli/src/args.rs`,
`widget-cli/src/main.rs`, `widget-core/src/render.rs`,
`widget-core/src/error.rs`, `widget-cli/tests/timeout.rs`
</task-scenarios>
</task>

<task id="T-002" state="pending" covers="REQ-003">
## Document the timeout behaviour in `widget render --help` and the README

Extend the `--timeout-ms` help text in `widget-cli/src/args.rs` to
name the default (30000), the inclusive range (1..=600000), the
exit code on overshoot (124), and the deterministic stderr message
shape. Mirror the help text in the README's "Render command"
section under a new "Timeout" sub-heading.

<task-scenarios>
Given the binary built at HEAD after this task,
when `widget render --help` runs,
then the output contains the substring `default: 30000`, the
substring `1..=600000`, and the substring `exit code 124`.

Given the README at HEAD,
when the "Render command" section is scanned for a "Timeout"
sub-heading,
then the heading is present and its body names the same default,
range, and exit code values as the `--help` output.

Suggested files: `widget-cli/src/args.rs`, `README.md`
</task-scenarios>
</task>
```

---

## Shape invariants the lint suite enforces

- YAML frontmatter declares `spec`, `spec_hash_at_generation`,
  `generated_at`. The hash binds the task decomposition to the SPEC
  revision it was generated from.
- The `# Tasks: SPEC-NNNN ...` heading appears as the first non-empty
  Markdown line after the frontmatter; the heading-matches-SPEC lint
  flags any mismatch.
- Each `<task>` element is a direct child of the document root (no
  outer `<tasks>` wrapper; the parser rejects that element).
- Required `<task>` attributes: `id` (e.g. `T-NNN`), `state` (one of
  `pending`, `in-progress`, `in-review`, `completed`), `covers`
  (space-separated list of `REQ-NNN` ids — the multi-requirement form
  is `covers="REQ-001 REQ-002"` as on `T-001` above, the single-cover
  form is `covers="REQ-003"` as on `T-002`).
- Each `<task>` body opens with `## <one-line task title>`, then
  prose describing the work, then a `<task-scenarios>` block with
  Given/When/Then prose and a `Suggested files:` line naming the
  files the implementer is likely to touch.
- No `<implementer>`, `<review>`, or `<blockers>` elements appear
  in `TASKS.md`; those live in the per-task journal
  file at `.speccy/specs/NNNN-slug/journal/T-NNN.md`. A journal element
  placed in `TASKS.md` is flagged by the misplaced-journal-element lint.
- Unresolved placeholder substrings (the conventional unfinished-draft
  markers and angle-bracket ellipses) do not appear anywhere in a real
  TASKS.md.
