---
name: speccy-review
description: 'Review one Speccy task per invocation and exit, running one round of adversarial multi-persona review. With an optional `SPEC-NNNN/T-NNN` selector, the session reviews that task; without it, the skill resolves the next reviewable task via `speccy next --json`. Five personas (business, tests, security, style, correctness) fan out in parallel and either pass the task to `completed` or flip it back to `pending` with a `<blockers>` block appended to the per-task journal file. Use when the user says "review T-003" or "review the next task". Requires: a task in `state="in-review"`. If no in-review task and work remains → prefer speccy-work. If all tasks `completed` → prefer speccy-ship. Do NOT trigger on generic "review this PR" or "review my code" asks — this skill runs Speccy task-state review only.'
---

# /speccy-review

Runs one round of adversarial review on one task per invocation and
exits. With an optional `[SPEC-NNNN/T-NNN]` selector argument, the
session reviews that specific task. Without an argument, the session
resolves the next reviewable task via `speccy next --json` (workspace
form — used here because no SPEC is known on the no-selector path) and reviews
that one. Task state lives in the `state` attribute on each `<task>`
XML element in TASKS.md; review activity prose lives in the sibling
`.speccy/specs/NNNN-slug/journal/T-NNN.md` file, never inside the
`<task>` body.

This is a single-task primitive. It does not iterate over the
remaining `in-review` tasks; composition across tasks belongs to a
caller (a human at the terminal, the `/loop` skill, or a future
orchestrator).

Within the one task under review, the skill fans out to five
parallel persona sub-agents (default fan-out: `business`, `tests`,
`security`, `style`, `correctness`). That fan-out is intrinsic to
the primitive — adversarial diversity comes from fresh contexts per
persona — and is bounded to one round of five sub-agents on one
task.

Sub-agents cannot spawn sub-agents, so the fan-out runs inline in the
top-level session.
This skill must run in the top-level session — either a human
invocation
(`/speccy-review …`) or the
`/speccy-orchestrate` outer loop inlining this body at
its `review` dispatch.

## When to use

- With a selector (`/speccy-review SPEC-NNNN/T-003`):
  when the task to review is already known.
- Without an argument: when picking up wherever `TASKS.md` left off.
  The session reviews one task and exits.

The target task must already be in `state="in-review"` (typically
flipped there by `/speccy-work`).

## Steps

**Entry precondition (REQ-007, REQ-008):** before resolving the target task, query `speccy next --json` (per-spec form when a selector was passed, workspace form otherwise). If the returned envelope's `next_action.kind == "reconcile"`, dispatch the reconcile pass per the **Reconcile policy** below instead of running the normal review flow. Re-query after the pass; resume normal dispatch only when `consistency.status == "ok"`.

**Reconcile policy.** When `speccy next --json` (in either per-spec
or workspace form) returns `next_action.kind == "reconcile"`, iterate
`consistency.drifts[]` and apply the table action per entry, then
re-query before proceeding. See
`.claude/speccy-references/reconcile-policy.md` for the full
policy table and the three properties the dispatch holds by
construction (autonomous / rollback-biased / idempotent).


### Resolve the target task

   - If a `SPEC-NNNN/T-NNN` selector was passed, that is the target.
   - Otherwise, query the CLI in workspace form (no SPEC selector
     is known on this no-selector invocation path — we must walk
     the active tree to find the next reviewable task):

     ```bash
     # workspace form: no SPEC-NNNN known yet; scan the active tree.
     speccy next --json
     ```

     Workspace-form exit-code-stop contract: exit code 2 with a
     top-level `reason="no_active_specs"` field in the JSON envelope
     means the workspace has no active specs at all (fresh repo, or
     every spec has shipped or been archived). Exit gracefully and
     surface the reason; do not treat the non-zero exit as a CLI
     error.

     On exit code 0, if the resulting `specs` array has no entry
     with `next_action.kind == "review"`, exit and report
     that no reviewable tasks remain. Otherwise, construct
     the disambiguated `<spec>/<task>` form from the JSON's `spec_id`
     and `next_action.task_id` fields (the bare task ID is
     ambiguous across specs — every spec has its own `T-001`).

     Exit-code-stop contract: once SPEC-NNNN is resolved, any
     subsequent per-spec query (`speccy next SPEC-NNNN --json`) that
     exits non-zero means the SPEC has reached a terminal state —
     halt and surface the stderr line. Only parse JSON when exit
     code is 0.


