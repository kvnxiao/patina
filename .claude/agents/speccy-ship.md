---
name: speccy-ship
description: Closes out a Speccy spec — REPORT.md, speccy verify, pull request. Invoke via /agent speccy-ship for the pinned execution path defined in this file's frontmatter.
model: sonnet[1m]
effort: medium
---

# /speccy-ship

Writes `REPORT.md`, runs `speccy verify` as a CI dry-run, and opens
the pull request.

## When to use

After `/speccy-review` has flipped every task to
`state="completed"` and `/speccy-vet` has passed.
Confirm the spec state first:

```bash
speccy next SPEC-NNNN --json
```

Readiness semantics:

- `next_action.kind == "ship"` (exit 0) is the ship-readiness
  signal — all tasks completed, vet gate passing, no REPORT.md
  yet. Proceed.
- `next_action: null` paired with **non-zero exit** is the
  terminal-already-shipped signal — REPORT.md is present, the
  SPEC has already shipped. Stop; do not re-ship. Run
  `speccy archive SPEC-NNNN` if it should be moved out of the
  active tree.
- Any other `next_action.kind` (`work`, `review`, `vet`,
  `decompose`) means tasks remain — pick up
  `/speccy-work`, `/speccy-review`,
  or `/speccy-vet` first.

## Steps

1. The `speccy next SPEC-NNNN --json` already run in "When to use"
   confirms ship-readiness (`next_action.kind == "ship"`, exit 0) and
   carries the `spec_md_path` and `tasks_md_path` fields that locate
   the files — no separate `speccy status` call is needed. If instead
   it returned `next_action: null` with a non-zero exit, REPORT.md
   already exists and the SPEC has already shipped; do not proceed.

2. Write `.speccy/specs/NNNN-slug/REPORT.md` with frontmatter
   (`spec`, `outcome`, `generated_at`), a `<report>` root element
   wrapping one `<coverage req="REQ-NNN"
   result="satisfied|partial|deferred" scenarios="CHK-NNN...">`
   element per surviving SPEC requirement, retry counts, and any
   out-of-scope items implementers absorbed.

   Canonical REPORT.md shape: `.claude/skills/speccy-ship/references/report.md`.

3. Ship-time memory retro. With REPORT.md written and before the ship
   commit (step 6), distill the just-completed loop into mutations of
   the repo's loop ledger at `.speccy/MEMORY.md`. Mine the evidence
   already on disk — REPORT.md coverage, the per-task journal
   (`<blockers>` directives, review verdict flips, retry rounds), and
   the spec diff — rather than re-deriving the work from scratch.

   Resolve the diff baseline rather than hardcoding `main`, so a repo
   whose default branch is `master` or `trunk` still works:

   ```bash
   git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's@^refs/remotes/origin/@@'
   ```

   Use the output as `<base-ref>`; if empty (no remote, detached
   HEAD), fall back to `main`. Read the spec diff with the single-arg
   `git diff <base-ref>`, **not** `git diff <base-ref>...HEAD`: the
   retro runs at step 3, before the step-6 ship commit, while the
   loop's per-task work is still uncommitted in the working tree (the
   work phase never commits per task). The single-arg form diffs the
   working tree against the ref and so captures that uncommitted work;
   the `<base-ref>...HEAD` form compares the merge-base against
   committed HEAD and silently misses it, handing the retro a stale or
   empty diff exactly when it must mine the just-completed loop. The
   entry shape you write here is defined once at:

   ## Memory ledger entry shape

The repo's loop memory lives at `.speccy/MEMORY.md` — a user-owned,
git-tracked file. `speccy init` never
enumerates or overwrites it, so a `--force` reeject leaves it byte-identical
and learned content survives speccy CLI updates. Its **absence is normal and
silent**: a missing or malformed ledger produces no `speccy verify` error or
warning, and the implementer simply has no slice to load.

### The four-part entry shape

Every entry — whether it records a convention the loop followed or a mistake it
made — carries the same four parts. Convention-flavoured and mistake-flavoured
entries differ only by which feed produced them, never in shape:

- **Trigger** — when the entry applies: a task area, a file region, or a
  recurring situation. This is what a future implementer matches against to
  decide the entry is relevant to the slice in front of them.
- **Convention or mistake** — the thing observed: the convention that was
  followed, or the mistake that was made.
- **Corrective rule** — the actionable instruction to follow next time, stated
  so the implementer can act on it without re-deriving the context.
- **Provenance** — the SPEC / task / review that produced the entry, named by
  real identifier so the entry is auditable back to its source.

### Authoring discipline

- **Prefer abstract, convention-level wording over fragile code coordinates.**
  An entry phrased as a durable convention survives a refactor that moves or
  renames the construct it came from; an entry pinned to a specific function,
  line, or module name becomes a phantom reference the moment that construct
  changes. Write the rule, not the address.

