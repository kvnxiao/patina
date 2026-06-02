---
spec: SPEC-0005
spec_hash_at_generation: bc911ad85e3dfb43bcd097692580590899160ab2bc74a113167e0a364c8cffcd
generated_at: 2026-06-02T18:37:41Z
---
# Tasks: SPEC-0005 Patina skip-if-satisfied — idempotent no-op re-apply via Create/Update/Unchanged classification

<task id="T-001" state="completed" covers="REQ-013">
## Introduce the `Disposition` type and reset the on-disk format major to 1

Add a `Disposition { Create, Update, Unchanged }` enum to `patina-core`,
deriving `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize` (it
rides the `postcard` wire on `PlannedOperation` and `ExpectedTarget` in
T-002). Give it one `&'static str` label method returning `"create"` /
`"update"` / `"unchanged"` — define this mapping once on the type, since
the same words become the `--json` `state` value in T-010 (per the
"enum → label" rule in `rust-let-types-carry-invariants.md`). Its home is
under `patina-core/src/journal/` (sibling to the two wire types that will
carry it); re-export it from `patina-core/src/lib.rs` next to
`ExpectedTarget`.

Reset `FILE_MAJOR_VERSION` from `2` to `1` (`patina-core/src/journal/plan.rs:29`).
Patina is pre-release with no on-disk state to preserve (assumptions), so
a major-2 journal becoming undecodable is acceptable and no migration is
provided. The existing `decode_envelope` already refuses `found > supported`
(`patina-core/src/version_envelope.rs:142`), so a buffer prefixed with major
`2` now fails with `JournalError::VersionMismatch`. Audit the `plan.rs` and
`record.rs` envelope tests: the ones that reference `FILE_MAJOR_VERSION` are
robust, but any test that hard-codes the literal `2` must be updated.

Record the durable policy in `AGENTS.md` (DEC-005): hold the on-disk format
major at `1` and do not bump per breaking change until v1.0. Place it where
the version-envelope / journal conventions live so it outlives this slice.

<task-scenarios>
Given the `Disposition` enum at HEAD after this task,
when its label method is called for each variant,
then it returns `"create"`, `"update"`, and `"unchanged"` respectively (one
mapping site, asserted by unit test).

Given a `Plan` or `ApplyRecord` buffer whose envelope major byte is set to
`2` wrapping an otherwise valid payload (CHK-018),
when the current decoder reads it,
then it returns `JournalError::VersionMismatch` rather than a decoded value.

Given the journal source at HEAD,
when `FILE_MAJOR_VERSION` is read,
then it is `1`.

Suggested files: `patina-core/src/journal/disposition.rs` (new),
`patina-core/src/journal/mod.rs`, `patina-core/src/lib.rs`,
`patina-core/src/journal/plan.rs`, `patina-core/src/journal/record.rs`,
`AGENTS.md`
</task-scenarios>
</task>

<task id="T-002" state="completed" covers="REQ-002 REQ-013">
## Thread `disposition` onto `PlannedOperation` and `ExpectedTarget` with round-trip coverage

Add a `disposition: Disposition` field to every `PlannedOperation` variant
(`patina-core/src/journal/plan.rs:39`) and to every `ExpectedTarget` variant
(`patina-core/src/journal/record.rs:50`), updating their constructors
(`PlannedOperation::symlink/render/copy`, `plan.rs:65`) and accessors
(`record.rs`) accordingly. Per DEC-007, `PlannedOperation` carries a
**per-op** disposition (an aggregate for tree modes) and `ExpectedTarget`
carries a **per-leaf** disposition.

This is a field-only, behavior-neutral task: thread a placeholder
disposition (`Disposition::Create`) through `planned_operation`
(`engine.rs:722`) and `build_apply_record` (`engine.rs:934`) so the
workspace compiles; T-004 replaces the placeholder with real classification
and T-005 records real per-leaf dispositions. Update every in-tree
constructor and test that builds these types — the `plan.rs` / `record.rs`
sample fixtures, the `recovery.rs` and `rollback` test helpers that craft
plans and records (`patina-core/src/rollback/mod.rs`,
`patina-core/src/journal/recovery.rs` tests).

Confirm the new fields survive the version envelope at major 1.

<task-scenarios>
Given a `Plan` containing one Create, one Update, and one Unchanged target
(CHK-004),
when it is encoded with the version envelope and decoded,
then the decoded plan's per-op dispositions equal the originals.

Given an `ApplyRecord` containing one Create, one Update, and one Unchanged
target (CHK-005, CHK-017),
when it is encoded and decoded,
then the decoded record's per-leaf dispositions equal the originals and the
envelope major byte is `1`.

