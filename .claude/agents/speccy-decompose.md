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
  the tasks may now be stale (the CLI surfaces a spec-hash-mismatch
  lint when it detects hash drift).

## Steps

1. Read the spec's current state to locate SPEC.md:

   ```bash
   speccy status SPEC-NNNN --json
   ```

   The JSON's `spec_md_path` field names the SPEC.md to decompose.
   If `tasks_md_path` is non-null, an existing TASKS.md is present
   and this is an amendment run (edit surgically; preserve
   `state="completed"` tasks unless invalidated).

   Before authoring `TASKS.md`, invoke the `plan-architect` subagent
   against the SPEC to produce an implementation blueprint. Treat its
   build-sequence checklist as the **candidate** task list — you
   retain final `<task>` authorship and MAY merge, split, reorder, and
   renumber the candidates to land agent-sized tasks. Promote any
   load-bearing design decision surfaced by the blueprint into a
   SPEC.md `### Decisions` (DEC-NNN) block rather than burying it in
   task prose.

2. Write `TASKS.md` as Markdown with a sequence of
   `<task id="T-NNN" state="pending" covers="REQ-NNN">...</task>`
   block per task directly under the heading — no `<tasks>` wrapper
   element. Each `<task>` body contains a `<task-scenarios>`
   element with slice-level Given/When/Then prose and an optional
   `Suggested files:` bullet.

   The file must open with a YAML frontmatter block followed immediately
   by a level-1 heading.

   Canonical TASKS.md shape: `.claude/skills/speccy-decompose/references/tasks.md`.

   Key constraints:
   - The `# Tasks: SPEC-` heading must appear on the line immediately
     after the closing `---` of the frontmatter block (no blank line
     between them).
   - No `<tasks spec="...">` wrapper element. Tasks are a flat sequence
     of `<task>` elements at the top level of the document.
   - Multiple requirements in `covers=` are separated by single ASCII
     spaces — `covers="REQ-NNN REQ-NNN"` — never by commas.
   - Seed `spec_hash_at_generation` and `generated_at` with the
     `bootstrap-pending` sentinel; step 3 fills them in. Do not invoke
     `speccy lock` before TASKS.md exists on disk — it errors when the
     file is missing.
3. After writing, run `speccy lock SPEC-NNNN` to rewrite the two
   `bootstrap-pending` placeholders to the current SPEC.md sha256 and
   the UTC timestamp:

   ```bash
   speccy lock SPEC-NNNN
   ```

   `speccy lock` requires TASKS.md to already exist.

4. Branch-guard, then commit `TASKS.md` alone. This closes the
   bootstrap-commit gap that would otherwise trip the strict clean-tree
   gate when `/speccy-orchestrate SPEC-NNNN` is invoked
   on a freshly decomposed SPEC. The commit runs
   after `speccy lock`. It commits only the spec's `TASKS.md` —
   `SPEC.md` is committed by `/speccy-plan`, not here, so
   the new-spec path lands two separate commits (one per skill). The
   step uses narrow file-list staging (never `git add -A` or
   `git add .`), so any unrelated dirty paths outside `<spec-dir>/`
   remain in the working tree untouched. The step is idempotent:
   re-running decompose on an already-committed `TASKS.md` produces no
   new commit.

   First run the branch-guard prelude so the commit lands on a feature
   branch rather than the repository's default branch. Supply the
   prelude's one parameter — the **spec directory** (`<spec-dir>/`,
   i.e. the path that holds `SPEC.md` and `TASKS.md`) — and run it:

## Branch-guard prelude

The prelude guarantees that HEAD is on a feature branch before any
artifact is committed, so an authored `SPEC.md` / `TASKS.md` never lands
on the repository's default branch. It creates a branch only when it
must, reuses an existing feature branch otherwise, and never prompts for
confirmation.

The caller supplies one parameter:

