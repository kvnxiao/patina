---
id: SPEC-0005
slug: patina-skip-if-satisfied
title: Patina skip-if-satisfied — idempotent no-op re-apply via Create/Update/Unchanged classification
status: in-progress
created: 2026-06-02
supersedes: []
---

# SPEC-0005: Patina skip-if-satisfied — idempotent no-op re-apply via Create/Update/Unchanged classification

## Summary

`patina apply` is remove-and-recreate today. Every re-apply unconditionally
removes and relinks each symlink (`link_file`, `patina-core/src/apply/symlink.rs:182`)
and rewrites every copy and rendered template (`copy_file`,
`patina-core/src/apply/copy.rs:31`; `render`,
`patina-core/src/apply/template.rs:50`), regardless of whether the target
already matches the desired state. The product north star's idempotency bar
says re-applying against unchanged source is "a no-op — no writes, byte-identical
stdout"; today it always writes. The diff preview compounds this: `diff.rs` has
no "unchanged" state (`patina-cli/src/output/diff.rs:30`), so every entry renders
as a change even when nothing will change.

This SPEC classifies each target at plan time into **Create**, **Update**, or
**Unchanged**, threads that per-target disposition through the in-memory
operation, the durable plan, and the commit record, and has `execute` skip the
filesystem mutation (and the backup) for Unchanged targets. A re-apply against
fully-satisfied state short-circuits before writing anything — no plan, no commit,
no backup — and reports "up to date" with byte-identical stdout.

The load-bearing subtlety is rollback and crash recovery. Both decide what to
undo purely from backup presence — backup exists → restore bytes; no backup →
treat as a fresh creation and delete (`reverse_operation`,
`patina-core/src/journal/recovery.rs:189`; `revert_target`,
`patina-core/src/rollback/replay.rs:154`). A skipped target takes no backup, so
naïve skipping would make rollback and recovery wrongly delete a pre-existing,
already-correct target. Worse: planning runs *before* `recover_orphans` within a
single invocation, so after a crash, recovery would delete the skipped target
before the same run's execute could act on it — permanent divergence. The fix is
a durable disposition recorded in **both** the fsync'd plan (so recovery is
crash-safe by construction) and the commit record (so `rollback` honors it),
giving each reversal path a third "leave it" arm.

Because Patina is pre-release with no on-disk state to preserve, this SPEC also
resets the on-disk format major version from 2 back to 1 (`FILE_MAJOR_VERSION`,
`patina-core/src/journal/plan.rs:29`) — the disposition fields are added at major
1 with no further bump, codifying a "do not bump per breaking change until v1.0"
policy.

## Goals

<goals>
- Re-applying against fully-satisfied state is a true no-op: no target mutation,
  no backup, no journal write, exit 0, and byte-identical stdout across runs.
- Each resolved target is classified Create / Update / Unchanged at plan time;
  only Create and Update targets are mutated during execute.
- Rollback and crash recovery preserve fidelity for skipped targets: a target the
  apply did not touch is never deleted or restored on the way back.
- The human diff shows only what will change (Unchanged entries omitted, reported
  as a single count line); `--json` reports a per-entry `state` for machine
  consumers.
- The watcher stops churning links on every re-apply, removing the
  reapply-on-own-write feedback loop.
</goals>

## Non-goals

<non-goals>
- No new CLI flag to force re-materialization of already-satisfied targets.
  Skipping is unconditional when a target is satisfied; a force-rewrite affordance
  is a possible follow-up SPEC.
- No persisted classification cache and no reuse of the watcher's drift cache.
  Classification reads live target state on every plan; the SPEC-0003 watch drift
  cache (`DRIFT_CACHE_MAJOR_VERSION`) is a separate structure that this slice
  neither consults nor changes (see DEC-006).
- No change to what "managed" means, nor to the reap policy, beyond ensuring
  Unchanged targets remain recorded in the commit so status and reap continue to
  see them.