- **Provenance must resolve to a real SPEC / task / review identifier**, never
  a fabricated one. Dangling SPEC/task provenance is the only structurally
  checkable slice of ledger hygiene; the rest is a semantic judgment the
  ship-time retro owns, deliberately not a CLI freshness check. Keeping
  provenance honest at authoring time is what makes that future check possible.

### Worked example

The placeholders below are illustrative — substitute your own values.

```markdown
- Trigger: implementing a new CLI subcommand that parses a bounded numeric
  flag.
- Convention: bounded numeric flags are validated with a range value parser at
  the argument layer, not with an ad-hoc check inside the command body.
- Corrective rule: reach for the existing range-value-parser helper before
  writing a fresh bounds check; keep validation at the parse boundary.
- Provenance: SPEC-0042 / T-001 (0042-widget-render-timeout), reviewer-style pass.
```


   The retro does three things in one pass:

   - **Capture (both feeds).** Append convention-flavoured and/or
     mistake-flavoured entries to `.speccy/MEMORY.md` in the four-part
     shape, **one entry per write** so the prose-layer append stays
     serial. A loop with recorded friction — a blocking-then-passed
     review round, a retry round, a `<blockers>` directive — yields at
     least one mistake-flavoured entry whose provenance cites that
     evidence and whose corrective rule addresses the cause. A clean,
     frictionless loop with no durable lesson records that explicitly
     ("no durable lesson this loop") rather than inventing one. Create
     the file if it does not yet exist.

   - **Consolidate and dedupe (human-gated).** Propose promoting stable,
     repeatedly-affirmed entries up into the durable tier (`AGENTS.md`
     / rules) and surface each promotion for **human approval** — never
     promote silently or automatically. On approval, make the
     durable-tier edit and **remove the promoted entry from
     `.speccy/MEMORY.md`** so it is not stored in both tiers. Dedupe
     candidates within the ledger and against the repo's existing
     durable docs (`AGENTS.md`, rule files, anything they point at):
     drop a candidate already covered there rather than appending it.

   - **Phantom-reference GC.** Re-validate existing ledger entries
     against the current tree and retire or rewrite any whose
     referenced construct no longer resolves, so the ledger never
     feeds a phantom forward to the next implementer. Abstractly-worded
     convention entries that name no specific construct survive a
     refactor unchanged; entries pinned to a now-gone module or symbol
     are retired or reworded to the surviving convention. This is a
     semantic judgment plus the abstract-authoring discipline the entry
     reference describes — deliberately not a CLI freshness check, and
     no such mechanism is added.

   The resulting `.speccy/MEMORY.md` mutation lands in the same ship
   commit as REPORT.md (step 6), so the lesson and the loop that taught
   it ship together.

   **Mirror the future-spec subset of the deferred section.** Walk the
   REPORT "Deferred / known limitations" entries written at step 2 (the
   `<coverage result="deferred">` rows and any out-of-scope items) and
   ask, per item, "its own future SPEC, or just a limitation of this
   one?". Mirror ONLY the future-spec-worthy subset into
   `.speccy/BACKLOG.md` with provenance `SPEC-NNNN, ship`, per the
   reference below. Items judged local limitations — bug-level caveats,
   small follow-ups — stay in REPORT.md only and are not appended, so
   the backlog stays a high-signal new-spec register. This append lands
   in the same ship commit (step 6).

## Backlog ledger entry shape

The repo's future-spec register lives at `.speccy/BACKLOG.md` — a user-owned,
git-tracked file, sibling to `MEMORY.md` and distinct from it. `speccy init`,
`speccy init --force`, and reeject never create, enumerate, or overwrite it, so
learned content survives speccy CLI updates. Its **absence is normal and
silent**: a missing or malformed file produces no `speccy verify` error or
warning, and the CLI never reads it. The backlog is a flat, unordered list of
candidate specs — ideas worth their own SPEC later, not deferrals within a spec
already in flight.

### When to append a candidate

A producing phase appends an entry here only when it deliberately cuts a piece
of scope worth its OWN later spec — "not this spec, but its own SPEC later."
Self-create the file with the header below (copied verbatim) on first append,
then record the cut in the four-field shape below.

Distinguish the two kinds of cut. A future-spec candidate goes here. A cut that
is merely out of the current spec's scope is a spec-local Non-goal — it belongs
in that SPEC's `## Non-goals`, not the backlog.

### The file header

When the file self-creates on first append, the producing skill copies in this
preamble verbatim so the lifecycle stays legible to the next reader:

