---
name: speccy-decompose
description: Decomposes a Speccy SPEC into a checklist of agent-sized tasks. Invoke via /agent speccy-decompose for the pinned execution path defined in this file's frontmatter.
model: opus[1m]
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

4. Bootstrap commit the SPEC artefacts. This closes the
   bootstrap-commit gap that would otherwise trip the SPEC-0045/REQ-002
   strict clean-tree gate when `/speccy-orchestrate
   SPEC-NNNN` is invoked on a freshly decomposed SPEC. The step uses
   narrow file-list staging (never `git add -A` or `git add .`), so
   any unrelated dirty paths outside `<spec-dir>/` remain in the
   working tree untouched. The step is idempotent: re-running
   decompose on an already-committed SPEC produces no new commit.

   1. Stage exactly the two SPEC artefacts via narrow `git add`:

      ```bash
      git add <spec-dir>/SPEC.md <spec-dir>/TASKS.md
      ```

      Do not use `git add -A` or `git add .`. Staging unchanged
      content is a no-op, so passing both paths unconditionally is
      safe regardless of whether SPEC.md was already committed.

   2. Run `git diff --cached --quiet`. If exit code is 0 (nothing
      staged), skip the commit silently — both files are already
      committed at their current content. If non-zero, proceed to
      the commit.

   3. Build the commit message:

      - **Title:** `[SPEC-NNNN]: create spec and decompose tasks`
        with `SPEC-NNNN` substituted for the resolved spec id.
      - **Body:** the trimmed value of the `title:` field from
        SPEC.md's YAML frontmatter (the one-line title slug, not
        the full document heading).
      - **Trailer:** a single `Co-Authored-By: <model> <noreply@anthropic.com>`
        line where `<model>` is sourced from the host harness's
        runtime model identifier (env var, runtime API, or
        host-specific equivalent). When the host does not expose a
        model identifier, use the documented fallback string
        `Co-Authored-By: Speccy Skill Pack <noreply@anthropic.com>`.
        Trailer resolution matches SPEC-0045/REQ-004 verbatim.

      Pass the body via a HEREDOC so newlines and any special
      characters in the SPEC title survive verbatim, e.g.:

      ```bash
git commit -m "$(cat <<'EOF'
[SPEC-NNNN]: create spec and decompose tasks

<SPEC title from frontmatter>

Co-Authored-By: <model> <noreply@anthropic.com>
EOF
)"
      ```

5. Suggest the next step: `/speccy-work SPEC-NNNN` to start the
   implementation loop.

This recipe does not loop.