Suggested files: `patina-core/src/journal/plan.rs`,
`patina-core/src/journal/record.rs`, `patina-core/src/apply/engine.rs`,
`patina-core/src/journal/probe.rs` (constructor call sites),
`patina-core/src/rollback/mod.rs`
</task-scenarios>
</task>

<task id="T-003" state="pending" covers="REQ-001">
## Plan-time classifier reusing the `status` Clean primitives

Implement a classifier that, given a resolved leaf
`(mode, source, target, rendered-bytes?)`, reads live target state and
returns a `Disposition`. It MUST reuse the exact primitives `status` uses
to classify `Clean`, so "Unchanged" coincides with status's "Clean" with no
third definition of "matches" (assumptions, REQ-001):

- symlink family — `Create` if the target is absent; `Unchanged` if the
  target is a symlink whose link target equals the desired source via the
  `simplified_str` compare (`patina-core/src/status/classify.rs:84`);
  `Update` otherwise;
- copy / copy-tree leaf — `Create` if absent; `Unchanged` if the target is
  a regular file whose `content_hash` (`patina-core/src/journal/record.rs:118`)
  equals the source's; `Update` otherwise;
- template — `Create` if absent; `Unchanged` if the target's bytes equal the
  freshly rendered output; `Update` otherwise.

Factor the symlink/content comparison out of `status::classify`
(`patina-core/src/status/classify.rs`) into a `pub(crate)` seam both call
sites share rather than copying the compare (grep first, per
`rust-let-types-carry-invariants.md`). Add the template arm (render once,
compare bytes). Unit-test the matrix: each mode × {Create, Update,
Unchanged}, plus a property tie that a state `status` calls `Clean`
classifies `Unchanged`.

<task-scenarios>
Given a symlink target pointing at the desired source,
when it is classified,
then the result is `Unchanged`; pointing elsewhere yields `Update`; absent
yields `Create`.

Given a copy/template target whose bytes hash-equal the desired output,
when it is classified,
then the result is `Unchanged`; differing bytes yield `Update`; absent
yields `Create`.

Given a live target state that `status` reports `Clean`,
when the classifier runs on the same state,
then it returns `Unchanged` (shared primitive, no second definition).

Suggested files: `patina-core/src/status/classify.rs`,
`patina-core/src/apply/classify.rs` (new) or sibling module,
`patina-core/src/journal/record.rs` (content_hash reuse)
</task-scenarios>
</task>

<task id="T-004" state="pending" covers="REQ-001 REQ-002">
## Classify at plan time and populate dispositions on the operation and durable plan

Wire the T-003 classifier into `plan` / `assemble_plan_operations`
(`patina-core/src/apply/engine.rs:570`). For each resolved entry, classify
its target(s) and replace T-002's placeholder with the real disposition:

- Single-target entries (`[[file]]` modes, `symlink-dir`) — one disposition
  per target on both the in-memory `ResolvedOperation` and the durable
  `PlannedOperation`.
- Tree modes (`copy-tree`, `symlink-tree`) — per DEC-007, enumerate the
  source leaves at plan time (reuse `walk_files`,
  `patina-core/src/apply/mod.rs:256`), classify each leaf, carry the per-leaf
  dispositions in-memory on `ResolvedOperation` (for the T-005 write-skip and
  the T-009/T-010 per-leaf reporting), and set the single durable
  `PlannedOperation`'s disposition to the **aggregate**: `Unchanged` iff every
  leaf is `Unchanged`, `Create` iff the target is absent, otherwise `Update`.

`ResolvedOperation` (`engine.rs:143`) grows the per-target/per-leaf
disposition carrier this requires. Templates are rendered at plan time to
classify and again at execute time to write — the double render is accepted
(assumptions). Add tempdir plan-level tests asserting the computed
dispositions.

<task-scenarios>
Given a fixture with a symlink already pointing at its source, a copy target
matching the source, and a template target matching the rendered output
(CHK-001),
when the plan is computed,
then all three classify `Unchanged`.

Given that fixture mutated so the copy target's bytes differ and the symlink
target is deleted (CHK-002),
when the plan is computed,
then the copy target is `Update`, the symlink target is `Create`, and the
template target stays `Unchanged`.