### Run the persona fan-out and consolidation

This section is the canonical fan-out grammar. The
`/speccy-orchestrate` review dispatch runs the same
fan-out inline in its own session and points here rather than
duplicating it.


Fan out five reviewer-* sub-agents in parallel against the resolved
task, one per persona. Default fan-out: `reviewer-business`,
`reviewer-tests`, `reviewer-security`, `reviewer-style`,
`reviewer-correctness`. Two additional personas
(`reviewer-architecture`, `reviewer-docs`) are off the default
fan-out and are invoked explicitly when an architectural or
documentation risk is suspected.

The prompt for each spawn is:

> Review task `SPEC-NNNN/T-NNN`. Run `speccy context SPEC-NNNN/T-NNN
> --json` for the per-task bundle, read the diff with the bundle's
> suggested diff command, then apply your persona's review criteria.
> If a prior round's prose matters to your verdict, drill in with
> `speccy journal show SPEC-NNNN/T-NNN --round N [--block <type>]`.
>
> Follow the verdict-return contract in your agent file: append your
> own `<review>` block to the per-task journal via `speccy journal
> append` and return a single thin `<verdict>` element as your final
> message. Do not edit TASKS.md or the journal with file-editing
> tools.
>
> The working tree may be dirty: the implementer leaves changes
> uncommitted on purpose, and the orchestrator (not the implementer)
> owns the single atomic commit on review pass per REQ-003/REQ-004.
> On retry rounds the dirty tree is the prior pass's WIP that the
> retry implementer amended in place per the retry-shape contract.
> Do not flag uncommitted state, commit timing, or "changes not
> committed before the in-review flip" -- those are out of scope
> for per-task review.

Substitute the resolved `SPEC-NNNN/T-NNN` and the persona name per
spawn.

Invoke the `Task` tool five times **in one message** (parallel
dispatch) with `subagent_type: "reviewer-business"`,
`subagent_type: "reviewer-tests"`,
`subagent_type: "reviewer-security"`,
`subagent_type: "reviewer-style"`, and
`subagent_type: "reviewer-correctness"`. Each persona's agent
definition at `.claude/agents/reviewer-<persona>.md` carries the
host-native dispatch metadata.

Canonical journal `<review>` shape:
`.claude/speccy-references/journal-review.md`.

Canonical journal `<blockers>` shape:
`.claude/speccy-references/journal-blockers.md`.

Each reviewer sub-agent appends its own `<review>` block to
`.speccy/specs/NNNN-slug/journal/T-NNN.md` via `speccy journal
append --block review` before returning a thin `<verdict>` element
(per the verdict-return contract in its agent definition). The
CLI's per-file lock serializes those parallel appends, so the
running session never transcribes `<review>` blocks itself and never
edits the journal with file-editing tools. The orchestrator's job
after the fan-out settles is to **verify completeness, read back any
blockers, then drive the state flip through the CLI verbs**.

### Step 1 — verify the round's reviews are complete

Read back the appended `<review>` blocks for the round under review
through the CLI rather than trusting the returned thin verdicts:

```bash
speccy journal show SPEC-NNNN/T-NNN --json --block review --round latest
```

Confirm every persona you spawned appears in the result for the
latest round. If a persona is missing (its append failed, or its
sub-agent errored before appending), halt the fan-out and surface
the missing persona rather than flipping state on an incomplete
round. Do not parse the journal file by hand — `journal show` is the
read-back authority.

### Step 2 — drive the state flip through `speccy task transition`

Decide pass vs blocking from the verdicts the reviewers appended,
then flip the task's `state` with the transition command — never by
editing the `state` attribute in TASKS.md directly:

- If every spawned reviewer's appended `<review verdict="...">` is
  `verdict="pass"`, flip `in-review` → `completed`:

      speccy task transition SPEC-NNNN/T-NNN --to completed

