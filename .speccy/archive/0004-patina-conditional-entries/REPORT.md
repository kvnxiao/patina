---
spec: SPEC-0004
outcome: implemented
generated_at: 2026-06-01T23:56:29Z
---

# REPORT: SPEC-0004 Conditional entries, the file/directory schema split, recurse symlinks, and complete variable layering

<report spec="SPEC-0004">

<coverage req="REQ-001" result="satisfied" scenarios="CHK-001 CHK-002 CHK-003">
T-001 replaced the single `FileMode` taxonomy in
`patina-core/src/config/file_entry.rs` with a kind-typed model: an
`EntryKind` (`File` / `Directory`) and a collapsed mode set where the
table supplies the file/dir context. `[[file]]` accepts `symlink`
(default) / `copy` and the implicit `.tmpl` template render;
`[[directory]]` accepts `symlink` (default, atomic whole-directory) /
`symlink-tree` (per-leaf) / `copy` (recursive). The strings
`symlink-dir` and `copy-tree` no longer parse, and a `[[directory]]`
whose source ends in `.tmpl` is rejected. `RawModule` deserializes both
`[[file]]` and the new `[[directory]]` table-array with per-table
validation; the exactly-one-of `target`/`targets` and non-empty-`targets`
rules apply to both. Mode-rejection errors name the offending mode and
that table's accepted modes (CHK-003: a `[[file]]` with `symlink-tree`
names `symlink-tree`, `symlink`, `copy`). T-011 extended the writer to
emit the collapsed names and the `[[directory]]` table. Retry count:
2 (T-001: 1, T-002: 1).
</coverage>

<coverage req="REQ-002" result="satisfied" scenarios="CHK-004 CHK-005 CHK-018">
T-006 added a plan-time source existence-and-kind check after `when`
gating in `apply::plan`. New `EngineError` variants name the offending
source path and direct the author to the correct table (a `[[file]]`
source that is a directory names `[[directory]]`, and vice versa), and a
missing source raises a typed "source not found" error (DEC-008). Because
`paths::canonicalize` falls back to lexical resolution for an absent
path, existence is an explicit probe rather than a reliance on
canonicalization failing. The check runs in the plan phase before the
advisory lock, the journal flush, and any backup or materialization, so a
mismatched or missing source mutates nothing — CLI tests confirm no
`*.plan`/`*.COMMIT` artifacts are written (CHK-004, CHK-018). The
executor's materialize-time existence check is retained as a TOCTOU
backstop. Retry count: 0.
</coverage>

<coverage req="REQ-003" result="satisfied" scenarios="CHK-006 CHK-007 CHK-019">
T-005 made `apply::plan` evaluate each managed entry's `when` predicate
first, before the source is canonicalized or its targets expanded. A
`when`-false entry is dropped immediately — it pushes nothing into the
operations or resolved-ops vectors, so it produces no planned operation
and no diff line (DEC-004); a `when`-true or no-`when` entry plans
unchanged. The gate is per-entry, above the target loop, so a multi-target
entry's `when` gates all targets together. T-008 closed the `when`-flip
reaping leg (CHK-019): an entry whose `when` flips true→false has its
prior target classified orphaned by `patina status` and reaped on the next
apply with its bytes backed up first. Two consecutive `apply --yes` runs
over unchanged source with `when`-gated entries produce byte-identical
stdout (CHK-007, REQ-021 parity). Retry count: 1 (T-008: 1).
</coverage>

<coverage req="REQ-004" result="satisfied" scenarios="CHK-008 CHK-009 CHK-020 CHK-021">
T-009 removed the narrow single-equality predicate evaluator
(`evaluate_predicate`, `parse_string_literal`) and the
`ProfileError::UnsupportedPredicate` variant from
`patina-core/src/profile.rs`, routing `[[auto_match]]` `when` predicates
through the shared `Engine::eval_when` under a builtins-only resolver
(DEC-006). All four `when` sites — `[[file]]`, `[[directory]]`,
`[[hook]]`, `[[auto_match]]` — now share one MiniJinja engine and one
strict-undefined grammar. Parity holds for predicates over defined
built-ins (CHK-008); the wider grammar (`!=`, `and`, `or`) the narrow
evaluator rejected now evaluates (CHK-009); a `when` accessing an
undefined variable — bare or inside a comparison — is a typed error
naming the variable rather than a silent false (CHK-020, DEC-010); and an
`[[auto_match]]` `when` referencing `patina.profile` (unresolved during
profile resolution) errors rather than silently failing to match
(CHK-021). Retry count: 0.
</coverage>