- No fix to the pre-existing recovery behavior where a crash before a later
  entry's backup lets recovery delete a not-yet-touched Update target (it
  converges to post-apply state on the same run's re-materialize). This is
  out of scope; see Notes.
- No merge-mode file types, mode-bit comparison, or any v1.0 north-star non-goal.
</non-goals>

## User Stories

<user-stories>
- As an existing-machine maintainer, I re-run `patina apply` after changing
  nothing and expect zero writes and an "up to date" message, so a routine
  re-apply never churns my files or surprises me.
- As a multi-machine syncer running the watch service, I want re-apply to leave
  already-correct links untouched, so the watcher does not storm itself by
  reacting to its own rewrites.
- As a cautious user, I want the diff to show only the entries that will actually
  change, not a wall of entries that are already correct, so the prompt is about
  real changes.
- As a CI script author, I want `patina apply --json` to report a per-entry state
  (create / update / unchanged), so my pipeline can see exactly what a deployment
  would change.
</user-stories>

## Assumptions

<assumptions>
- "Unchanged" is defined to mean exactly what `status` reports as `Clean`, reusing
  the existing `content_hash` (blake3) and `simplified_str` link-target compare —
  the same primitives status uses. There is no third definition of "matches", and
  mode bits are not part of the test (consistent with status, which compares
  content hashes only).
- Classification performs the first live-filesystem reads in the plan path (the
  planner does zero target reads today). The added stat / hash / render cost at
  plan time is accepted for v1.0.
- Templates are rendered at plan time to compare against the target bytes; a
  drifted template is rendered twice per apply (once to classify, once to write).
  Accepted.
- Disposition is a per-target attribute; `copy-tree` and `symlink-tree` expand to
  per-leaf targets and are classified per leaf.
- Patina is pre-release with no on-disk state to preserve, so resetting the format
  major to 1 and making any major-2 journal undecodable is acceptable; no migration
  path is provided and existing dev state is disposable.
- "No-op" means all targets Unchanged AND an empty reap set; a dropped entry or a
  `when` predicate flipping to false still mutates (reap) and is therefore not a
  no-op.
</assumptions>

## Requirements

<requirement id="REQ-001">
### REQ-001: Plan-time classification into Create / Update / Unchanged

At plan time, `apply` classifies each resolved target by comparing live target
state against desired state, producing one of `Create` (target absent),
`Update` (target present but differs), or `Unchanged` (target present and
matches). The satisfied (Unchanged) test is mode-specific and reuses the same
primitives `status` uses to classify `Clean`, so "Unchanged" coincides exactly
with status's "Clean":

- symlink family — target is a symlink whose link target equals the desired
  source (`simplified_str` compare, `patina-core/src/status/classify.rs:84`);
- copy / copy-tree — target is a regular file whose `content_hash`
  (`patina-core/src/journal/record.rs:118`) equals the source's;
- template — target is a regular file whose bytes equal the freshly rendered
  output.

For `copy-tree` and `symlink-tree`, classification is per materialized leaf, not
whole-tree-atomic.

<done-when>
- A symlink target pointing at the desired source classifies Unchanged; pointing
  elsewhere classifies Update; absent classifies Create.
- A copy or template target whose bytes hash-equal the desired output classifies
  Unchanged; differing bytes classify Update; absent classifies Create.
- In a tree, a single drifted leaf classifies Update while the remaining leaves
  classify Unchanged; a leaf present in source but absent at target classifies
  Create.
- A target that `status` would report `Clean` classifies Unchanged for the same
  live state, with no third definition of "matches".
</done-when>

<behavior>
- Given a target symlink already pointing at the desired source, when the plan is
  computed, then the target's disposition is Unchanged.
- Given a copy target whose content differs from the source, when the plan is
  computed, then the disposition is Update.
- Given an absent target, when the plan is computed, then the disposition is
  Create.