```markdown
# Speccy backlog — future-spec candidates

> User-owned, git-tracked, never created or overwritten by `speccy init`,
> `speccy init --force`, or reeject. Absence is normal and silent; the CLI
> never reads this file. Distinct from `MEMORY.md` (durable loop conventions)
> and from spec-local deferred surfaces (`## Non-goals`, deferred decisions,
> deferred coverage): each entry below should become its OWN spec. Promotion
> retires an entry by deletion. See
> `resources/modules/references/backlog-ledger.md` for the entry shape.
```

### The four-field entry shape

Every entry carries the same four fields, one line per field:

- **Title** — the prospective spec named in a phrase.
- **What & why** — what the spec would deliver plus the value it carries: the
  case for building it.
- **Deferred-because** — why it is not being built now: out of the current
  slice, needs infrastructure that does not exist yet, or blocked on some
  named prerequisite.
- **Provenance** — the originating spec and phase that surfaced the candidate,
  e.g. `SPEC-NNNN, ship` or `SPEC-NNNN, plan`, or `manual` for a hand-added
  entry.

### Authoring discipline

- **Terse.** One phrase per field. The backlog is a working list scanned at
  plan time, not a design document; a candidate that needs a paragraph to
  justify wants its own brainstorm, not a longer backlog line.

- **Provenance must resolve to a real spec and phase**, never a fabricated one
  — or `manual` when added by hand. Honest provenance is what lets a reader
  trace a candidate back to the moment it surfaced.

- **Promotion strikes the entry by deletion.** When a candidate becomes its own
  SPEC, delete its line; the promotion trail lives in git history and the new
  SPEC's own provenance. The backlog reads as current candidates only, never a
  tombstone field.

- **Many entries from one spec's loop is a focus smell.** The per-spec add rate
  is itself feedback: a single spec spawning a long tail of backlog entries
  signals the slice was drawn too wide or the work kept discovering adjacent
  scope. This is a signal to weigh, not an enforced threshold — nothing gates
  on it.

### Worked example

The placeholders below are illustrative — substitute your own values.

```markdown
- Title: Cross-repo spec linking.
- What & why: let a SPEC in one repo reference requirements in another so a
  shared contract has one source of truth; removes the copy-paste drift between
  the two repos that share the protocol.
- Deferred-because: needs a cross-repo resolution surface that does not exist
  yet — out of the current single-repo slice.
- Provenance: SPEC-0042, ship.
```


4. Flip the SPEC's frontmatter status. Edit
   `.speccy/specs/NNNN-slug/SPEC.md` and change `status: in-progress`
   to `status: implemented`. The diff that ships in this PR is what
   makes the SPEC implemented, so the status flip belongs in the
   same PR, not in a follow-up. The status flip is hash-neutral
   because `status` is excluded from `spec_hash_at_generation`, so
   TASKS.md does not need a hash refresh and the spec-hash-mismatch
   lint cannot fire — no post-flip re-check is needed.
5. Run the CI gate locally as a dry-run *after* the status flip so
   verify reads the post-ship tree:

   ```bash
   speccy verify
   ```

   → expected: exit 0. A non-zero exit means the proof shape is broken
   (uncovered requirement, malformed task state, parser-rejected journal
   element) — stop and fix before opening the PR.

6. Commit SPEC.md, TASKS.md, REPORT.md, the `.speccy/MEMORY.md`
   mutation and any `.speccy/BACKLOG.md` mutation from the retro
   (step 3), and the code changes from the loop. Then push:

   - If this branch has no open PR yet, open one. Render the PR body
     from the canonical template at
     `.claude/skills/speccy-ship/references/pr-body.md`: fill its
     three placeholders (named `spec-dir`, `summary`, `coverage-rows`
     inside angle-bracket markers in the template) from
     `.speccy/specs/NNNN-slug/SPEC.md`'s `## Summary` prose, the
     `<coverage>` elements in `.speccy/specs/NNNN-slug/REPORT.md`, and
     the spec-dir path itself. Write the rendered markdown to a scratch
     file (e.g. `/tmp/pr-body.md`) and pass it via `--body-file`:

     ```bash
     gh pr create --title "<spec id> <slug>" \
       --body-file /tmp/pr-body.md
     ```

     Do **not** pipe `REPORT.md` inline via shell command substitution
     into the `--body` flag. GitHub does not render the `<report>` and
     `<coverage>` XML wrappers as markdown, so the angle brackets leak
     into the PR page as visible prose; always use `--body-file` with
     the rendered template instead.

     Multi-SPEC fallback: branches that bundle multiple SPECs, or
     carry unrelated precursor commits, fall back to a hand-authored
     PR body. The template can serve as a per-SPEC starting skeleton
     when hand-authoring — render once per SPEC and stitch the
     sections — but this recipe does not prescribe multi-SPEC
     composition.

   - If a PR already exists for this branch (e.g., a long-running
     branch carrying multiple specs), push to update it:

     ```bash
     git push
     ```

   The status flip in step 4 lands in the same PR — no follow-up
   commit needed after merge.

## Exit

REPORT.md is written, the SPEC's frontmatter status is flipped to
`implemented`, the loop's uncommitted work is bundled into one atomic ship
commit, and a PR is opened (or the existing branch PR updated by push).
`speccy verify` passed as the CI dry-run. Single pass, no loop — the SPEC has
shipped; run `speccy archive SPEC-NNNN` if it should leave the active tree.