Given a `copy-tree` materialized to three leaves with one leaf altered out of
band (CHK-003),
when the plan is computed,
then exactly that leaf is `Update` and the other two are `Unchanged`, and the
durable tree op's aggregate disposition is `Update`.

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-core/src/apply/mod.rs`
</task-scenarios>
</task>

<task id="T-005" state="pending" covers="REQ-003 REQ-004">
## Execute skips write and backup for Unchanged targets, and records Unchanged in the commit

In `execute`'s materialize loop (`patina-core/src/apply/engine.rs:854`):

- Single-target entry classified `Unchanged` — call neither
  `backup_before_overwrite` (`engine.rs:862`) nor `materialize` for it; its
  inode/mtime stay unchanged and no backup entry is written. `Create` and
  `Update` targets are backed up (Update) and materialized exactly as today.
- Tree op (DEC-007) — if the aggregate is `Unchanged`, skip the whole op
  (no backup, no write). Otherwise back up the target directory as a unit
  (today's single `backup_before_overwrite(<dir>)`), then materialize only
  the leaves whose per-leaf disposition is not `Unchanged`; clean leaves are
  not rewritten, so their inode/mtime is preserved. This needs a per-leaf
  write-skip path in the tree executors (`copy_tree`,
  `patina-core/src/apply/copy.rs:48`; `tree_symlink`,
  `patina-core/src/apply/symlink.rs`) driven by the per-leaf dispositions
  T-004 put on `ResolvedOperation`.

Rework `build_apply_record` (`engine.rs:934`) so every managed target —
including `Unchanged` ones that produced no `CompletionRecord` — becomes an
`ExpectedTarget` in the commit, carrying its disposition. Source the
`Unchanged` targets' metadata from the resolved plan (their disposition and
recorded link-target/hash) rather than from `completed`, so `status` reports
them managed and `reap_orphans` (`engine.rs:996`) never removes them.

<task-scenarios>
Given a repo applied once, then mutated so exactly one of two entries drifts
(CHK-006),
when `patina apply` re-runs and is confirmed,
then the untouched entry's mtime is byte-for-byte unchanged and the run's
backup directory holds no entry for it, while the drifted entry is updated and
its prior bytes are backed up.

Given a repo applied once, then drifted so one entry is Update and another
stays Unchanged, then re-applied (CHK-007),
when `patina status` runs,
then the Unchanged entry's target is reported `Clean` and present in the
output, and a subsequent apply's reap phase does not remove it.

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-core/src/apply/copy.rs`, `patina-core/src/apply/symlink.rs`,
`patina-cli/tests/skip_if_satisfied.rs` (new)
</task-scenarios>
</task>

<task id="T-006" state="pending" covers="REQ-007 REQ-008 REQ-009">
## Full no-op short-circuit, deterministic up-to-date reporting, and skipped prompt

When every target classifies `Unchanged` and the reap set is empty, `apply`
must write nothing for the invocation: no `<ts>.plan`, no `<ts>.COMMIT`, no
backup cycle. In `execute` (`patina-core/src/apply/engine.rs:755`), after
`recover_orphans` runs and the reap set is computed, detect the
all-Unchanged + empty-reap case and return **before** `flush_plan_and_fsync`
(`engine.rs:840`). The prior commit record remains authoritative. Surface the
no-op via the `ApplyResult` (`engine.rs:211`) so the CLI can distinguish it
(e.g. an `up_to_date` signal on `Applied`, or a dedicated variant).
`recover_orphans` still runs first, so a prior crash is still cleaned up.

In the CLI apply path (`patina-cli/src/cmd/apply.rs`): on a full no-op, do
not show the diff-and-prompt confirmation and do not read stdin (REQ-009),
and print a deterministic "up to date" line through the `Reporter` layer (not
`println!`) with no timestamp, PID, random id, or absolute state path
(REQ-008). A plan with at least one Create/Update target or a reap prompts as
today.

<task-scenarios>
Given a repo applied once with the source unchanged afterward (CHK-011),
when `patina apply` runs a second time,
then no new `*.plan`/`*.COMMIT` file appears in the journal directory and no
new backup cycle directory appears.

Given a fully-satisfied repo (CHK-012),
when `patina apply` is run twice and stdout captured,
then the two captures are byte-identical and contain the up-to-date message.

