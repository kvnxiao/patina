---
name: speccy-init
description: 'Bootstrap a new Speccy workspace by scaffolding `.speccy/` and installing the host-native skill pack. Use when the user says "set up speccy", "init speccy", "add speccy to this repo", or wants to start a spec-driven workflow somewhere that has no `.speccy/` yet. Also seeds two canonical sections into `AGENTS.md`: the `## Product north star` (freeze-on-first-write, captured via an adaptive draft-or-Socratic flow) and the `## Speccy conventions` section (always-upsert canonical boilerplate covering when-to-use-which-skill, the dev loop, test hygiene, commit hygiene, and the CI-gate suggestion). Run once per project before any other speccy-* skill. Requires: no preconditions. Do NOT trigger when `.speccy/` already exists — use speccy-amend for SPEC edits or speccy-plan for a new SPEC instead.'
---

# /speccy-init

Bootstraps a Speccy workspace: scaffold `.speccy/`, copy the Claude
Code skill pack into `.claude/skills/`, seed the product north star
into the project's root `AGENTS.md` (freeze-on-first-write), and
upsert the canonical `## Speccy conventions` section into the same
`AGENTS.md` (always-upsert, so re-runs refresh it).

## When to use

Run once per project, before any other Speccy slash-command. Re-run
with `--force` after upgrading `speccy` to refresh both the shipped
skill files **and** the `## Speccy conventions` section in
`AGENTS.md` so your agents pick up newly shipped skills and refined
rules. The `## Product north star` section is written once and then
left alone; the conventions section is always re-upserted from the
canonical template. `speccy init` only ever touches files it ships;
user-authored skill files in `.claude/skills/` are left alone.

## Steps

1. **Scaffold the workspace.** Run the CLI:

   ```bash
   speccy init
   ```

   If `.speccy/` already exists, ask the user whether to pass
   `--force` to refresh shipped files in place. Re-run as needed.

2. **Read the plan summary.** The CLI prints every file it will
   `create` or `overwrite`, then writes them. There is no
   "preserve" category: Speccy never plans writes against
   user-authored files in the host skill directory — they are
   simply not enumerated.

3. **Inspect `AGENTS.md` at the repo root and decide per-section.**
   `/speccy-init` seeds two independent sections — `## Product north
   star` and `## Speccy conventions`. Make the two seeding decisions
   independently per the AGENTS.md state matrix: north-star
   (present / absent) × conventions (present / absent), four cells.

   - **North star — absent.** Run the Q&A flow (step 4) and write
     the `## Product north star` section. Equivalent headings like
     `## Mission`, `## Product`, or `## Vision` count as present —
     do not duplicate.
   - **North star — present.** Skip the Q&A entirely. Confirm with
     the user that the existing content is current and continue
     without modification (freeze-on-first-write — the section
     carries user-authored prose that must not be stomped on
     re-run).
   - **Conventions — absent.** Append the canonical body (step 5).
   - **Conventions — present.** Replace the body verbatim (step 5).

   The four cells of the matrix are the four combinations: (north
   star absent + conventions absent) runs both Q&A and the append
   path; (north star absent + conventions present) runs Q&A then the
   replace path; (north star present + conventions absent) skips Q&A
   and runs the append path; (north star present + conventions
   present) skips Q&A and runs the replace path.

   **Missing-file path.** When `AGENTS.md` is missing entirely
   (first init, or the user deleted it between invocations),
   re-bootstrap from scratch — run the Q&A and write a fresh file
   with both sections. Do not warn, refuse, or special-case "user
   deleted AGENTS.md after a prior init" as a regression; the skill
   silently re-bootstraps.

   The two seeding decisions are independent: the state of one
   section does not bias the treatment of the other. In particular,
   the conventions upsert (step 5) runs on every invocation
   regardless of whether the north-star Q&A ran or was skipped.
   Never overwrite `## Product north star` content; the conventions
   section is the only section the skill replaces in place.