- Given a `copy-tree` where one of three leaves drifted, when the plan is
  computed, then exactly that leaf is Update and the other two are Unchanged.
</behavior>

<scenario id="CHK-001">
Given a tempdir repo fixture with a symlink entry whose target already points at
the source, a copy entry whose target bytes match the source, and a template
entry whose target matches the rendered output,
when the plan is computed,
then all three targets classify Unchanged.
</scenario>

<scenario id="CHK-002">
Given the same fixture mutated so the copy target's bytes differ and the symlink
target is deleted,
when the plan is computed,
then the copy target classifies Update and the symlink target classifies Create,
while the template target remains Unchanged.
</scenario>

<scenario id="CHK-003">
Given a `copy-tree` entry materialized to three leaves, with one leaf's bytes
altered out of band,
when the plan is computed,
then the altered leaf classifies Update and the other two classify Unchanged.
</scenario>
</requirement>

<requirement id="REQ-002">
### REQ-002: Per-target disposition threaded through operation, plan, and commit

The disposition is a per-target attribute carried on the in-memory operation
(`ResolvedOperation`, `patina-core/src/apply/engine.rs:143`), persisted on the
durable plan (`PlannedOperation`, `patina-core/src/journal/plan.rs:39`), and
persisted on the commit record (`ExpectedTarget`,
`patina-core/src/journal/record.rs:50`). It survives postcard encode/decode of
both the plan and the apply record.

<done-when>
- Each target in the in-memory plan carries its classified disposition.
- A plan flushed to `<ts>.plan` and re-decoded preserves every target's
  disposition.
- An apply record committed and re-read preserves every target's disposition.
</done-when>

<behavior>
- Given a computed plan with mixed dispositions, when the plan journal is flushed
  and re-decoded, then each target's disposition round-trips unchanged.
- Given a commit record with mixed dispositions, when it is read back, then each
  target's disposition round-trips unchanged.
</behavior>

<scenario id="CHK-004">
Given a `Plan` containing one Create, one Update, and one Unchanged target,
when it is encoded with the version envelope and decoded,
then the decoded plan's per-target dispositions equal the originals.
</scenario>

<scenario id="CHK-005">
Given an `ApplyRecord` containing one Create, one Update, and one Unchanged
target,
when it is encoded and decoded,
then the decoded record's per-target dispositions equal the originals.
</scenario>
</requirement>

<requirement id="REQ-003">
### REQ-003: Execute skips both mutation and backup for Unchanged targets

During execute, a target classified Unchanged is neither materialized nor backed
up: `materialize` is not invoked for it and `backup_before_overwrite`
(`patina-core/src/apply/engine.rs:862`) is not called for it. Create and Update
targets are materialized as today, and Update targets are backed up as today.

<done-when>
- An Unchanged symlink target is not removed or recreated; its inode/mtime is
  unchanged across the re-apply.
- An Unchanged copy or template target is not rewritten; its mtime is unchanged
  across the re-apply.
- No backup entry is written for an Unchanged target.
- A plan mixing an Unchanged entry and an Update entry mutates only the Update
  entry and backs up only the Update entry's prior bytes.
</done-when>

<behavior>
- Given a plan with an Unchanged symlink and an Update copy, when execute runs,
  then the symlink is not touched and no backup is written for it, while the copy
  is overwritten and its prior bytes are backed up.
</behavior>

<scenario id="CHK-006">
Given a repo applied once, then mutated so exactly one of two entries drifts,
when `patina apply` re-runs and is confirmed,
then the untouched entry's mtime is byte-for-byte unchanged and the run's backup
directory contains no entry for it, while the drifted entry is updated and its
prior bytes are backed up.
</scenario>
</requirement>

<requirement id="REQ-004">
### REQ-004: Unchanged targets remain recorded in a written commit

