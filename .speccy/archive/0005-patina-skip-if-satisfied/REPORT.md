---
spec: SPEC-0005
outcome: implemented
generated_at: 2026-06-03T00:00:00Z
---

# REPORT: SPEC-0005 Patina skip-if-satisfied — idempotent no-op re-apply via Create/Update/Unchanged classification

<report spec="SPEC-0005">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002 CHK-003">
T-003 added the plan-time classifier in `patina-core/src/apply/classify.rs`,
factoring the symlink `simplified_str` compare and the content-hash compare
out of `status::classify` into a shared `pub(crate)` seam. Templates are
rendered at plan time and their bytes compared to the target. T-004 wired the
classifier into `assemble_plan_operations`, classifying each resolved leaf and
computing a per-op aggregate for tree modes. Unit tests cover all three mode
families × {Create, Update, Unchanged} and a property tie confirming any state
`status` reports `Clean` classifies `Unchanged`. Integration tests in
`patina-cli/tests/skip_if_satisfied.rs` cover CHK-001, CHK-002, and CHK-003
(single-entry and copy-tree fixtures). Retry count: 0.
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-004 CHK-005">
T-001 introduced `Disposition { Create, Update, Unchanged }` with a label
method. T-002 added `disposition: Disposition` to `PlannedOperation` and
`ExpectedTarget`, updating all constructors and test fixtures; T-004 replaced
the T-002 placeholder with real classification. Round-trip tests in
`patina-core` assert that both a `Plan` and an `ApplyRecord` with mixed
dispositions encode, survive the version envelope at major 1, and decode
unchanged. Retry count: 0.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-006">
T-005 added the Unchanged write-skip path in the materialize loop for both
single-target entries and tree ops. Unchanged single entries skip
`backup_before_overwrite` and `materialize` entirely; a fully-Unchanged tree
op is skipped as a unit. The integration test in `skip_if_satisfied.rs`
verifies that after a partial re-apply the untouched entry's mtime is unchanged
and the backup directory contains no entry for it. Retry count: 0.
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-007">
T-005 reworked `build_apply_record` so Unchanged targets — which produce no
`CompletionRecord` — are still added to the commit `ExpectedTarget` list from
the resolved plan. The integration test for CHK-007 runs `patina status` after
a partial re-apply and asserts the Unchanged entry appears as `Clean`; a
follow-on apply does not reap it. Retry count: 0.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-008 CHK-009">
T-007 added the Unchanged arm in `reverse_operation`
(`patina-core/src/journal/recovery.rs`) ahead of the backup-presence branch.
Crafted orphan-plan unit tests cover CHK-008 (Unchanged target with no backup
is left in place) and CHK-009 (Create target with no backup is deleted). The
fsync-before-mutation ordering ensures this holds at any crash point. Retry
count: 0.
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-010">
T-008 added the Unchanged arm in `revert_target`
(`patina-core/src/rollback/replay.rs`) and threaded each target's disposition
through `replay_entry` and `group_by_entry`. The end-to-end test in
`skip_if_satisfied.rs` drives a real apply then rollback with mixed dispositions
and asserts the Unchanged target is preserved, the Create target is absent, and
the Update target holds its pre-apply bytes (CHK-010). Retry count: 0.
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-011">
T-006 added the all-Unchanged + empty-reap early-exit in `execute`, returning
before `flush_plan_and_fsync`. The integration test for CHK-011 runs `patina
apply` twice against an unchanged source and asserts that no new `*.plan` or
`*.COMMIT` file appears in the journal directory and no new backup cycle
directory is created. Retry count: 0.
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-012">
T-006 added an `up_to_date` signal on `ApplyResult` and wired it to a
deterministic "Already up to date." line emitted through the `Reporter` layer
in `patina-cli/src/cmd/apply.rs`. The line contains no timestamp, PID, random
ID, or absolute state path. The deterministic-stdout integration test
(`deterministic_stdout.rs`) captures two consecutive fully-satisfied apply runs
and asserts byte-identical stdout (CHK-012). Retry count: 0.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-013">
T-006 gates the diff-and-prompt block on the presence of at least one
Create/Update target or a non-empty reap set; a full no-op bypasses it. The
test for CHK-013 uses a test reporter that records prompt invocations and
asserts zero prompts fired on a fully-satisfied apply. Retry count: 0.
</coverage>

<coverage req="REQ-010" result="satisfied" scenarios="CHK-014">
T-009 modified the diff renderer (`patina-cli/src/output/diff.rs`) to skip
Unchanged entries' per-entry blocks and append a single deterministic summary
line of the form `N file(s) already up to date.`. An `insta` snapshot test
covers CHK-014 (one Update + three Unchanged: exactly one block rendered, one
summary count line). Retry count: 0.
</coverage>

<coverage req="REQ-011" result="satisfied" scenarios="CHK-015">
T-010 added a `state` field to each `--json` plan array entry, derived from the
target's `Disposition` label method (the T-001 single-mapping site). Tree modes
list per-leaf entries with per-leaf `state`. The integration test for CHK-015
runs `apply --json` against a mixed-disposition fixture and asserts `create`,
`update`, and `unchanged` appear correctly; a second run is byte-identical.
Retry count: 0.
</coverage>

<coverage req="REQ-012" result="satisfied" scenarios="CHK-016">
T-010 wired the no-op path to still compute and emit the standard JSON result
envelope with zero change counts and an all-`unchanged` plan array. The test
for CHK-016 runs `apply --json` on a fully-satisfied repo and asserts the
standard top-level shape, zero counts, and every entry's `state` is
`"unchanged"`. Retry count: 0.
</coverage>

<coverage req="REQ-013" result="satisfied" scenarios="CHK-017 CHK-018">
T-001 reset `FILE_MAJOR_VERSION` from `2` to `1` in
`patina-core/src/journal/plan.rs` and recorded the pre-release no-bump policy
in `AGENTS.md`. T-002 confirmed both `Plan` and `ApplyRecord` with disposition
fields round-trip at major 1 (CHK-017). The existing `decode_envelope` guard
(`version_envelope.rs`) already refuses `found > supported`; T-001 updated the
test to present a major-2 buffer and assert `JournalError::VersionMismatch`
(CHK-018). Retry count: 0.
</coverage>

</report>
