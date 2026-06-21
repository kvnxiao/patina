# Evidence for SPEC-0042 T-001

Canonical shape of an evidence paper-trail file. The real file lives at
`.speccy/specs/NNNN-slug/evidence/T-NNN.md` (sibling of `SPEC.md` and
`TASKS.md`) and captures the red-then-green pairs that prove the task's
behaviour changed in the expected direction. This example continues the
`SPEC-0042` widget-render-timeout worked instance from the sibling `spec.md`,
`tasks.md`, and `journal-implementer.md` — illustrative example, substitute
your own ids and values.

## Coverage rule

The journal `<implementer>` block's `Evidence:` field carries a CHK-by-CHK roll
call; this file backs the entries it labels `demonstrated`.

- Every `demonstrated` label ↔ exactly one `### Scenario N` block here. Cite the
  scenario heading verbatim in the roll call so the reviewer navigates without
  grepping.
- An orphan scenario (no `demonstrated` label cites it) signals a stale evidence
  file or a missing journal entry.
- CHKs labeled `hygiene` or `judgment-only` get no scenario block: the
  hygiene-gate block below is the catch-all for `hygiene`, and `judgment-only`
  CHKs have no execution artifact — they live in the persona-review surface.

## Session 2026-05-21-attempt-1

Red-then-green trail for the `--timeout-ms` flag added to `widget render` with a
bounded range parser and an exit-124 abort path. Backs the three `demonstrated`
CHKs under REQ-001 / REQ-002 in T-001's roll call.

### Scenario 1 — Range parser rejects out-of-bounds values (CHK-001)

<red>
Pre-edit the flag is unknown, so `clap` emits the generic
`error: unexpected argument '--timeout-ms' found` (exit 2) — not the
SPEC-mandated message naming the rejected value and the allowed range.
</red>

<green>
Post-edit a range value parser rejects out-of-bounds values with the mandated
message:

```
$ ./target/debug/widget render --timeout-ms 0 fixtures/cycle.gv
error: invalid value '0' for '--timeout-ms <MS>': --timeout-ms must be between 1 and 600000 (got 0)
$ echo $?
2
```
</green>

### Scenario 2 — Cycle fixture aborts at the configured budget (CHK-003)

<red>
Pre-edit the flag does not exist, so the run leaned on the external wrapper
`timeout 60 widget render fixtures/cycle.gv`, which hangs to the wrapper's own
exit 124 — the abort is external, not from `widget`:

```
$ timeout 60 ./target/debug/widget render fixtures/cycle.gv
(60s of no output)
$ echo $?
124
```
</red>

<green>
Post-edit the binary aborts itself at ~511ms and writes the documented stderr
line:

```
$ ./target/debug/widget render --timeout-ms 500 fixtures/cycle.gv
widget render: aborted after 500ms (timeout reached)
$ echo $?
124
```

Wall-clock 0.51s sits inside the SPEC's 500..600ms window; exit 124 matches the
requirement; the stderr line is byte-identical to the SPEC-mandated form.
Repeated 20×: elapsed 503..519ms (mean 511, stddev 5).
</green>

### Scenario 3 — Trivial fixture renders under budget (CHK-004)

<red>
Pre-edit the flag is unknown, so the documented happy-path invocation errors
before any render runs:

```
$ ./target/debug/widget render --timeout-ms 60000 fixtures/trivial.gv
error: unexpected argument '--timeout-ms' found
$ echo $?
2
```
</red>

<green>
Post-edit a render that finishes well under the budget exits 0 with the rendered
output and no timeout-attributable stderr:

```
$ ./target/debug/widget render --timeout-ms 60000 fixtures/trivial.gv >/dev/null
$ echo $?
0
$ ./target/debug/widget render --timeout-ms 60000 fixtures/trivial.gv 2>&1 >/dev/null | grep -c 'aborted after'
0
```

The 60000ms budget is never approached (the trivial graph renders in ~3ms), so
the timeout machinery stays silent on the happy path.
</green>

## Hygiene-gate evidence

Backs CHK-002 (`--timeout-ms` defaults to 30000 when omitted, read back via the
`--print-config` debug flag), proved by the `render_timeout_observed` unit test
in the project's hygiene suite. Every gate the project's `AGENTS.md` declares
exited 0 after the diff landed; record each one and its observed result. For a
project whose `AGENTS.md` declares a test gate and a lint gate:

```
$ <test>   142 passed; 0 failed; 0 ignored
$ <lint>   no warnings
```