- **Spec directory.** The path to the spec's directory, used to derive
  the branch name. For a flat spec this is `.speccy/specs/NNNN-slug/`;
  for a mission-foldered spec it is `.speccy/specs/[focus]/NNNN-slug/`.

### No-git short-circuit

Before doing anything, check whether the working directory is inside a
git repository:

```bash
git rev-parse --is-inside-work-tree
```

If this exits non-zero (the project is not a git repository), **skip the
entire branch-guard without erroring**. The authoring skill still writes
its artifact and continues; no branch is created and no git failure is
surfaced. This preserves Speccy's "works identically in any project
state" property for non-git projects.

### Default-branch detection

When a git repository is present, identify the repository's default
branch via an ordered three-tier chain. Each tier is consulted only when
the prior tier **does not resolve**:

1. **Remote symbolic ref `origin/HEAD`.** When a remote exists, read the
   branch it points at:

   ```bash
   git symbolic-ref --quiet refs/remotes/origin/HEAD
   ```

   The trailing path component (e.g. `origin/main` → `main`) is the
   default-branch name. If there is no remote, or `origin/HEAD` is not
   set, this tier does not resolve — fall through to tier 2.

2. **`git config init.defaultBranch`.** Otherwise, read the configured
   default-branch name:

   ```bash
   git config init.defaultBranch
   ```

   If this is set (e.g. `trunk`), it is the default-branch name. If it is
   unset, this tier does not resolve — fall through to tier 3.

3. **`{main, master}` name match.** Otherwise, when neither remote nor
   config resolved, treat HEAD as the default branch only when its own
   branch name is `main` or `master`. HEAD on any other name is treated
   as a feature branch.

### Branch-creation condition

Read the current HEAD branch name:

```bash
git symbolic-ref --quiet --short HEAD
```

A detached HEAD makes this exit non-zero (there is no current branch
name).

- **Create path** — when HEAD is the detected default branch **or** HEAD
  is detached, derive the branch name and create it. The branch name is
  the literal `spec-` prefix followed by the **basename** of the spec
  directory (its final `NNNN-slug` path component). For a mission-
  foldered spec at `.speccy/specs/[focus]/NNNN-slug/`, the `[focus]`
  segment is dropped — only the basename is used, so the name stays flat
  `spec-NNNN-slug`. Then create and switch to it:

  ```bash
  git switch -c spec-NNNN-slug
  ```

  Emit a one-line notice naming the created branch, for example:

  ```
  Created and switched to branch spec-NNNN-slug.
  ```

- **Reuse path** — when HEAD is on any other branch (an existing
  `spec-NNNN-slug`, or an unrelated feature branch), reuse it: create
  nothing, switch nothing, leave HEAD unchanged.

The one-line creation notice is emitted **only on the create path**,
never on the reuse path.

> Illustrative example — substitute your own values. For a spec at
> `.speccy/specs/0042-example-slug/` (or
> `.speccy/specs/acme/0042-example-slug/` under a mission folder), the
> derived branch is `spec-0042-example-slug` in both cases.


   Then run the shared commit recipe, supplying its two
   behaviour-varying parameters as follows:

   - **Staging breadth: narrow `git add <spec-dir>/TASKS.md`.** Stage
     exactly the spec's `TASKS.md` and nothing else. Do not use
     `git add -A` or `git add .`. Staging unchanged content is a no-op,
     so passing the path unconditionally is safe regardless of whether
     `TASKS.md` was already committed.
   - **Title and body.**
     - **Title:** `[SPEC-NNNN]: decompose tasks` with `SPEC-NNNN`
       substituted for the resolved spec id.
     - **Body:** the trimmed value of the `title:` field from SPEC.md's
       YAML frontmatter (the one-line title slug, not the full document
       heading).

   With those two parameters fixed, run the shared recipe — it defines
   the no-git short-circuit, the unified stage-then-`git diff --cached
   --quiet` idempotency check (an unchanged `TASKS.md` skips the commit
   silently), the `Co-Authored-By` trailer, and the HEREDOC commit
   mechanics:

