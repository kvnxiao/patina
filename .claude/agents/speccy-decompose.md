---
name: speccy-decompose
description: Decomposes a Speccy SPEC into a checklist of agent-sized tasks. Invoke via /agent speccy-decompose for the pinned execution path defined in this file's frontmatter.
model: sonnet[1m]
effort: medium
---

# /speccy-decompose

Decomposes the SPEC into an ordered, single-agent-sized task list in
`TASKS.md`. If `TASKS.md` already exists, amends it surgically instead
(preserving the state of any task already in flight).

## When to use

- Initial: after `/speccy-plan` lands a fresh SPEC.
- Amendment: after `/speccy-amend SPEC-NNNN` edited an existing SPEC and
  the tasks may now be stale (the CLI surfaces a `TSK-003` lint when
  it detects hash drift).

## Steps

1. Read the spec's current state to locate SPEC.md:

   ```bash
   speccy status SPEC-NNNN --json
   ```

   The JSON's `spec_md_path` field names the SPEC.md to decompose.
   If `tasks_md_path` is non-null, an existing TASKS.md is present
   and this is an amendment run (edit surgically; preserve
   `state="completed"` tasks unless invalidated).
2. Write `TASKS.md` as Markdown with a sequence of
   `<task id="T-NNN" state="pending" covers="REQ-NNN">...</task>`
   block per task directly under the heading — no `<tasks>` wrapper
   element. Each `<task>` body contains a `<task-scenarios>`
   element with slice-level Given/When/Then prose and an optional
   `Suggested files:` bullet.

   The file must open with a YAML frontmatter block followed immediately
   by a level-1 heading.

   Canonical TASKS.md shape: `references/tasks.md`.

   Key constraints:
   - The `# Tasks: SPEC-` heading must appear on the line immediately
     after the closing `---` of the frontmatter block (no blank line
     between them).
   - No `<tasks spec="...">` wrapper element; the parser rejects it
     with an `UnknownMarkerName` error. Tasks are a flat sequence of
     `<task>` elements at the top level of the document.
   - Multiple requirements in `covers=` are separated by single ASCII
     spaces — `covers="REQ-001 REQ-002"` — never by commas. The parser
     rejects comma-separated values with a `TSK-004` lint error.
   - Seed `spec_hash_at_generation` and `generated_at` with the
     `bootstrap-pending` sentinel; step 3 fills them in. Do not invoke
     `speccy lock` before TASKS.md exists on disk — the command edits
     the file in place and errors when it is missing.
3. After writing, run `speccy lock SPEC-NNNN` to rewrite the two
   `bootstrap-pending` placeholders to the current SPEC.md sha256 and
   the UTC timestamp:

   ```bash
   speccy lock SPEC-NNNN
   ```

   `speccy lock` edits TASKS.md's frontmatter in place; it does not
   emit a hash to stdout, and it requires TASKS.md to already exist.

4. Suggest the next step: `/speccy-work SPEC-NNNN` to start the
   implementation loop.

This recipe does not loop.