<coverage req="REQ-005" result="satisfied" scenarios="CHK-010 CHK-011">
T-003 added net-new deserialization of the root manifest's repo-shared
`[variables]` table and its `[profiles.<name>.variables]` tables, reusing
`variables::reject_reserved_keys` so a `patina.*` key in either is
rejected with the existing reserved-key error; absent sections yield
empty results rather than an error. T-004 wired those tables into
`apply::plan`: the root `[variables]` table is pushed as the repo-shared
layer and the active profile's table as the per-profile layer, both
during the same `plan()` pass before module planning. Resolution
precedence is unchanged — the resolver's fixed layer order (CLI >
per-machine > per-profile > per-module > repo-shared > built-ins) is
preserved; this task only populated the two layers `plan()` had omitted.
A root value renders into a `.tmpl` target (CHK-010), the active profile
shadows it (CHK-011), and a per-module key still beats repo-shared.
Retry count: 0.
</coverage>

<coverage req="REQ-006" result="satisfied" scenarios="CHK-012 CHK-013">
T-007 implemented the `symlink-tree` directory executor by factoring the
existing directory-walk-and-link path (`walk_files` →
`link_file`/`ensure_parent`) into the new mode and dispatching it from
`materialize`. It creates one symbolic link per source leaf at the
mirrored target path, leaving intermediate target directories real
(CHK-012); empty source subdirectories produce neither a target directory
nor a link because `walk_files` collects only regular files; a
pre-existing regular file at a leaf target is backed up via
`backup_before_overwrite` then replaced by the link (CHK-013, DEC-007);
and a re-apply over unchanged source is a no-op. Each materialized leaf
returns one `CompletionRecord`, preserving per-leaf granularity for the
commit record and status managed-set. Retry count: 0.
</coverage>

<coverage req="REQ-007" result="satisfied" scenarios="CHK-014 CHK-015">
T-008 taught `patina status` to classify a `symlink-tree` leaf orphaned
when its source leaf is deleted, and the next apply to reap it, reusing
the existing commit-record and removed-entry machinery. The gap was
`status::current_plan_targets`, which inserted only an entry's declared
target; it now (a) walks the live source directory of a `symlink-tree`
entry in the same `walk_files` order and inserts one managed key per
current leaf, and (b) drops entries whose `when` evaluates false on this
host. A deleted source leaf is then absent from the managed set →
classified orphaned (CHK-014) → reaped by the next apply with its bytes
backed up first. Reaping removes leaf links only and never removes an
intermediate directory, even one left empty after its last leaf is reaped
(CHK-015, DEC-005). Retry count: 1 (T-008: 1).
</coverage>

<coverage req="REQ-008" result="satisfied" scenarios="CHK-016 CHK-017">
T-010 updated `patina add` to detect the registered path's kind from
filesystem metadata and write the matching table-array: `[[file]]` for a
file source, `[[directory]]` for a directory source, mode defaulting to
`symlink`. A `--symlink-tree` flag was added to the existing mode group
and `--copy` made valid for a directory source. The mode flags are
kind-checked: `--symlink-tree` on a file source and `--template` on a
directory source are rejected with a typed error naming the incompatible
flag and source kind. A directory source never emits a `[[file]]` entry
(CHK-016) and a directory `--symlink-tree` writes
`mode = "symlink-tree"` (CHK-017). T-011 supplied the `[[directory]]`
writer (`append_directory_entry`) and updated `mode_manifest_str` to the
collapsed taxonomy so the removed `symlink-dir`/`copy-tree` spellings are
never emitted. Retry count: 0.
</coverage>

<coverage req="REQ-009" result="satisfied" scenarios="CHK-022">
T-002 reworked the per-module entry loop in `apply::plan` to consume both
table-arrays and emit them in one deterministic order — every `[[file]]`
entry in declaration order, then every `[[directory]]` entry — assigning
each managed entry a single monotonic `u32` index over the full declared
sequence (files first, then directories) so no `[[file]]` and
`[[directory]]` entry collide on the `entry` index that drives per-entry
rollback (DEC-009); the journal wire-format major was left unchanged.
T-005 placed the `when` gate as step (1) of the fixed order and T-006 the
existence/kind check as step (3), so a `when`-false entry is dropped
before its source is canonicalized or kind-checked — an absent or
wrong-kind source on the current OS for a gated-off entry raises no plan
error (CHK-022), the property that lets one cross-OS repository apply
cleanly everywhere. A `when`-false entry still occupies its index but
emits no operation. Retry count: 1 (T-002: 1).
</coverage>

</report>

## Notes

One non-blocking presentation-fidelity advisory surfaced during
`/speccy-vet` (drift review, round 1): the rendered diff and `--json`
plan show a `[[directory]]` `symlink-tree` entry as a single
whole-directory `symlink <target> -> <source-dir>` line, even though the
executor materializes one link per leaf and the commit record holds one
`ExpectedTarget` per leaf. REQ-006 / REQ-009 `<done-when>` and scenarios
assert only on materialized filesystem state and JSON mode-label
stability — both of which hold — so this is a preview-understatement note
for the human reader, not SPEC drift. A future SPEC could expand the plan
preview to enumerate per-leaf links.

The deferred per-machine variable layer (SPEC Notes) remains out of scope:
the resolver defines it but `plan()` still does not populate it. REQ-005
wired only the repo-shared and per-profile layers, as scoped.
