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

3. Flip the SPEC's frontmatter status. Edit
   `.speccy/specs/NNNN-slug/SPEC.md` and change `status: in-progress`
   to `status: implemented`. The diff that ships in this PR is what
   makes the SPEC implemented, so the status flip belongs in the
   same PR, not in a follow-up. The status flip is hash-neutral
   because `status` is excluded from `spec_hash_at_generation`, so
   TASKS.md does not need a hash refresh and `TSK-003` cannot fire —
   no post-flip re-check is needed.
4. Run the CI gate locally as a dry-run *after* the status flip so
   verify reads the post-ship tree:

   ```bash
   speccy verify
   ```

5. Commit SPEC.md, TASKS.md, REPORT.md, and the code changes from the
   loop. Then push:

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

   The status flip in step 3 lands in the same PR — no follow-up
   commit needed after merge.

This recipe does not loop.