4. **North-star adaptive flow (states A and B — north-star absent).**
   Drive the `## Product north star` section as a brainstorm-style
   adaptive iteration over its five subsections in template order:
   opening prose (the project description and motivation paragraph),
   `### Users`, `### V1.0 outcome`, `### Quality bar`, and
   `### Known unknowns`. Do not run a fixed question script.

   **Inspect the repo first.** Before asking the user anything, read
   the obvious context the working tree already carries: any
   top-level `README.md`, manifest files (`Cargo.toml`,
   `package.json`, `pyproject.toml`, `go.mod`, etc.), the top-level
   source layout, and any prose already in `AGENTS.md`. Use the
   inspection to gauge per-subsection legibility — which of the five
   subsections you can plausibly draft from context, and which
   require eliciting answers from the user.

   **Per-subsection draft-or-Socratic decision.** Walk the five
   subsections in the documented order. For each one, decide
   independently:

   - **Legible from repo context → draft and confirm.** Draft the
     subsection from the inspected context and present it to the
     user for confirmation. Iterate on user redirects until the
     subsection is approved.
   - **Not legible → one-at-a-time Socratic Q&A.** Ask one question
     at a time. Prefer multiple-choice framings when the answer
     space is enumerable (e.g. "Is this primarily for: (a) solo
     developers, (b) a small team, (c) a public audience, (d)
     other — please describe?"); fall back to open prose only when
     enumeration genuinely doesn't fit. Iterate until the
     subsection is user-approved.

   These brainstorm-style patterns — one question at a time,
   multiple-choice when enumerable, draft-and-confirm, hard gate
   before write — are inlined here deliberately. Do not invoke
   `/speccy-brainstorm` or any other sub-skill from this path;
   `/speccy-init` stays self-contained.

   **Hard gate before write.** Do not write the `## Product north
   star` section to `AGENTS.md` until every one of the five
   subsections is explicitly user-approved. Partial drafts stay in
   the conversation; only the fully approved composition lands on
   disk. User redirects mid-iteration return to the relevant
   subsection rather than restarting the whole flow.

   When all five subsections are approved, compose them into the
   `## Product north star` section under the subheadings
   `### Users`, `### V1.0 outcome`, `### Quality bar`, and
   `### Known unknowns`, with the opening prose at the section root.
   Non-goals belong as prose at the section root or under an
   optional `### Non-goals` subsection if they surfaced during
   iteration. Constraints should reference the project's existing
   `## Core principles` / `## Standard hygiene` if present.

   **Asymmetry vs. the conventions upsert (step 5).** The north
   star is freeze-on-first-write because its content is
   user-authored — once the user has approved a draft and it lands,
   re-runs of `/speccy-init` must not stomp it (step 3, State C
   path). The `## Speccy conventions` section is the opposite:
   canonical boilerplate sourced from upstream, safe to refresh
   verbatim on every invocation. The asymmetry is principled —
   each section's update policy follows from who owns its content.

5. **Upsert the `## Speccy conventions` section.** After the
   north-star step completes (whether the Q&A ran or was skipped),
   perform a deterministic upsert on the `## Speccy conventions`
   section using the heading boundary as the delimiter:

   - **Heading absent.** Append the canonical body (with its
     `## Speccy conventions` heading) to the end of `AGENTS.md`.
   - **Heading present.** Replace everything from the
     `## Speccy conventions` heading down to (but not including)
     the next top-level `##` heading — or to end of file if no
     subsequent top-level heading exists — with the canonical
     body. Content under sibling `##` headings (before or after
     the conventions section) is left byte-identical.

   The replace path runs on every invocation. There is no "section
   already matches canonical body, skip" optimization — the
   canonical body is rewritten verbatim every time so upstream
   prose refreshes propagate after a Speccy upgrade.

   The heading boundary is the sole delimiter. Do not introduce
   HTML comment markers (`<!-- speccy:conventions:start -->` or
   similar) to fence the region — the heading plus the preamble
   line inside the canonical body make the upsert contract
   visible without machine-readable markers.

   The canonical body is the literal content below, expanded from
   the shared reference module at render time:

   ## Speccy conventions

> Managed by `/speccy-init`; edits inside this section are
> overwritten on re-run. Put project-specific additions in a sibling
> section.

### When to use which skill

- `/speccy-init` — bootstrap a new Speccy workspace by scaffolding
  `.speccy/` and seeding both the product north star and this
  conventions section into `AGENTS.md`. Run once per project before
  any other `speccy-*` skill. Re-running refreshes this section.
- `/speccy-brainstorm` — atomize a fuzzy ask into first-principle
  requirements before any `SPEC.md` is written. Use when the user
  says "help me brainstorm", "let's think about X", or when the
  scope is unclear. Stops at a hard gate until the framing is
  user-approved.
- `/speccy-plan` — draft a new `SPEC.md` from the product north
  star. Use when the user says "write a spec", "draft a SPEC", or
  "spec out X". Requires `.speccy/` and `AGENTS.md`.
- `/speccy-amend` — orchestrate a mid-loop SPEC change. Edits
  `SPEC.md` with a Changelog row, reconciles `TASKS.md`, and
  re-records the spec hash. Use when requirements shift or
  `speccy` reports the SPEC and tasks are out of sync.
- `/speccy-decompose` — decompose a SPEC into a checklist of
  agent-sized tasks in `TASKS.md`, or reconcile the list after an
  amendment. Use when the user says "break the spec into tasks" or
  the task list looks stale.
- `/speccy-work` — implement one Speccy task per invocation. With
  an optional `SPEC-NNNN/T-NNN` selector, implements that task;
  without one, resolves the next implementable task. Use when the
  user says "implement T-003" or "work the next task".
- `/speccy-review` — review one Speccy task per invocation by
  fanning out adversarial multi-persona review (business, tests,
  security, style by default). Passes the task to `completed` or
  flips it back to `pending` with a blockers block in the journal.
- `/speccy-vet` — run a holistic SPEC-vs-implementation drift
  review at the pre-ship boundary, with an autonomous drift-fix
  retry loop and a simplifier polish pass. Use when the user says
  "check for drift before shipping".
- `/speccy-ship` — close out a Speccy spec: write `REPORT.md`,
  run `speccy verify`, commit, and open a pull request. Use when
  every task is `state="completed"`.
- `/speccy-orchestrate` — drive the full implementation + review
  loop for one SPEC end-to-end by chaining `/speccy-work`,
  `/speccy-review`, and `/speccy-vet` until the spec is
  ready-to-ship. Stops one step before shipping so the operator
  can decide.

### The dev loop

Speccy work moves through five phases:

1. **Plan** — draft `SPEC.md` (`/speccy-plan`, optionally preceded
   by `/speccy-brainstorm`).
2. **Tasks** — decompose into agent-sized work (`/speccy-decompose`).
3. **Impl** — implement one task at a time (`/speccy-work`).
4. **Review** — adversarial per-task review (`/speccy-review`),
   followed by holistic pre-ship drift review (`/speccy-vet`).
5. **Ship** — produce the report and open the PR (`/speccy-ship`).

Per-task implementer notes, reviewer verdicts, and blocker
directives all live in a per-task journal file at
`.speccy/specs/NNNN-slug/journal/T-NNN.md`, sibling to `SPEC.md`
and `TASKS.md`. Inspect that file to follow the conversation
between implementer and reviewer rounds for any given task.

### Test hygiene

A test must gate a real invariant of the system under test — not
editorial decisions, not its own source constant, not the build's
own ability to compile. Do not write any of the following vacuous
shapes:

1. **Substring-matching human-curated prose.** Asserting that a
   specific sentence appears in a hand-authored document (a
   README, an AGENTS file, a SPEC body) gates editorial choices,
   not behavior. Such tests break on legitimate rewrites. If a
   concept must be discoverable in docs, enforce it via review or
   over a stable structural surface (section IDs, frontmatter
   fields), not via substring match.
2. **Copying production constants into the test.** A test that
   hard-codes the same value the production code uses and compares
   them proves only that someone updated both sites in sync — it
   cannot fail in any interesting way. Either derive a property
   of the constant (length, ordering, prefix relation to another
   constant) or delete the test.
3. **File existence or non-emptiness only.** Reading a file
   already gates readability; asserting only that the file is
   non-empty after a successful read is tautological. Assert at
   least one property of the content.
4. **Mocking the function under test and asserting the mock was
   called.** The mock replaces the very behavior the test claims
   to verify. The assertion proves the test plumbing works, not
   the system.
5. **Loose-outcome assertions any input passes.** Assertions so
   permissive that any input satisfies them — checking only that a
   function returned without error when the function is
   infallible, or that an output is non-empty when the function
   always returns non-empty — gate nothing. Pick an assertion that
   would fail for at least one realistic regression.

When a test you wrote is flaky, investigate the flake. Do not
retry it until green; intermittent failures point at real races,
ordering assumptions, or shared state that will bite again later.

### Commit hygiene

- AI-authored commits identify themselves via the `Co-Authored-By`
  trailer in the commit message footer, naming the model and a
  contact address.
- Prefer narrow, well-scoped commits over sprawling ones. One
  logical change per commit makes review, revert, and bisect
  tractable.

### CI gate (suggestion)

`speccy verify` is designed to run as a CI gate. It fails when the
proof shape is broken (missing requirement coverage, malformed
task state, parser-rejected journal elements) and passes when
intact. Wire it into whichever CI service the project uses —
GitHub Actions, GitLab CI, Jenkins, CircleCI, Buildkite, etc. —
so drift surfaces on every push rather than at ship time. The
gate is informational by design: it tells you when the contract
between intent and shipped behavior is visibly broken; it does
not block anyone from making mistakes.


   Use that body verbatim (heading and all) when writing or
   replacing the section. Do not paraphrase, reorder subsections,
   or add project-specific bullets — project-specific additions
   belong in a sibling section per the preamble line.

6. **Report.** Tell the user what was scaffolded, what was added
   to `AGENTS.md` (if anything), and the final counts (`N
   created, N overwritten`).

7. **Suggest the next step.** `/speccy-plan` to draft the first
   SPEC slice from the now-populated north star.

This recipe does not loop. The bootstrap runs once; subsequent
re-runs of `speccy init --force` only refresh the
shipped skill files and re-upsert the `## Speccy conventions`
section. The `## Product north star` section is never overwritten
once written.
