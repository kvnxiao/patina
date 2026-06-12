# Evidence for SPEC-NNNN T-001

This file shows the canonical shape of an evidence
paper-trail file. A real evidence file lives at
`.speccy/specs/NNNN-slug/evidence/T-NNN.md` (sibling of `SPEC.md`
and `TASKS.md`) and captures the red-then-green pairs that prove
the task's behaviour changed in the expected direction.

The example continues the SPEC-NNNN widget-render-timeout scenario
that runs through the other canonical reference files (`spec.md`,
`tasks.md`, and the sibling `journal-implementer.md`).

## Coverage rule

The per-task journal `<implementer>` block's `Evidence:` field
carries a CHK-by-CHK roll call (see the sibling
`journal-implementer.md`); this file backs the entries
labelled `demonstrated` there. Every `demonstrated` label must be
backed by exactly one `### Scenario N` block in this file; cite the
scenario heading verbatim in the journal roll call so the reviewer
can navigate without grepping. Conversely, every scenario block
here should be cited by a `demonstrated` label in the journal --
orphan scenarios suggest either a stale evidence file or a missing
journal entry.

CHKs labelled `hygiene` or `judgment-only` in the journal do not
appear as `### Scenario N` blocks in this file. The hygiene-gate
evidence at the bottom of this file is the catch-all proof for
`hygiene` CHKs; `judgment-only` CHKs have no execution-time
artifact -- they live entirely in the persona-review judgment
surface.

## Session 2026-05-21-T001-attempt-1

Red-then-green paper trail for T-001: `--timeout-ms` flag added to
`widget render` with bounded range parser and exit-124 abort path.

### Scenario 1 — Cycle fixture aborts at the configured budget (CHK-003)

<red>
Pre-edit: `widget render --timeout-ms 500 fixtures/cycle.gv` against
the binary built from `git show HEAD~1` hangs indefinitely (killed
manually after 60s wall-clock; no exit code observed because the
process was SIGKILLed). The pre-edit binary carries no
`--timeout-ms` flag at all, so `clap` would have rejected the
argument; the run instead used the workaround
`timeout 60 widget render fixtures/cycle.gv` which exited 124 from
the GNU `timeout(1)` wrapper rather than from `widget`.

```
$ timeout 60 ./target/debug/widget render fixtures/cycle.gv
(60s of no output)
$ echo $?
124
```

The `124` here is `timeout(1)`'s code, not `widget`'s — there is no
stderr line from `widget` itself. This is the pre-edit baseline:
the abort signal is external, not coming from the binary under
test.
</red>

<green>
Post-edit: same fixture and budget, the binary aborts itself at
511ms wall-clock and writes the documented stderr line.

```
$ ./target/debug/widget render --timeout-ms 500 fixtures/cycle.gv
widget render: aborted after 500ms (timeout reached)
$ echo $?
124
$ /usr/bin/time -f "%e" ./target/debug/widget render --timeout-ms 500 fixtures/cycle.gv 2>&1 | tail -2
widget render: aborted after 500ms (timeout reached)
0.51
```

Wall-clock 0.51s is inside the SPEC's 500..600ms acceptance window;
exit code 124 matches REQ-002; the stderr line is byte-identical
to the SPEC-mandated form. Repeated 20 times via
`for i in $(seq 1 20); do ...; done`; observed elapsed values
ranged 503..519ms (mean 511, stddev 5).
</green>

### Scenario 2 — Range parser rejects out-of-bounds values (CHK-001)

<red>
Pre-edit: the flag does not exist; `widget render --timeout-ms 0`
exits 2 from `clap`'s unknown-argument handler, but the stderr
message is the generic
`error: unexpected argument '--timeout-ms' found` — not the
SPEC-mandated message naming the rejected value and allowed range.

```
$ ./target/debug/widget render --timeout-ms 0 fixtures/cycle.gv
error: unexpected argument '--timeout-ms' found
$ echo $?
2
```
</red>

<green>
Post-edit: the flag exists with a range value parser; out-of-bounds
values are rejected with the SPEC-mandated message.

```
$ ./target/debug/widget render --timeout-ms 0 fixtures/cycle.gv
error: invalid value '0' for '--timeout-ms <MS>': --timeout-ms must be between 1 and 600000 (got 0)
$ echo $?
2
$ ./target/debug/widget render --timeout-ms 600001 fixtures/cycle.gv
error: invalid value '600001' for '--timeout-ms <MS>': --timeout-ms must be between 1 and 600000 (got 600001)
$ echo $?
2
```

Both rejections carry the documented stderr message naming the
rejected value and the allowed range.
</green>

### Scenario 3 — Trivial fixture renders successfully under budget (CHK-004)

<red>
Pre-edit: the flag does not exist, so this scenario cannot run
against the pre-edit binary. Baseline observed via the binary
built from `git show HEAD~1` is `cargo run -- render
fixtures/trivial.gv` exiting 0 in 0.04s with no stderr — establishing
that the trivial fixture itself does not time out and any post-edit
timeout regression would be a behavioural change, not an inherent
fixture property.

```
$ ./target/debug/widget render fixtures/trivial.gv
(stdout: rendered widget bytes)
$ echo $?
0
```
</red>

<green>
Post-edit: same trivial fixture with a generous budget, the binary
exits 0 without writing any timeout-attributable stderr.

```
$ ./target/debug/widget render --timeout-ms 60000 fixtures/trivial.gv
(stdout: rendered widget bytes — identical bytes to the pre-edit baseline)
$ echo $?
0
$ ./target/debug/widget render --timeout-ms 60000 fixtures/trivial.gv 2>/tmp/stderr; cat /tmp/stderr
(empty)
```

Stderr is empty; the post-edit binary produces no timeout-related
output on a successful render, confirming the timeout machinery
does not leak noise into the happy path.
</green>

## Hygiene-gate evidence

```
$ cargo test --workspace
test result: ok. 142 passed; 0 failed; 0 ignored
$ echo $?
0

$ cargo clippy --workspace --all-targets --all-features -- -D warnings
(no warnings)
$ echo $?
0

$ cargo +nightly fmt --all --check
$ echo $?
0

$ cargo deny check
$ echo $?
0
```

All four standard hygiene gates exited 0 after the T-001 diff
landed.
