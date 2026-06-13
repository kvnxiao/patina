---
name: speccy-amend
description: 'Orchestrate a mid-loop SPEC change — surgically edit SPEC.md with a Changelog row, reconcile TASKS.md, and re-record the spec hash so the SPEC and task list are back in sync. Use when the user says "amend SPEC-NNNN", "the requirements shifted", or when speccy reports the SPEC and tasks are out of sync. Requires: an existing `SPEC.md`. If no `SPEC.md` exists yet → prefer speccy-plan to draft one. Do NOT trigger for cosmetic edits to SPEC.md that do not change Requirements — direct edits are fine.'
---

# /speccy-amend

Orchestrates a mid-loop SPEC change. Edits SPEC.md surgically via the
plan-amend prompt, then reconciles TASKS.md via the tasks-amend prompt,
then re-records the spec hash.

## When to use

When `speccy status` reports `TSK-003: spec hash mismatch`, or when the
user signals that intent has shifted mid-loop. Use this rather than
manually editing SPEC.md so that the Changelog row and TASKS.md
reconciliation are not forgotten.

## Steps

1. Read the existing SPEC's current location and state:

   ```bash
   speccy status SPEC-NNNN --json
   ```

   The JSON's `spec_md_path` field names the SPEC.md file to edit.
2. Edit SPEC.md surgically (including its `<requirement>` /
   `<scenario>` element blocks if requirements changed); append a
   `## Changelog` row explaining *why* the amendment was needed.
   If editing `## Open Questions`, use the alpha-prefix format:
   `- [ ] a.`, `- [ ] b.`, ..., `- [ ] z.`. Preserve existing ordinals
   (do not renumber on amend); allocate the next free letter when
   appending a new question. Reaching `z.` signals an over-scoped
   session — 26 open questions is a scope smell, not a format
   limitation.
3. **Self-review pass.** Run this pass exactly once after writing the
   SPEC.md diff and appending the Changelog row. Do not re-check after
   applying fixes.

   <!-- Shared self-review core for plan + amend; supersedes SPEC-0034 DEC-001 (lists stabilized → extracted). Brainstorm's pre-check is intentionally separate. -->

   **Mechanical/semantic split.** Mechanical issues are
   string-matchable from the SPEC.md text: `TBD`/`TODO` strings,
   "and"/"also" inside `<requirement>` blocks, untouched `<...>`
   template placeholders, missing alpha-prefix ordinals in
   `## Open Questions`. Fix mechanical issues inline by editing
   SPEC.md — do not write anything to `## Open Questions` or to
   chat. If judging requires reading semantics, it is semantic.

   Semantic issues surface as a row appended to `## Open Questions`
   using this fixed template string verbatim:

   `- [ ] {ordinal}. **Self-review caught:** {issue}`

   where `{ordinal}` is the next free alpha-prefix letter continuing
   any existing sequence, and `{issue}` is a one-line description of
   the problem. Do not substitute freeform prose.

   **The check properties:**

   - **Routing fidelity.** Brainstorm artifacts landed in the
     correct SPEC.md sections: restated ask → Summary +
     Requirements; assumptions → `<assumptions>`; open questions →
     `## Open Questions`; rejected framings → `## Notes` or
     `<decision>` blocks. This check applies only when brainstorm
     ran for this SPEC. When brainstorm was skipped, scope-traces
     alone covers the equivalent verification against the user's
     stated ask.

   - **Atomization.** No `<requirement>` body contains "and"/"also"
     multi-outcome wording that implies two distinct verifiable
     outcomes in one requirement. A requirement that bundles two
     outcomes should be split.

   - **Scope-traces.** Every `<requirement>` traces to a brainstorm
     artifact or to the user's explicitly stated ask. Requirements
     that appeared without a visible source in the approved framing
     are scope creep.

   - **Internal consistency.** No contradictions exist across the
     goals, non-goals, requirements, and assumptions sections. A
     goal that a non-goal denies, or a requirement that violates an
     assumption, is an internal contradiction.

   - **Ambiguity.** No `<requirement>` wording is interpretable in
     two materially different ways that would lead to different
     implementations. If the requirement is ambiguous, surface it
     as a semantic issue.

   Amend adds two deltas beyond the shared core:

   - **Changelog row presence.** The `## Changelog` section contains
     a new row explaining *why* this amendment was needed. A missing
     or empty Changelog entry is a mechanical issue; fix it inline
     before handing off to TASKS.md reconciliation.

   - **Surgical-diff shape.** Only the requirements and sections
     directly affected by the triggering intent shift were edited.
     A diff that rewrites unrelated requirements, re-words stable
     prose, or restructures sections that the amendment did not
     touch is out-of-scope and should be reverted inline.