When a commit record is written (any apply that is not a full no-op), Unchanged
targets are recorded in it alongside Create and Update targets, so `status`
reports them managed and `reap_orphans` (`patina-core/src/apply/engine.rs:996`)
never removes them. status and reap iterate the committed `ApplyRecord` target
list joined against the live plan; an Unchanged target absent from the record
would silently drop out of status, so it must be recorded.

<done-when>
- After a partial apply that skipped an Unchanged target, the new commit record's
  target list includes that target.
- `patina status` reports the skipped target as `Clean`, not `Orphaned` or
  missing from the report.
- A subsequent apply does not reap the skipped target.
</done-when>

<behavior>
- Given a partial apply that skipped an Unchanged target, when `status` runs, then
  the target appears in the report as `Clean`.
- Given the same state, when the next apply's reap phase runs, then the target is
  not removed.
</behavior>

<scenario id="CHK-007">
Given a repo applied once, then drifted so one entry is Update and another stays
Unchanged, then re-applied,
when `patina status` runs,
then the Unchanged entry's target is reported `Clean` and is present in the
status output.
</scenario>
</requirement>

<requirement id="REQ-005">
### REQ-005: Crash recovery leaves plan-recorded Unchanged targets untouched

`recover_orphans` (`patina-core/src/journal/recovery.rs:101`) gains a third arm
ahead of the backup-presence branch: a target whose orphan-plan disposition is
Unchanged is left in place, regardless of backup presence. Create targets with no
backup are still deleted and Update targets with a backup are still restored
(unchanged behavior). Because the plan is fsync'd before any mutation
(`flush_plan_and_fsync`, `patina-core/src/apply/engine.rs:840`), this holds at any
crash point.

<done-when>
- An orphan plan whose target is marked Unchanged is preserved by recovery even
  though no backup exists for it.
- An orphan plan whose target is marked Create with no backup is still deleted.
- An orphan plan whose target is marked Update with a backup is still restored
  from the backup.
</done-when>

<behavior>
- Given an orphan plan with an Unchanged-marked target that pre-exists and matches
  the source, when recovery runs, then the target is left byte-for-byte unchanged.
- Given an orphan plan with a Create-marked target and no backup, when recovery
  runs, then the target is removed.
</behavior>

<scenario id="CHK-008">
Given a crafted orphan `<ts>.plan` (no COMMIT, no ROLLED_BACK) containing an
Unchanged-marked target that exists and matches, with no backup present,
when `recover_orphans` runs,
then the target still exists and is byte-for-byte unchanged.
</scenario>

<scenario id="CHK-009">
Given a crafted orphan plan containing a Create-marked target with no backup,
when `recover_orphans` runs,
then the target is deleted.
</scenario>
</requirement>

<requirement id="REQ-006">
### REQ-006: Rollback leaves commit-recorded Unchanged targets untouched

`revert_target` (`patina-core/src/rollback/replay.rs:154`) gains the same third
arm: a commit-recorded target whose disposition is Unchanged is left in place.
Create targets in the same commit are deleted and Update targets are restored from
backup (unchanged behavior).

<done-when>
- `patina rollback` of a committed apply leaves every Unchanged target in place.
- Create targets from that commit are deleted by the rollback.
- Update targets from that commit are restored to their pre-apply bytes.
</done-when>

<behavior>
- Given a committed apply with a mix of Create, Update, and Unchanged targets,
  when `patina rollback` runs, then Unchanged targets are untouched, Create
  targets are deleted, and Update targets are restored.
</behavior>

<scenario id="CHK-010">
Given a repo applied to produce one Create, one Update, and one Unchanged target,
then `patina rollback`,
when the rollback completes,
then the Unchanged target is byte-for-byte identical to its pre-rollback state,
the Create target is absent, and the Update target holds its pre-apply bytes.
</scenario>
</requirement>

<requirement id="REQ-007">
### REQ-007: Full no-op writes nothing to disk

