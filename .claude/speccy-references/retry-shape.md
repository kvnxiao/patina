## Rule statement

> `T-NNN` is in **retry shape** at `<spec-dir>` iff
> `<spec-dir>/journal/T-NNN.md` exists, contains at least one
> `<implementer>` element block, and contains at least one
> `<blockers>` element block whose `round` attribute equals the
> highest `round` attribute on any `<implementer>` block in the
> file. Otherwise `T-NNN` is in **first-attempt shape**.

## Read-only scope

The rule reads only the resolved journal for `T-NNN`, through
`speccy journal show` — not by hand-parsing the journal file with
file-editing tools. It does not read TASKS.md, does not invoke `git`,
and does not call `speccy next`. Detection is mechanical: read the
journal's blocks via the CLI and compare `round` attributes across
block types.

Read the latest implementer round and the blockers attached to it:

```bash
# Highest implementer round present (and its blocks).
speccy journal show SPEC-NNNN/T-NNN --json --block implementer --round latest
# Blockers attached to that same latest round, if any.
speccy journal show SPEC-NNNN/T-NNN --json --block blockers --round latest
```

`journal show` exits non-zero when the journal file is absent; that
absence is itself **first-attempt shape** (the rule's existence
conjunct fails — see Worked example 2). When the file exists, the
task is in retry shape iff the latest implementer round has a
`<blockers>` block at that same round, and first-attempt shape
otherwise. The CLI is the read-back authority for the `round`
comparison; do not re-implement the grammar by parsing the file
directly.

## Worked example 1 — retry shape

```
<implementer round="1" date="2026-05-26T18:00:00Z" model="claude-opus-4-8[1m]/low">
... first-pass implementer body ...
</implementer>

<review persona="style" verdict="blocking" round="1" ...>
... style persona feedback ...
</review>

<blockers round="1" ...>
Style: drop the `println!` short-circuit in `reporter.rs`.
</blockers>
```

Applying the rule: the journal contains one `<implementer>` block
(highest `round="1"`) and a `<blockers round="1">` block whose
`round` attribute equals that highest implementer round. The
result is **retry shape**. The dirty tree from the round-1
implementer is the WIP the round-2 implementer amends in place.

## Worked example 2 — first-attempt shape

```
<implementer round="1" date="2026-05-26T18:00:00Z" model="claude-opus-4-8[1m]/low">
... first-pass implementer body ...
</implementer>
```

Applying the rule: the journal contains one `<implementer>` block
and no `<blockers>` blocks. The result is **first-attempt shape**.
The strict clean-tree gate applies — a non-empty
`git status --porcelain` halts the calling skill with the
dirty-paths surface.

A journal file that does not exist on disk also yields
**first-attempt shape** (the rule's first conjunct fails). The
strict clean-tree gate applies the same way.

## Edge case — implementer awaiting review

```
<implementer round="1" ...>...</implementer>
<review persona="style" verdict="blocking" round="1" ...>...</review>
<blockers round="1" ...>...</blockers>

<implementer round="2" ...>...</implementer>
<review persona="business" verdict="blocking" round="2" ...>...</review>
<blockers round="2" ...>...</blockers>

<implementer round="3" ...>... round-3 pass, awaiting review ...</implementer>
```

Applying the rule: the highest implementer-block round in this
journal is `3`, but no `<blockers round="3">` block
exists (the round-3 reviewer fan-out has not yet fired). The
result is **first-attempt shape** — the task is awaiting review,
not awaiting a retry. The strict clean-tree gate applies; if the
round-3 implementer's WIP is still in the tree, the calling skill
halts.