## Shared commit recipe

The caller supplies two — and only two — behaviour-varying parameters:

- **Staging breadth.** Either `git add -A` (stage everything in the
  working tree) or a narrow `git add <paths>` list (stage exactly the
  named paths, leaving unrelated dirty paths untouched). The caller's
  prose states which form applies and why.
- **Title and body.** The commit message title line and body, built by
  the caller from its own artifact (e.g. a `[SPEC-NNNN]:`-prefixed
  title and a body drawn from the artifact's frontmatter or journal).

Everything else — the no-git short-circuit, the idempotency check, the
trailer, and the HEREDOC mechanics — is identical for every caller and
is defined once here.

### No-git short-circuit

Before staging anything, check whether the working directory is inside
a git repository:

```bash
git rev-parse --is-inside-work-tree
```

If this exits non-zero (the project is not a git repository), **skip
the entire commit step without erroring**. The just-written artifact is
left in place on disk; no commit is attempted and no git failure is
surfaced. This preserves Speccy's "works identically in any project
state" property for non-git projects.

### Stage, then skip-if-empty, then commit

When a git repository is present:

1. **Stage** using the caller's chosen breadth — `git add -A` or the
   narrow `git add <paths>` list. Staging unchanged content is a no-op,
   so a narrow caller may pass its full path set unconditionally
   regardless of whether some of those paths were already committed.

2. **Idempotency check** — run the single unified form:

   ```bash
   git diff --cached --quiet
   ```

   If exit code is 0 (nothing staged), **skip the commit silently** —
   the configured paths are already committed at their current content.
   No surface to the user, no error. This is the only idempotency
   check; do not substitute a pre-stage `git status --porcelain`
   variant. If exit code is non-zero, proceed to the commit.

3. **Commit** with the caller's title and body, passing the message via
   a HEREDOC so newlines and any special characters survive verbatim:

   ```bash
   git commit -m "$(cat <<'EOF'
   <caller title>

   <caller body>

   Co-Authored-By: <model> <noreply@anthropic.com>
   EOF
   )"
   ```

   The commit is single-parent (parent count = 1). The skill body does
   not check or change the current git branch; the commit lands on
   whatever HEAD is.

### Trailer

The `Co-Authored-By` trailer is resolved by the identity-sourcing rule,
not restated here:

## Sourcing your recorded identity

Build the `model="..."` value from two independently sourced parts;
never infer either from the skill-pack name, the persona name, or an
inherited environment variable.

- **Model segment** — the resolved long-form identifier your host
  states in-context (e.g. `claude-opus-4-8[1m]`), transcribed
  verbatim: keep the host's version punctuation (`claude-opus-4-8`,
  never `claude-opus-4.8`), never substitute a configured alias.
  When the host states no resolved identifier in-context, fall back
  to the `model:` value in your own agent definition file.
- **Effort suffix** — when the host exposes a reasoning-effort knob,
  read it from your own definition file (`effort:` on Claude Code,
  `model_reasoning_effort` on Codex) and append it as a slash-suffix
  (e.g. `claude-opus-4-8[1m]/low`); never read it from a runtime
  env override. A host with no effort knob omits the suffix
  entirely.


Apply that rule to fill the `<model>` segment of the trailer line. When
the host states no resolved identifier in-context, use the documented
fallback string
`Co-Authored-By: Speccy Skill Pack <noreply@anthropic.com>`.


## Exit

`TASKS.md` is written, locked (the two `bootstrap-pending` placeholders are now
the SPEC sha256 + UTC timestamp), and committed alone on a feature branch.
Single pass, no loop. Next step:
`/speccy-orchestrate SPEC-NNNN` to drive the implementation +
review loop end-to-end, or `/speccy-work SPEC-NNNN` for one task
at a time.