When every target classifies Unchanged and the reap set is empty, `apply` writes
nothing for that invocation: no `<ts>.plan`, no `<ts>.COMMIT`, no backup cycle.
The prior commit record remains authoritative for status and rollback. The
invocation exits 0. (`recover_orphans` still runs first, so a prior crash is still
cleaned up.)

<done-when>
- A fully-satisfied apply creates no new file under the journal directory.
- A fully-satisfied apply creates no new backup cycle directory.
- The invocation exits 0.
- The state directory is byte-for-byte identical before and after the no-op
  invocation (modulo any orphan recovery that legitimately ran).
</done-when>

<behavior>
- Given a fully-satisfied repo with no entry dropped or `when`-flipped, when
  `apply` runs, then the journal and backups directories gain no new entries.
</behavior>

<scenario id="CHK-011">
Given a repo applied once, with the source unchanged afterward,
when `patina apply` runs a second time,
then no new `*.plan`/`*.COMMIT` file appears in the journal directory and no new
backup cycle directory appears.
</scenario>
</requirement>

<requirement id="REQ-008">
### REQ-008: Fully-satisfied apply prints a deterministic up-to-date result

A fully-satisfied apply prints a deterministic, byte-identical result indicating
the targets are up to date, with no timestamps, PIDs, random IDs, or state-dir
paths. Two consecutive fully-satisfied applies produce byte-identical stdout,
extending the existing determinism contract (REQ-021 / CHK-035,
`patina-cli/tests/deterministic_stdout.rs`).

<done-when>
- A fully-satisfied apply emits an up-to-date message via the `Reporter` layer
  (not `println!`).
- Two consecutive fully-satisfied applies produce byte-identical stdout.
- The message contains no varying content (no timestamp, PID, or absolute state
  path).
</done-when>

<behavior>
- Given a fully-satisfied repo, when `apply` runs twice, then the two stdout
  captures are byte-identical and both indicate the targets are up to date.
</behavior>

<scenario id="CHK-012">
Given a fully-satisfied repo,
when `patina apply` is run twice and stdout captured,
then the two captures are byte-identical and contain the up-to-date message.
</scenario>
</requirement>

<requirement id="REQ-009">
### REQ-009: Confirmation prompt skipped when there is nothing to apply

When the plan has no Create or Update targets and nothing to reap, the interactive
diff-and-prompt confirmation is not shown and `apply` completes without reading
stdin. When at least one Create/Update target or a reap exists, the prompt behaves
as today.

<done-when>
- A full no-op on an interactive TTY presents no confirmation prompt and reads no
  stdin.
- A plan with at least one Create/Update target (or a reap) still presents the
  prompt as today.
</done-when>

<behavior>
- Given a fully-satisfied repo and an interactive reporter, when `apply` runs,
  then no confirmation prompt is issued.
- Given a repo with one drifted entry and an interactive reporter, when `apply`
  runs, then the confirmation prompt is issued as today.
</behavior>

<scenario id="CHK-013">
Given a fully-satisfied repo and a test reporter that records prompt invocations,
when `apply` runs,
then the reporter records zero prompt invocations and the run completes.
</scenario>
</requirement>

<requirement id="REQ-010">
### REQ-010: Human diff omits Unchanged bodies and prints one summary count line

In a partial apply, the rendered diff body contains per-entry blocks only for
Create and Update targets; Unchanged targets produce no per-entry block. The diff
prints exactly one deterministic summary line stating the count of Unchanged
targets.

<done-when>
- For a plan with Create/Update and Unchanged targets, only the Create/Update
  targets render a per-entry diff block.
- Exactly one summary line reporting the Unchanged count is printed.
- The diff output is deterministic across runs for the same plan.
</done-when>

<behavior>
- Given a plan with one Update and three Unchanged targets, when the diff renders,
  then only the Update target has a block and a single line reports `3` unchanged.
</behavior>