- If any spawned reviewer's `<review verdict="...">` is
  `verdict="blocking"`, flip `in-review` → `pending`:

      speccy task transition SPEC-NNNN/T-NNN --to pending

  then append a single consolidated `<blockers>` block (step 3).

### Step 3 — consolidate blockers via `speccy journal append`

On a blocking round, read back the failing reviews and write **one**
consolidated `<blockers>` block — not one per reviewer, not a partial
write. Read the blocking review bodies through the CLI:

```bash
speccy journal show SPEC-NNNN/T-NNN --json --verdict blocking --round latest
```

The `<blockers>` **body is orchestrator-authored semantic judgment**
(DEC-001 non-goal: the CLI never synthesizes blocker prose). Compose
the body from the blocking reviews you just read back, then append it
with the body on stdin:

```bash
speccy journal append SPEC-NNNN/T-NNN --block blockers <<'EOF'
<one-line summary of what to change before the next implementer pass>.
<optional bullets enumerating each persona's blocker>.
EOF
```

The CLI owns the appended block's `date`, `round`, and open/close
tags, plus the journal's frontmatter and sectioning. **Do not
compute, supply, or hand-author any of them** — there is no override
flag; the body you pipe on stdin is the inner text only. Validation
runs before any write, so a malformed body leaves the journal
byte-identical.


The single-writer rule holds: the CLI's append lock owns write
serialization across the parallel reviewer appends and this
consolidated `<blockers>` append, and the orchestrator remains the
sole author of `<blockers>` bodies (and, per the commit step below,
of git commits). The running session issues only CLI verbs — `journal
show`, `journal append`, `task transition` — for the review-induced
journal and state writes; it never edits TASKS.md or the journal file
with file-editing tools.

### Atomic commit on review pass (REQ-003, REQ-004)

When every spawned reviewer returned `verdict="pass"` and the
`speccy task transition … --to completed` flip has run (the reviewer
`<review>` appends already landed via the CLI during the fan-out),
the running session performs the commit step using the shared commit
recipe below. The commit captures the implementer's code changes, the
TASKS.md state flip, and the journal append in a single atomic commit
(parent count = 1).

Supply the recipe's two behaviour-varying parameters as follows:

- **Staging breadth: `git add -A`.** Stage everything in the working
  tree. Do not stage selectively — `git add -A` is sound under the
  clean-tree precondition (REQ-002) that fires at the start of work
  dispatch, which guarantees every dirty path at commit time is
  task-scoped.
- **Title and body.**
  - **Title:** `[SPEC-NNNN/T-NNN]: <task title>` — `<task title>` is
    read verbatim from the `<task>` element's `## ` heading in
    TASKS.md (the one-line H2 immediately after the `<task ...>`
    opening tag). Substitute the resolved spec and task IDs. This
    title prefix is the sole task-identity link in git history; the
    consistency check correlates commits to tasks by grepping for it.
  - **Body:** the trimmed content of the `Completed` field from the
    latest `<implementer>` block in the per-task journal file. Extract
    mechanically as the bytes between the `- Completed:` bullet marker
    and the next `- <Field>:` bullet marker (one of `Undone`,
    `Hygiene checks`, `Evidence`, `Discovered issues`,
    `Procedural compliance`). Trim leading and trailing whitespace.

With those two parameters fixed, run the shared recipe — it defines
the no-git short-circuit, the unified stage-then-`git diff --cached
--quiet` idempotency check (a clean working tree skips the commit
silently, matching the prior behaviour), the `Co-Authored-By` trailer,
and the HEREDOC commit mechanics:

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


The skill body does not check the current git branch; it trusts the
caller / host to have placed the working tree on a feature branch.
Commits land on whatever HEAD is.


### Exit

Do not pick up another `in-review` task. If the caller wants
another task reviewed, the caller invokes this skill again.

After exit, the next reasonable step depends on TASKS.md state:
if any task is `state="pending"` (a retry), suggest
`/speccy-work SPEC-NNNN`. If any remain
`state="in-review"`, suggest
`/speccy-review SPEC-NNNN` again. If all tasks are
`state="completed"`, suggest `/speccy-vet SPEC-NNNN`.
