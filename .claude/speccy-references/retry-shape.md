## Rule statement

> `T-NNN` is in **retry shape** at `<spec-dir>` iff
> `<spec-dir>/journal/T-NNN.md` exists, contains at least one
> `<implementer>` element block, and contains at least one
> `<blockers>` element block whose `round` attribute equals the
> highest `round` attribute on any `<implementer>` block in the
> file. Otherwise `T-NNN` is in **first-attempt shape**.

## Read-only scope

The rule reads only `<spec-dir>/journal/T-NNN.md`. It does not read
TASKS.md, does not invoke `git`, does not call `speccy next`, and
does not invoke any other CLI subcommand. Detection is mechanical:
parse the journal's XML elements (using the same closed-set journal
grammar `<implementer>` / `<review>` / `<blockers>` enforced by the
`JNL-*` lint family), read the `round` attributes, compare.

## Worked example 1 — retry shape

```
<implementer round="1" date="2026-05-26T18:00:00Z" model="claude-opus-4.8[1m]/low">
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
<implementer round="1" date="2026-05-26T18:00:00Z" model="claude-opus-4.8[1m]/low">
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
halts. (In practice the round-3 implementer's atomic-commit step
would have already landed its work before the journal entered this
state; this edge case is documented for completeness.)