<scenario id="CHK-014">
Given a plan with one Update target and three Unchanged targets,
when the diff is rendered,
then the snapshot shows exactly one per-entry block (the Update) and one summary
line stating three unchanged.
</scenario>
</requirement>

<requirement id="REQ-011">
### REQ-011: `--json` plan array carries a per-entry `state` field

Each entry in the `--json` plan array (`json_envelope`,
`patina-cli/src/cmd/apply.rs:239`) carries a `state` field with value `create`,
`update`, or `unchanged`, derived purely from the target's disposition. The field
is deterministic: byte-identical across runs for the same plan and live state, and
is part of the deterministic-stdout contract.

<done-when>
- Every `--json` plan entry has a `state` field whose value matches the target's
  disposition.
- The `--json` plan output is byte-identical across two runs against unchanged
  state.
</done-when>

<behavior>
- Given a mixed plan, when `apply --json` runs, then each plan entry's `state`
  equals its classified disposition.
</behavior>

<scenario id="CHK-015">
Given a fixture producing one Create, one Update, and one Unchanged target,
when `patina apply --json` runs,
then the plan array entries carry `state` values `create`, `update`, and
`unchanged` matching their targets, and a second run is byte-identical.
</scenario>
</requirement>

<requirement id="REQ-012">
### REQ-012: Fully-satisfied no-op emits the standard `--json` result envelope

A fully-satisfied `apply --json` emits the standard result envelope shape (the
same top-level structure as a changing apply) with zero-change counts, and the
plan array lists every entry with `state` `unchanged`. "No writes" is a filesystem
property; the JSON result is still produced.

<done-when>
- A fully-satisfied `apply --json` emits the standard envelope shape, not a
  reduced or special-cased shape.
- The result's change counts are all zero.
- The plan array lists every managed entry with `state` `unchanged`.
</done-when>

<behavior>
- Given a fully-satisfied repo, when `apply --json` runs, then the result object
  has the standard shape with zero change counts and an all-`unchanged` plan
  array.
</behavior>

<scenario id="CHK-016">
Given a fully-satisfied repo,
when `patina apply --json` runs,
then the emitted envelope has the standard top-level shape, zero change counts,
and every plan entry's `state` is `unchanged`.
</scenario>
</requirement>

<requirement id="REQ-013">
### REQ-013: On-disk format is major version 1

`FILE_MAJOR_VERSION` (`patina-core/src/journal/plan.rs:29`) is `1`. The plan and
apply-record layouts carrying the new disposition fields encode and decode at
major 1 with no further bump. A byte stream written at major 2 fails to decode
with a version-mismatch error rather than being silently misread
(`decode_envelope` refuses `found > supported`,
`patina-core/src/version_envelope.rs:142`). The pre-release "hold at major 1, do
not bump per breaking change until v1.0" policy is recorded in `AGENTS.md`.

<done-when>
- `FILE_MAJOR_VERSION` is `1`.
- A `Plan` and an `ApplyRecord` with disposition fields round-trip through encode/
  decode at major 1.
- A buffer prefixed with major `2` fails to decode with a version-mismatch error.
- `AGENTS.md` records the pre-release no-bump policy (verified by review).
</done-when>

<behavior>
- Given a `Plan`/`ApplyRecord` encoded at major 1 with dispositions, when decoded,
  then it round-trips.
- Given a buffer whose envelope major is 2, when decoded by the current code, then
  it returns a version-mismatch error.
</behavior>

<scenario id="CHK-017">
Given a `Plan` and an `ApplyRecord` encoded with the current version envelope,
when their envelope major bytes are read,
then the value is 1, and decoding round-trips the dispositions.
</scenario>

<scenario id="CHK-018">
Given a buffer constructed with envelope major byte 2 wrapping an otherwise valid
payload,
when the current decoder reads it,
then it returns a version-mismatch error rather than a decoded value.
</scenario>
</requirement>