Given a fully-satisfied repo and a test reporter recording prompt invocations
(CHK-013),
when `apply` runs,
then the reporter records zero prompt invocations and the run completes
without reading stdin.

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-cli/src/cmd/apply.rs`, `patina-cli/tests/skip_if_satisfied.rs`,
`patina-cli/tests/deterministic_stdout.rs`
</task-scenarios>
</task>

<task id="T-007" state="pending" covers="REQ-005">
## Crash recovery leaves plan-recorded Unchanged targets untouched

`reverse_operation` (`patina-core/src/journal/recovery.rs:189`) gains a third
arm ahead of the backup-presence branch: an operation whose plan-recorded
disposition is `Unchanged` is left in place, regardless of backup presence.
`Create` operations with no backup are still deleted and `Update` operations
with a backup are still restored (unchanged behavior). Because the plan is
fsync'd before any mutation (`flush_plan_and_fsync`,
`patina-core/src/apply/engine.rs:840`), this holds at any crash point. Per
DEC-007 the disposition read here is the durable per-op aggregate, so tree
ops reverse whole-directory (aggregate `Unchanged` → leave; aggregate
`Create` with no backup → delete; aggregate `Update` with a whole-tree backup
→ restore) with no per-leaf plan entries.

Add crafted-orphan-plan unit tests (no COMMIT, no ROLLED_BACK).

<task-scenarios>
Given a crafted orphan `<ts>.plan` containing an Unchanged-marked target that
exists and matches the source, with no backup present (CHK-008),
when `recover_orphans` runs,
then the target still exists and is byte-for-byte unchanged.

Given a crafted orphan plan containing a Create-marked target with no backup
(CHK-009),
when `recover_orphans` runs,
then the target is deleted.

Suggested files: `patina-core/src/journal/recovery.rs`
</task-scenarios>
</task>

<task id="T-008" state="pending" covers="REQ-006">
## Rollback leaves commit-recorded Unchanged targets untouched

`revert_target` (`patina-core/src/rollback/replay.rs:154`) gains the same
third arm: a commit-recorded target whose per-leaf disposition is `Unchanged`
is left in place. `Create` targets in the commit are deleted and `Update`
targets are restored from backup (unchanged behavior). Thread each target's
disposition through the rollback path that drives `revert_target` —
`replay_entry` (`patina-core/src/rollback/replay.rs:38`) and
`group_by_entry` (`patina-core/src/rollback/mod.rs:166`) — so the recorded
disposition reaches the revert decision. For tree leaves, the `Update` restore
reads the whole-tree backup at each leaf's mirror path (DEC-007).

Add an end-to-end test driving a real apply → rollback.

<task-scenarios>
Given a repo applied to produce one Create, one Update, and one Unchanged
target, then `patina rollback` (CHK-010),
when the rollback completes,
then the Unchanged target is byte-for-byte identical to its pre-rollback
state, the Create target is absent, and the Update target holds its pre-apply
bytes.

Suggested files: `patina-core/src/rollback/replay.rs`,
`patina-core/src/rollback/mod.rs`,
`patina-cli/tests/skip_if_satisfied.rs`
</task-scenarios>
</task>

<task id="T-009" state="pending" covers="REQ-010">
## Human diff omits Unchanged bodies and prints one summary count line

In the diff renderer (`patina-cli/src/output/diff.rs:30`), per-entry blocks
are emitted only for `Create` and `Update` targets; `Unchanged` targets
produce no per-entry block. Print exactly one deterministic summary line
stating the count of Unchanged targets. For tree modes the count is over
materialized leaves (DEC-003, DEC-007): a drifted tree renders blocks for its
drifted leaves and contributes its clean leaves to the count. Output must be
deterministic across runs for the same plan. Add an `insta` snapshot test.

<task-scenarios>
Given a plan with one Update target and three Unchanged targets (CHK-014),
when the diff is rendered,
then the snapshot shows exactly one per-entry block (the Update) and one
summary line stating three unchanged.

Suggested files: `patina-cli/src/output/diff.rs`,
`patina-cli/tests/` snapshot fixtures
</task-scenarios>
</task>

<task id="T-010" state="pending" covers="REQ-011 REQ-012">
## `--json` plan entries carry a per-entry `state`; the no-op emits the standard envelope

Each entry in the `--json` plan array (`json_envelope`,
`patina-cli/src/cmd/apply.rs:239`) carries a `state` field whose value is the
target's `Disposition` label (`create` / `update` / `unchanged`), derived
purely from the disposition and reusing the T-001 label method. For tree
modes the array lists per-leaf entries with per-leaf `state` (DEC-003,
DEC-007). The field is deterministic and part of the deterministic-stdout
contract.

A fully-satisfied `apply --json` emits the **standard** result envelope shape
(same top-level structure as a changing apply), with zero change counts and a
plan array listing every entry with `state: "unchanged"` (DEC-004) — not a
reduced or special-cased shape. "No writes" is a filesystem property; the JSON
result is still produced.

<task-scenarios>
Given a fixture producing one Create, one Update, and one Unchanged target
(CHK-015),
when `patina apply --json` runs,
then the plan array entries carry `state` values `create`, `update`, and
`unchanged` matching their targets, and a second run is byte-identical.

Given a fully-satisfied repo (CHK-016),
when `patina apply --json` runs,
then the emitted envelope has the standard top-level shape, zero change
counts, and every plan entry's `state` is `unchanged`.

Suggested files: `patina-cli/src/cmd/apply.rs`,
`patina-cli/tests/skip_if_satisfied.rs`,
`patina-cli/tests/deterministic_stdout.rs`
</task-scenarios>
</task>
