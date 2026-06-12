---
name: speccy-plan
description: 'Draft a new Speccy SPEC from the `AGENTS.md` product north star. Use when the user wants to "write a spec", "draft a SPEC", "spec out X", or "plan a new feature with speccy". Requires: `.speccy/` and `AGENTS.md`. If `.speccy/` is absent → prefer speccy-init. Do NOT trigger on fuzzy asks lacking concrete scope — prefer speccy-brainstorm first to atomize the ask.'
---

# /speccy-plan

Drafts a new `SPEC.md` from the `AGENTS.md` product north star. The
host harness auto-loads `AGENTS.md` (which carries the project-wide
product north star); this recipe walks the agent through writing
SPEC.md. Top-level intent surfaces (`<goals>`, `<non-goals>`,
`<user-stories>`, optional `<assumptions>`) and per-requirement
sub-sections (`<done-when>`, `<behavior>`, `<scenario>`) live as raw
XML element blocks inside SPEC.md itself.

## When to use

When starting a new spec slice. If the ask is still fuzzy, run
`/speccy-brainstorm` first to atomize the intent —
this skill writes SPEC.md in a single pass and assumes the framing
is already agreed.

## What to consider

- Bounded scope. One SPEC must answer one product question; refuse
  to bundle unrelated work. If the scope is too large to be tested
  end-to-end within one PR, split it.
- Decisions hidden inside requirement prose belong in `### Decisions`
  instead. Keep `<requirement>` bodies focused on observable behaviour
  and lift any architectural commitment into a `### Decisions` block.

## Steps

1. Query the next available ID:

   ```bash
   speccy vacancy --json
   ```

   The JSON's `next_spec_id` field is the allocated `SPEC-NNNN` ID.
   Decide placement: flat (`.speccy/specs/NNNN-slug/`) or under an
   existing mission folder (`.speccy/specs/[focus]/NNNN-slug/`).
   Do not invent a new mission folder for a single spec.

2. Write SPEC.md following the PRD template.

   When the slice touches existing code, invoke the `plan-explorer`
   subagent before/while drafting to trace the relevant feature
   through its entry points, call flows, and architecture layers.
   Fold its grounding into the `## Summary` prose and the
   `<requirement>` blocks. The explorer's report is **ephemeral**: do
   NOT persist it to a new `*.md` artifact file — its only durable home
   is the existing SPEC.md sections above.

   Canonical SPEC.md shape: `references/spec.md`.

   If the brainstorm output
   contains collapsed requirements (one requirement with an enumerated
   sub-list), you MAY expand each sub-bullet to its own atomic
   `<requirement>` block (when atomicity adds reviewer-fan-out value)
   or keep them grouped under one `<requirement>` with a `<done-when>`
   bullet list (when cohesive grouping serves the SPEC better). Agent
   discretion; neither choice is surfaced as a self-review issue.

3. **Self-review pass.** Run this pass exactly once after writing
   SPEC.md. Do not re-check after applying fixes.

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

   **The six check properties:**

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

   - **Placeholder leakage.** No `TBD`, `TODO`, or untouched
     `<...>` template-placeholder strings remain in SPEC.md.
     These are mechanical and should be fixed inline, not surfaced.

   - **Ambiguity.** No `<requirement>` wording is interpretable in
     two materially different ways that would lead to different
     implementations. If the requirement is ambiguous, surface it
     as a semantic issue.

   <!-- Note: the plan self-review above is an independent copy.
        The parallel copy for amend lives in speccy-amend.md. -->

4. Surface any material questions inline in `## Open Questions` using
   the alpha-prefix format: `- [ ] a.`, `- [ ] b.`, ..., `- [ ] z.`.
   Each question gets the next free letter in sequence. If the section
   already exists, preserve existing ordinals and allocate the next free
   letter for any new question added (no renumbering). Reaching `z.`
   signals an over-scoped session — 26 open questions is a scope smell,
   not a format limitation.

5. Branch-guard, then commit `SPEC.md` alone. After the self-review
   pass completes, commit the just-written `SPEC.md` so a
   `/speccy-plan` run-then-stop leaves `SPEC.md` already
   committed. The commit covers only the spec's `SPEC.md` —
   `TASKS.md` is committed by `/speccy-decompose`, not
   here, so the new-spec path lands two separate commits (one per
   skill). The step uses narrow file-list staging (never `git add -A`
   or `git add .`), so any unrelated dirty paths outside `<spec-dir>/`
   remain in the working tree untouched. The step is idempotent:
   re-running plan on an already-committed `SPEC.md` produces no new
   commit.

   First run the branch-guard prelude so the commit lands on a feature
   branch rather than the repository's default branch. Supply the
   prelude's one parameter — the **spec directory** (`<spec-dir>/`,
   i.e. the path that holds `SPEC.md`) — and run it:

## Branch-guard prelude

This module is the single source of truth for the branch-guard prelude
that the authoring skills run before their commit step. Each callsite
pulls it in with a MiniJinja `include` directive naming
`modules/references/branch-guard.md`; there is no verbatim copy of this
prelude in any individual skill body.

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

   - **Staging breadth: narrow `git add <spec-dir>/SPEC.md`.** Stage
     exactly the spec's `SPEC.md` and nothing else. Do not use
     `git add -A` or `git add .`. Staging unchanged content is a no-op,
     so passing the path unconditionally is safe regardless of whether
     `SPEC.md` was already committed.
   - **Title and body.**
     - **Title:** `[SPEC-NNNN]: create spec` with `SPEC-NNNN`
       substituted for the resolved spec id.
     - **Body:** the trimmed value of the `title:` field from SPEC.md's
       YAML frontmatter (the one-line title slug, not the full document
       heading).

   With those two parameters fixed, run the shared recipe — it defines
   the no-git short-circuit, the unified stage-then-`git diff --cached
   --quiet` idempotency check (an unchanged `SPEC.md` skips the commit
   silently), the `Co-Authored-By` trailer, and the HEREDOC commit
   mechanics:

## Shared commit recipe

This module is the single source of truth for how a skill turns a
just-written artifact into a git commit. Each callsite pulls it in with
a MiniJinja `include` directive naming
`modules/references/commit-recipe.md`; there is no verbatim copy of this
recipe in any individual skill body.

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
  (e.g. `claude-opus-4-8[1m]/low`). Never read `CLAUDE_EFFORT` or
  the `CLAUDE_CODE_EFFORT_LEVEL` runtime override — a sub-agent
  records its definition-file effort even when dispatched from a
  higher-effort parent session. A host with no effort knob omits
  the suffix entirely.


Apply that rule to fill the `<model>` segment of the trailer line. When
the host states no resolved identifier in-context, use the documented
fallback string
`Co-Authored-By: Speccy Skill Pack <noreply@anthropic.com>`.


6. Suggest the next step: `/speccy-decompose SPEC-NNNN` to
   decompose into `TASKS.md`.

This recipe does not loop.