4. Reconcile TASKS.md. Three kinds of reconciliation:

   - **Structural edits** — add new `<task>` elements for newly
     added requirements and remove `<task>` elements for dropped
     requirements. These are structural TASKS.md edits, not `state`
     mutations, so edit TASKS.md directly.
   - **State invalidation** — preserve `state="completed"` tasks
     unless the SPEC change invalidated them. For each invalidated
     task, flip `completed` → `pending` through the transition
     command, never by editing the `state` attribute directly:

     ```bash
     speccy task transition SPEC-NNNN/T-NNN --to pending
     ```

   - **Blocker directive** — for each invalidated task, append an
     amendment-driven `<blockers>` block to the per-task journal at
     `.speccy/specs/NNNN-slug/journal/T-NNN.md` via `speccy journal
     append`, never by editing the journal file directly and never
     into the `<task>` body in TASKS.md (the parser rejects journal
     elements there):

     ```bash
     speccy journal append SPEC-NNNN/T-NNN --block blockers <<'EOF'
     spec amended; <what changed in SPEC and what the next
     implementer attempt must address>.
     EOF
     ```

   The `<blockers>` body stays amendment-authored semantic judgment:
   name what changed in SPEC and what the next implementer attempt
   must address.

   The CLI owns the appended block's `date`, `round`, and open/close
tags, plus the journal's frontmatter and sectioning. **Do not
compute, supply, or hand-author any of them** — there is no override
flag; the body you pipe on stdin is the inner text only. Validation
runs before any write, so a malformed body leaves the journal
byte-identical.


   Here `round` matches the current implementer round, so the next
   attempt continues at `N+1`.

   Canonical journal `<blockers>` shape: `.claude/speccy-references/journal-blockers.md`.

   The `completed` → `pending` transition and the `<blockers>` append
   on the affected task are part of the same amendment turn.
5. Record the new spec hash:

   ```bash
   speccy lock SPEC-NNNN
   ```

6. Re-run `speccy status` to confirm `TSK-003` cleared.

7. Branch-guard, then commit the amend's reconcile delta. After the
   `TSK-003`-clear check in step 6 confirms the SPEC and tasks are back
   in sync, commit this amend's delta so the reconciled artifacts are
   recorded together. The commit covers the spec's `SPEC.md`, the
   reconciled `TASKS.md` **when one exists**, and any per-task journal
   blocker files this amend appended this run
   (`<spec-dir>/journal/T-NNN.md`). When the spec has no `TASKS.md` yet,
   the commit contains `SPEC.md` (plus any journal files) without failing
   on the absent tasks file — drop the missing `TASKS.md` from the
   staging list rather than requiring it to exist.

   First run the branch-guard prelude so the commit lands on a feature
   branch rather than the repository's default branch. Supply the
   prelude's one parameter — the **spec directory** (`<spec-dir>/`, i.e.
   the path that holds `SPEC.md`) — and run it:

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

   - **Staging breadth: narrow `git add <paths>`.** Stage exactly the
     amend delta and nothing else: `<spec-dir>/SPEC.md`, the reconciled
     `<spec-dir>/TASKS.md` **only when it exists** (omit it from the list
     when the spec has no tasks file yet — do not let a missing path
     fail the stage), and each `<spec-dir>/journal/T-NNN.md` blocker file
     appended this run. Do not use `git add -A` or `git add .`.
   - **Title and body.**
     - **Title:** `[SPEC-NNNN]: amend — <why>` with `SPEC-NNNN`
       substituted for the resolved spec id, and `<why>` a title-length
       phrase derived from the **newest `## Changelog` row** added during
       this amend (step 2). Do not separately prompt for `<why>`; read it
       off the row you just wrote.
     - **Body:** the full text of that newest `## Changelog` row,
       explaining why the amendment was needed.

   With those two parameters fixed, run the shared recipe — it defines
   the no-git short-circuit, the unified stage-then-`git diff --cached
   --quiet` idempotency check (nothing new to record skips the commit
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


### Loop exit criteria

This recipe is a single pass, not a loop -- but step 6 is the gate. If
the lint still fires, repeat from step 1 (something was missed).

Suggest the next step: `/speccy-work SPEC-NNNN` to pick up any tasks
that flipped back to `state="pending"`.