## Decisions

<decision id="DEC-001">
The disposition marker is recorded in **both** the durable plan and the commit
record, not in one alone. Planning runs before `recover_orphans` within a single
invocation, so a naïve skip (no marker, no backup) would let recovery delete the
skipped target before the same run's execute could act — permanent divergence.
Recording the disposition in the fsync'd plan makes recovery crash-safe by
construction; recording it in the commit makes `rollback` honor it. Rejected
alternatives: a commit-only marker (recovery has no commit for an orphan, so a
crash still deletes the skipped target) and backing up unchanged targets so the
existing reversal logic restores them (writes to the state dir on every partial
apply and reads as muddy intent — backing up something never overwritten).
</decision>

<decision id="DEC-002">
Tree modes (`copy-tree`, `symlink-tree`) classify per materialized leaf, not
whole-tree-atomic. The executors already walk and write per leaf, and the commit
already records one `ExpectedTarget` per leaf, so per-leaf disposition fits the
existing model with no structural change and is what actually removes churn (one
drifted leaf out of many yields one write, not a full-tree rewrite). Leaf
additions classify as Create; leaf removals continue to flow through reap.
</decision>

<decision id="DEC-003">
The human diff omits Unchanged entries and reports them as a single count line;
the `--json` plan array lists Unchanged entries with `state: "unchanged"`. The
asymmetry is deliberate: human output optimizes for scannability (the diff answers
"what will change", and the full inventory is `patina status`'s job), while machine
output optimizes for completeness.
</decision>

<decision id="DEC-004">
A fully-satisfied no-op still emits the standard `--json` result envelope with
zero-change counts rather than a reduced shape. "No writes" is a filesystem
property, not a stdout property; a special-cased no-op shape would force every CI
consumer to branch. The short-circuit therefore computes and reports the plan,
then returns before writing the plan, commit, or backups.
</decision>

<decision id="DEC-005">
The `FILE_MAJOR_VERSION` reset from 2 to 1 is folded into this SPEC because this
slice already rewrites both wire types that carry the version (`Plan` via
`PlannedOperation`, `ApplyRecord` via `ExpectedTarget`). Patina is pre-release with
no on-disk state to preserve, so making major-2 journals undecodable is acceptable
and no migration is provided. The durable policy — hold the on-disk major at 1 and
do not bump per breaking change until v1.0 — is recorded in `AGENTS.md` so it
outlives this slice.
</decision>

<decision id="DEC-006">
Plan-time classification reads live filesystem state and does not consult or
populate the SPEC-0003 watch drift cache. A skip decision must reflect ground
truth at plan time; a cache hit would still need live re-validation to be safe (a
stale hit would skip a needed write and diverge), so reuse buys little on the
correctness-critical path. The dominant `patina apply` invocation runs with no
watcher and therefore no warm cache, and keeping the core plan path independent of
the watch subsystem avoids a cross-component dependency and its lock-coordination
surface. The live read matches the cost `status` already pays and accepts (per the
assumptions). If plan-time hashing later profiles as a real bottleneck, prefer a
classifier-local mtime/size pre-filter over a cross-component cache dependency — a
post-v1.0 follow-up, not this slice.
</decision>

<decision id="DEC-007">
Tree modes (`copy-tree`, `symlink-tree`) carry disposition at two granularities,
not one. The durable `PlannedOperation` keeps **one op per declared tree target**
(the existing whole-directory shape, `engine.rs` `assemble_plan_operations` /
`planned_operation`) carrying an **aggregate** disposition: Unchanged iff every
materialized leaf is Unchanged, otherwise Update (a present tree with any drifted
or new leaf) or Create (an absent target). Per-leaf disposition is computed at plan
time, carried in-memory on `ResolvedOperation` for the execute write-skip and the
per-leaf diff/`--json` reporting, and recorded **per leaf** on the commit
`ExpectedTarget` (already per-leaf) for status and rollback fidelity.

Execute consequences: a tree op whose aggregate is Unchanged is skipped entirely
(no backup, no write). A tree op with any drifted leaf is **backed up as a whole
directory** — today's `backup_before_overwrite(<dir>)` model, which `clone_entry`
captures recursively and which populates each leaf's mirror path — and then only
its drifted leaves are (re)written; clean leaves are not rewritten, so their
inode/mtime is preserved (the churn-removal goal of DEC-002, "one drifted leaf
yields one write"). Recovery (REQ-005) reads the per-op aggregate from the fsync'd
plan and reverses whole-directory for trees (whole-tree backup → restore;
aggregate Unchanged → leave; aggregate Create with no backup → delete), so recovery
needs no per-leaf plan entries and stays crash-safe by construction. Rollback
(REQ-006) reads the per-leaf commit dispositions: Unchanged leaves are left, Update
leaves are restored from the whole-tree backup at their mirror path, Create leaves
are deleted.

Trade-off accepted: a partially-drifted tree still writes a whole-tree backup, so
clean leaves' prior bytes are captured in that backup. REQ-003's leaf-granular
"no backup for an Unchanged target" therefore holds exactly for single-target
entries and per-tree for tree modes (a fully-clean tree takes no backup at all).
Rejected: a fully per-leaf durable plan (one `PlannedOperation` per leaf with
per-leaf backup and per-leaf recovery) — it is the wire-format and recovery
restructure DEC-002 explicitly rules out, buys no scenario-tested behavior (no
`<scenario>` exercises tree-leaf-level backup, recovery, or rollback), and trades
the proven whole-directory reversal model for a larger correctness surface.
</decision>

## Notes

Rejected framings considered during brainstorm:

- **Whole-plan short-circuit only (no per-target skip).** Classify, but act only on
  the all-Unchanged case; mixed applies keep today's remove-and-recreate for every
  entry. Touches only the planner, a short-circuit, and stdout. Rejected: it
  delivers no honest per-entry diff, no per-entry no-churn in mixed applies, and no
  subsumption of the `EEXIST`/idempotency edge-case family — the classification
  would exist but go mostly unused.
- **Back-up-unchanged-anyway.** Skip the mutation but still snapshot Unchanged
  targets so the existing reversal logic restores them; recovery/rollback/wire
  untouched. Rejected: writes to the state dir on every partial apply, reads as
  muddy intent, and still needs the short-circuit anyway — more conceptual debt for
  fewer touched files, and the wire change it avoids is free pre-release.
- **Disposition in the commit only.** Rejected for the crash-safety reason in
  DEC-001.

Adjacent observation (out of scope, do not fix here): `recover_orphans` iterates
the whole plan and deletes any no-backup target. For an Update target in a
multi-entry plan, a crash before that entry's backup means recovery deletes the
not-yet-touched pre-existing target — but the same invocation's re-apply
re-materializes it to post-apply state, so the filesystem still converges on the
pre-apply-or-post-apply guarantee. This is pre-existing behavior, not introduced by
this SPEC.

## Open Questions

None — all framing questions were resolved before decomposition (see Decisions).

## Changelog

<changelog>
| Date | Author | Summary |
| --- | --- | --- |
| 2026-06-02 | kevin-xiao | Initial SPEC: Create/Update/Unchanged classification, skip-if-satisfied execute, durable disposition for rollback/recovery fidelity, no-op short-circuit, diff/`--json` state, `FILE_MAJOR_VERSION` reset to 1. |
| 2026-06-02 | kevin-xiao | Resolved open question a → DEC-006: classification reads live state, no watch drift-cache reuse. |
| 2026-06-02 | kevin-xiao | Added DEC-007 (decompose): tree-mode disposition is per-op aggregate on the durable plan + per-leaf on the commit record; per-leaf write-skip with whole-tree backup/recovery. |
</changelog>
