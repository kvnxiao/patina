---
spec: SPEC-0004
spec_hash_at_generation: 93c5b32df0aca14d8100a0e76a2f9063ca0021e1334b4f425da693a9c76b45d3
generated_at: 2026-06-01T20:33:56Z
---
# Tasks: SPEC-0004 Conditional entries, the file/directory schema split, recurse symlinks, and complete variable layering

<task id="T-001" state="completed" covers="REQ-001">
## Split the entry schema into kind-typed `[[file]]` / `[[directory]]` table-arrays with collapsed mode names and a `when` field

Restructure `patina-core/src/config/file_entry.rs` and
`patina-core/src/config/mod.rs` to model the two kind-typed
table-arrays from REQ-001 / DEC-001 / DEC-002. Replace the single
`FileMode` taxonomy (`Symlink` / `SymlinkDir` / `Copy` / `CopyTree` /
`TemplateRender`, `file_entry.rs:24`) with a kind-aware model. Introduce
an `EntryKind` (`File` / `Directory`) and a collapsed mode set where the
table supplies the file/dir context: a `[[file]]` accepts `symlink`
(default) / `copy` and the implicit `.tmpl` template render; a
`[[directory]]` accepts `symlink` (default, atomic whole-directory) /
`symlink-tree` (per-leaf) / `copy` (recursive). The strings
`symlink-dir` and `copy-tree` cease to exist as accepted input. A
`[[directory]]` whose `source` ends in `.tmpl` is rejected (template is
file-only). Add an optional `when: Option<String>` field to the
resolved entry struct (raw expression source, mirroring
`HookEntry.when` in `hook_entry.rs:34`); evaluation lands in T-005, so
this task only parses and carries it.

The resolved entry the rest of the engine consumes must expose its
kind, its collapsed mode, its `source`, its non-empty `targets`, and
its optional `when`. Decide the concrete representation (e.g. a single
`ManagedEntry { kind, mode, source, targets, when }` with a unified
mode enum, or two structs) so a source-kind enum cannot pair with an
illegal mode — DEC-001's "illegal states unrepresentable" is the bar.
Wire `RawModule` (`config/mod.rs:150`) to deserialize both `file` and a
new `directory` table-array, with `FileEntry::from_raw`-style
validation per table. The exactly-one-of `target`/`targets` rule and
the non-empty-`targets` rule (`file_entry.rs:106`) apply identically to
both tables. Mode-rejection errors must name the offending mode and the
accepted modes for *that* table (CHK-003 substring contract: a `[[file]]`
with `symlink-tree` names `symlink-tree`, `symlink`, `copy`).

This is the foundational schema change; downstream tasks (T-002, T-005,
T-006, T-007, T-008, T-010, T-011) all consume the new shape. Keep
`FileMode`'s public re-export surface (`config/mod.rs:24-26`) coherent
with the new model — `apply::materialize` (T-007) and the writer (T-011)
both match on it. Update all existing unit tests in `file_entry.rs` and
the parse tests in `config/mod.rs` to the new vocabulary, and add tests
for the new `[[directory]]` table and the `symlink-tree` mode.

Note: callers in `apply/engine.rs`, `apply/mod.rs::materialize`,
`config/writer.rs`, `status/mod.rs`, and `patina-cli/src/cmd/add.rs`
reference the old `FileMode` variants and the `config.files` field;
this task will break their compilation. Land the minimal mechanical
adjustments needed to keep the workspace compiling (e.g. a transitional
`directories` field on `ModuleConfig` and stub dispatch arms), leaving
the behavioural wiring to the dependent tasks. The build must be green
at the end of this task.

<task-scenarios>
Given a module `patina.toml` with a `[[file]]` entry
`source = "zshrc"`, `target = "~/.zshrc"` and no `mode`,
when the manifest is parsed,
then the resolved entry has file kind and symlink mode (CHK-001).

Given a module `patina.toml` with a `[[directory]]` entry
`source = "mpv"`, `target = "~/.config/mpv"`, `mode = "symlink-tree"`,
when the manifest is parsed,
then the resolved entry has directory kind and per-leaf symlink mode
(CHK-002).

Given a `[[file]]` entry declaring `mode = "symlink-tree"`,
when the manifest is parsed,
then parsing fails with a typed error whose message contains the
substring `symlink-tree` and the accepted `[[file]]` modes `symlink`
and `copy` (CHK-003).

Given a `[[directory]]` entry declaring `mode = "symlink-dir"` or
`"copy-tree"`,
when the manifest is parsed,
then parsing fails with a typed error naming the accepted
`[[directory]]` modes `symlink`, `symlink-tree`, `copy`.

Given a `[[directory]]` entry whose `source` ends in `.tmpl`,
when the manifest is parsed,
then parsing is rejected (template render is file-only).

Suggested files: `patina-core/src/config/file_entry.rs`,
`patina-core/src/config/mod.rs`
</task-scenarios>
</task>

<task id="T-002" state="completed" covers="REQ-009 REQ-001">
## Emit both table-arrays in one deterministic files-then-directories order with a single monotonic entry-index space

Rework the per-module entry loop in `apply::plan`
(`apply/engine.rs:256-287`) so it consumes both the `[[file]]` and
`[[directory]]` entries produced by T-001 and emits them in one
deterministic order: every `[[file]]` entry in declaration order, then
every `[[directory]]` entry in declaration order, across all modules as
the modules are already iterated. Assign each managed entry an index
over this full declared sequence as a single monotonic `u32` space
(files first, then directories) — the index that flows into the
per-entry `entry: u32` grouping on `ExpectedTarget` (`journal/record.rs:62`)
and drives atomic per-entry rollback (DEC-009). Today the loop derives
`entry_index` from `enumerate()` over `resolved.operations` in
`execute` (`engine.rs:421`); after the file/directory split the index
must be assigned at plan time over the combined declared sequence and
carried on the `ResolvedOperation` so `execute` records the planned
index rather than re-deriving it, guaranteeing no `[[file]]` and
`[[directory]]` entry collide on an index. The `entry: u32` journal
wire-format is unchanged (DEC-009): do not bump the version envelope.

Extend `planned_operation` (`engine.rs:304`) and the `ResolvedOperation`
struct (`engine.rs:138`) to carry the new collapsed mode and the entry
index. `symlink-tree`'s executor lands in T-007; here, map the new
`[[directory]]` `symlink` default to the existing atomic
directory-symlink `PlannedOperation::symlink`, `[[directory]]` `copy` to
`PlannedOperation::copy`, and leave a clearly-marked dispatch point for
`symlink-tree`. This task does not yet evaluate `when` (T-005) nor add
the source-kind check (T-006); it establishes the ordering and index
space those tasks slot into. Prove the ordering and index monotonicity
with an engine-level unit test over a synthetic multi-entry manifest.

<task-scenarios>
Given a manifest with two `[[file]]` entries and one `[[directory]]`
entry,
when the plan is built,
then both file operations appear before the directory operation, each
block in declaration order.

Given the same manifest,
when entry indices are assigned,
then they form a single monotonic space across both tables (all
`[[file]]` entries, then all `[[directory]]` entries) and no `[[file]]`
and `[[directory]]` entry share an index.

Given a committed apply over both tables,
when the COMMIT record is decoded,
then each `ExpectedTarget.entry` index is unique per declared entry and
the version envelope major is unchanged from before this SPEC.

Suggested files: `patina-core/src/apply/engine.rs`
</task-scenarios>
</task>

<task id="T-003" state="completed" covers="REQ-005">
## Parse `[variables]` and `[profiles.<name>.variables]` from the root `patina.toml`

Add net-new deserialization of the root manifest's repo-shared
`[variables]` table and its `[profiles.<name>.variables]` tables. Today
the root manifest is read only for `[[auto_match]]`
(`profile.rs::load_auto_match_rules`, `profile.rs:262`) and the
`RawRoot` projection there deserializes nothing else
(`profile.rs:280-288`); the per-module `[variables]` table is parsed in
`config/mod.rs` but the *root* `[variables]` and any `[profiles.*]`
section are net-new (per the SPEC assumption at SPEC.md:142-147). Add a
root-manifest parser (in `patina-core/src/config/` alongside the module
parser, or a dedicated root-config module — choose the seam that keeps
`profile.rs` focused on resolution) that returns the raw repo-shared
`[variables]` table and a map of profile-name → that profile's
`[variables]` table, each as a `toml::value::Table`. Reuse
`variables::reject_reserved_keys` (`variables/mod.rs:71`) so a
`patina.*` key in either table is rejected with the existing
reserved-key error, exactly as the module parser already does
(`config/mod.rs:134-136`). A missing root manifest, a missing
`[variables]` table, or a missing `[profiles]` section yields empty
results, not an error (mirror `load_auto_match_rules`'s NotFound
handling).

This task is parse-and-return only — wiring these tables into the
resolver layers during `plan()` is T-004. Cover the parser with unit
tests: a root `[variables]` table is read; a
`[profiles.work.variables]` table is read and keyed by `work`; a
`patina.*` key in either is rejected; absent sections yield empties.

<task-scenarios>
Given a root `patina.toml` with `[variables]` defining `editor = "nvim"`,
when the root config is parsed,
then the repo-shared table contains `editor = "nvim"`.

Given a root `patina.toml` with `[profiles.work.variables]` defining
`editor = "code"`,
when the root config is parsed,
then the per-profile map contains a `work` entry whose table has
`editor = "code"`.

Given a root `[variables]` table with a `patina.os` key,
when the root config is parsed,
then parsing fails with the existing reserved-key error naming
`patina.os`.

Given a root `patina.toml` with no `[variables]` and no `[profiles]`,
when the root config is parsed,
then both the repo-shared table and the per-profile map are empty and no
error is raised.

Suggested files: `patina-core/src/config/mod.rs` (or a new
`patina-core/src/config/root.rs`), `patina-core/src/variables/mod.rs`
</task-scenarios>
</task>

<task id="T-004" state="completed" covers="REQ-005">
## Wire the repo-shared and active-profile variable layers into apply planning

In `apply::plan` (`apply/engine.rs:227`), load the root manifest's
repo-shared `[variables]` table (parsed in T-003) and push it as the
resolver's repo-shared layer via `Resolver::with_repo_shared`
(`variables/mod.rs:224`), and select the active profile's
`[profiles.<name>.variables]` table — the profile name is already
resolved at `engine.rs:238` before the module loop — and push it via
`Resolver::with_per_profile` (`variables/mod.rs:192`). Both pushes
happen during the same `plan()` pass, before module planning, so the
resolver carried in `ResolvedPlan` (`engine.rs:175`) and reused by the
executors, the hook `when` evaluator, and the diff renderer sees them.
The no-profile fallback (empty profile name) selects no per-profile
table. Resolution precedence is already enforced by the resolver's fixed
layer order (`variables/mod.rs:249-266`): CLI > per-machine >
per-profile > per-module > repo-shared > built-ins — this task only
populates the two layers `plan()` omitted, changing no precedence.

Add CLI-level integration tests under `patina-cli/tests/` driving
`PATINA_REPO=<tempdir> patina apply --yes` over a fixture repo with a
`.tmpl` source: a root `[variables]` value renders into the target; the
same key set in the active profile's `[profiles.<name>.variables]`
shadows it; a per-module `[variables]` key still beats repo-shared.

<task-scenarios>
Given a tempdir repo whose root `patina.toml` declares `[variables]`
with `editor = "nvim"` and a module with a `.tmpl` source referencing
`editor`,
when `PATINA_REPO=T patina apply --yes` runs,
then the materialized target contains `nvim` (CHK-010).

Given that same repo plus an active profile `work` whose
`[profiles.work.variables]` sets `editor = "code"`,
when `PATINA_PROFILE=work PATINA_REPO=T patina apply --yes` runs,
then the materialized target contains `code` (CHK-011).

Given a key present in both the root `[variables]` and a module's
`[variables]` table,
when planning renders a template referencing it,
then the module value is used (per-module beats repo-shared).

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-cli/tests/variable_layers.rs`
</task-scenarios>
</task>

<task id="T-005" state="pending" covers="REQ-003 REQ-009">
## Gate a managed entry's plan presence on its `when`, evaluated before canonicalization

Make `apply::plan` evaluate each managed entry's `when` predicate first,
before the source is canonicalized or its targets are expanded. The
plan loop (`apply/engine.rs:268`) currently canonicalizes
unconditionally; insert a `when` gate at the top of the per-entry body
so the fixed REQ-009 order holds: (1) evaluate `when` (if any), (2)
canonicalize source/targets, (3) [kind/existence check lands in T-006].
A `when`-false entry is dropped immediately — it pushes nothing into
`operations` or `resolved_ops`, so it produces no planned operation and
no diff line (DEC-004), and is never canonicalized. An entry with no
`when` always plans. For a multi-target entry the `when` gates all
targets together (the gate is per-entry, above the target loop).

`Engine::eval_when` (`template/mod.rs:162`) is the single evaluator
(it already errors on undefined-variable access at every position — see
T-009 for the REQ-004 uniformity claim). The engine is currently
constructed only in `execute` (`engine.rs:339`); construct an `Engine`
in `plan()` too (it is cheap and clone-shares one `Arc` environment) and
pass it plus the planning `Resolver` into the gate. The entry index
(T-002) is assigned over the full declared sequence independent of
`when`, so a `when`-false entry still occupies its index but emits no
operation — keep that property when dropping the entry.

REQ-021 parity: two consecutive `apply --yes` runs over unchanged source
with `when`-gated entries must produce byte-identical stdout. Add a
CLI integration test for the byte-identical second run (CHK-007) and the
`when`-false-drops-the-target case (CHK-006). The flip-to-false reaping
behaviour (CHK-019) rides on the existing removed-entry orphan path; a
status/reap check is covered once the status managed-set is when-aware
(T-008) — assert here only that a `when`-false entry plans nothing.

<task-scenarios>
Given a module entry carrying `when = "patina.os == 'definitely-not-this-os'"`,
when `PATINA_REPO=T patina apply --yes` runs,
then the entry's target is not created and the run's plan records zero
operations for it (CHK-006).

Given an entry whose `when` equals `patina.os == '<current OS family>'`,
when `PATINA_REPO=T patina apply --yes` runs twice,
then the entry's target is materialized and the second run's stdout is
byte-identical to the first (CHK-007).

Given a multi-target entry with a false `when`,
when planning runs,
then none of its targets are planned; with a true `when`, all of them.

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-cli/tests/conditional_entries.rs`
</task-scenarios>
</task>

<task id="T-006" state="pending" covers="REQ-002 REQ-009">
## Add the plan-time source existence-and-kind check after `when`-gating

After a surviving (`when`-true or no-`when`) entry is canonicalized in
`apply::plan`, validate the canonical source against the entry's
declared kind (T-001) and its existence, raising a typed plan-time
error before the advisory lock, the journal flush, or any mutation. This
is step (3) of the REQ-009 order and runs only on entries that survived
the T-005 `when` gate, so a `when`-false entry on the current OS is
never canonicalized or kind-checked (CHK-022). Add typed error
variant(s) to `EngineError` (`patina-core/src/error.rs:22`): a
source-kind mismatch names the offending source path and directs the
author to the correct table (`[[file]]` source that is a directory →
name `[[directory]]`; `[[directory]]` source that is a file → name
`[[file]]`), and a missing source raises a "source not found" error
naming the source (DEC-008). Because `paths::canonicalize` falls back to
lexical resolution for a non-existent path (SPEC.md:127-134), the
existence check is an explicit `symlink_metadata`/`exists` probe on the
canonical source, not a reliance on canonicalization failing. The
kind check is an `is_file()` / `is_dir()` on the same already-resolved
path (no extra IO pass).

Retain the executor's materialize-time existence check
(`apply/symlink.rs:33-44` `ExecutorError::SourceMissing`, and the
copy/template equivalents) as a TOCTOU backstop (DEC-008) — do not
remove it. Add CLI integration tests: a `[[file]]` pointing at a
directory source exits 1, stderr names the source and `[[directory]]`,
and no `*.plan`/`*.COMMIT` is written (CHK-004); a `[[directory]]`
pointing at a file source directs to `[[file]]` (CHK-005); a `when`-true
entry whose source is absent exits 1 with a "source not found" error and
no journal artifacts (CHK-018); a `when`-false `[[directory]]` with an
absent, wrong-OS source exits 0 with no kind/missing-source error
(CHK-022).

<task-scenarios>
Given a tempdir repo whose `[[file]]` entry has `source = "confdir"`
where `T/<module>/confdir` is a directory,
when `PATINA_REPO=T patina apply --yes` runs,
then the process exits 1, stderr contains `confdir` and `[[directory]]`,
and the state directory has no `*.plan` or `*.COMMIT` for the run
(CHK-004).

Given a `[[directory]]` entry whose `source` is a regular file,
when `PATINA_REPO=T patina apply --yes` runs,
then the process exits 1 and stderr contains `[[file]]` (CHK-005).

Given a `[[file]]` entry with `source = "ghost"` and no `when`, where
`ghost` does not exist on disk,
when `PATINA_REPO=T patina apply --yes` runs,
then the process exits 1, stderr names `ghost` as a missing source, and
no `*.plan`/`*.COMMIT` is written (CHK-018).

Given a `[[directory]]` entry with
`when = "patina.os == 'definitely-not-this-os'"` and a `source` that
does not exist,
when `PATINA_REPO=T patina apply --yes` runs,
then the process exits 0, no target is created, and stderr contains no
missing-source or kind error (CHK-022).

Suggested files: `patina-core/src/apply/engine.rs`,
`patina-core/src/error.rs`,
`patina-cli/tests/source_kind_validation.rs`
</task-scenarios>
</task>

<task id="T-007" state="pending" covers="REQ-006">
## Implement the `symlink-tree` per-leaf directory executor

Add a `symlink-tree` executor that walks a source directory and creates
one symbolic link per leaf file at the mirrored target path, leaving
intermediate target directories real (REQ-006 / DEC-005). The seam
already exists: `per_file_symlink` (`apply/symlink.rs:29`) already walks
a directory source via `walk_files` (`apply/mod.rs:251`, deterministic
sorted order) and links each leaf through `link_file`
(`apply/symlink.rs:111`), which calls `ensure_parent` so intermediate
target directories are created as real directories on demand. Factor the
directory-walk-and-link path into the `symlink-tree` mode and dispatch
it from `materialize` (`apply/mod.rs:210`) on the new directory
`symlink-tree` mode (T-001), wiring the `engine.rs` dispatch point left
in T-002. Empty source subdirectories must produce neither a target
directory nor a link — `walk_files` already collects only regular files,
so an empty subdir yields no entry; confirm and test it. A pre-existing
regular file at a leaf target is backed up by the engine's
`backup_before_overwrite` (`engine.rs:424`, runs ahead of `materialize`)
and then replaced by the link (`link_file` clears the path first); this
is the same overwrite path every mode uses (DEC-007). A re-apply over
unchanged source is a no-op (idempotent).

Each materialized leaf returns one `CompletionRecord`
(`apply/mod.rs:88`); preserve that one-record-per-leaf granularity so
T-008's status managed-set and the commit record (T-002's index space)
track per-leaf targets. Cover with executor unit tests in `symlink.rs`
and a CLI integration test (CHK-012, CHK-013).

<task-scenarios>
Given a `[[directory]]` `symlink-tree` entry whose source contains
`a.conf` and `sub/b.conf`,
when `PATINA_REPO=T patina apply --yes` runs,
then `~/d/a.conf` and `~/d/sub/b.conf` are symbolic links resolving to
the source files, and `~/d` and `~/d/sub` are real directories
(CHK-012).

Given the same entry where `~/d/a.conf` already exists as a regular file
before apply,
when `PATINA_REPO=T patina apply --yes` runs,
then the prior file's bytes are recorded in a backup and `~/d/a.conf` is
afterward a symbolic link to the source (CHK-013).

Given the source additionally contains an empty `empty/` subdirectory,
when the entry materializes,
then `~/d/empty` does not exist.

Given an applied `symlink-tree` over unchanged source,
when `patina apply` runs again,
then it is a no-op for that entry.

Suggested files: `patina-core/src/apply/symlink.rs`,
`patina-core/src/apply/mod.rs`, `patina-core/src/apply/engine.rs`,
`patina-cli/tests/symlink_tree.rs`
</task-scenarios>
</task>

<task id="T-008" state="pending" covers="REQ-007 REQ-003">
## Report and reap `symlink-tree` orphan leaves, and make the status managed-set `when`-aware

Teach `patina status` to classify a `symlink-tree` leaf as orphaned when
its source leaf is deleted, and the next apply to reap it, reusing the
existing commit-record and removed-entry machinery (REQ-007). The commit
record already holds one `ExpectedTarget` per materialized leaf (T-007's
per-leaf `CompletionRecord` → `build_apply_record`, `engine.rs:484`), so
`status::classify` (`status/classify.rs:58`) already classifies each
recorded leaf. The gap is `status::current_plan_targets`
(`status/mod.rs:171`): it inserts each entry's *declared* target via
`manage_key` and (a) does not expand a `symlink-tree` directory entry
into its per-leaf target paths, and (b) does not honour `when`, so a
`when`-false entry's prior targets would wrongly count as still-managed.
Update `current_plan_targets` to (a) for a `symlink-tree` entry, walk
the live source directory (same `walk_files` order as T-007) and insert
one `manage_key` per current leaf target, and (b) drop entries whose
`when` evaluates false on this host (reusing the T-005 gate logic /
`Engine::eval_when`) so a flipped-to-false entry's targets become
orphaned. A deleted source leaf is then absent from the managed set →
classified orphaned → reaped by the next apply through the existing
removed-entry reap path, with its prior bytes backed up first. Reaping
removes leaf links only and never removes an intermediate directory,
even one left empty (DEC-005) — confirm the removed-entry reap path does
not delete directories, and add a guard/test if it would.

This task closes REQ-007 and the REQ-003 `when`-flip reaping leg
(CHK-019). Add CLI integration tests: a deleted `symlink-tree` source
leaf is reported orphaned by status (CHK-014) and reaped on the next
apply while its sibling leaf and parent directory survive (CHK-015); a
`[[file]]` entry whose `when` flips from true to false has its prior
target reported orphaned and reaped with a backup (CHK-019).

<task-scenarios>
Given an applied `symlink-tree` whose source contained `sub/b.conf`, and
that source leaf is then deleted,
when `PATINA_REPO=T patina status` runs,
then the output classifies `~/d/sub/b.conf` as orphaned (CHK-014).

Given that same state,
when `PATINA_REPO=T patina apply --yes` runs,
then `~/d/sub/b.conf` no longer exists, `~/d/sub` still exists as a
directory, and the surviving leaf `~/d/a.conf` is still a symbolic link
(CHK-015).

Given a `[[file]]` entry with a true `when` whose target was
materialized, then its `when` edited to a predicate false on this host,
when `PATINA_REPO=T patina status` runs then `PATINA_REPO=T patina apply
--yes` runs,
then status classifies the target orphaned and apply removes it after
recording its prior bytes in a backup (CHK-019).

Suggested files: `patina-core/src/status/mod.rs`,
`patina-core/src/status/classify.rs`,
`patina-cli/tests/symlink_tree_orphans.rs`
</task-scenarios>
</task>

<task id="T-009" state="pending" covers="REQ-004">
## Route `[[auto_match]]` through the shared engine and delete the narrow predicate evaluator

Remove the narrow single-equality predicate evaluator from
`patina-core/src/profile.rs` (`evaluate_predicate`,
`parse_string_literal`, `profile.rs:307-351`) and the
`ProfileError::UnsupportedPredicate` variant (`profile.rs:134-143`),
and evaluate `[[auto_match]]` `when` predicates through the shared
`Engine::eval_when` (`template/mod.rs:162`) instead, under a
builtins-only resolver (DEC-006). `profile::resolve` (`profile.rs:195`)
must accept an `Engine` and evaluate each rule's `when` against a
`Resolver` built from built-ins only — no active-profile, no user
layers — because profile resolution runs before those layers are
assembled (SPEC.md:148-158, DEC-006). Construct that builtins-only
resolver from the `Builtins` already passed to `resolve` (the caller at
`engine.rs:238` passes `&builtins`); thread an `Engine` from the call
site. The engine already errors on undefined-variable access at every
position including inside a comparison (`template/mod.rs:183-202` plus
the existing `eval_when_undefined_variable_names_it` test), so DEC-010's
"undefined access errors at every site" needs no new evaluator code —
it falls out of routing auto_match through the same engine. Parity
holds for predicates over defined built-ins; the wider grammar (`!=`,
`and`, `or`) that the narrow evaluator rejected now evaluates; an
`[[auto_match]]` `when` referencing `patina.profile` (unresolved during
profile resolution) now errors rather than silently failing to match
(CHK-021, DEC-010).

DEC-010 also makes a hook `when` over an undefined variable error
uniformly. Grep the existing hook tests and fixtures for any reliance on
the prior silent-false-on-undefined behaviour
(`rg "when" patina-core/src/apply/hooks.rs patina-cli/tests`); the
engine already errors here and `should_run_surfaces_undefined_variable_error`
(`apply/hooks.rs:465`) already asserts it, so confirm no hook test or
fixture depends on an undefined-in-`when` silently evaluating false, and
fix any that does. Replace `profile.rs`'s narrow-grammar unit tests
(`predicate_rejects_unsupported_shape`, `predicate_rejects_non_patina_lhs`,
`predicate_rejects_unquoted_rhs`, `missing_builtin_compares_as_empty_string`,
`profile.rs:502-535`) with tests asserting the engine path: a
previously-rejected `!=` / `or` shape now selects a profile, and a
`patina.profile` reference errors. Add CLI integration tests CHK-008,
CHK-009, CHK-020, CHK-021. Update the `profile.rs` module docs that
describe the narrow evaluator (`profile.rs:23-44`).

<task-scenarios>
Given a root `patina.toml` with an `[[auto_match]]` rule
`when = "patina.os == '<current OS family>'"` and `profile = "p"`,
when `PATINA_REPO=T patina apply --yes` runs,
then the resolved profile is `p` (CHK-008).

Given a `[[file]]` entry carrying
`when = "patina.os != 'definitely-not-this-os'"`,
when `PATINA_REPO=T patina apply --yes` runs,
then the entry's target is materialized (the inequality evaluates true,
no `UnsupportedPredicate` error) (CHK-009).

Given a `[[file]]` entry carrying `when = "patina.oss == 'windows'"` (a
misspelling of `patina.os`),
when `PATINA_REPO=T patina apply --yes` runs,
then the process exits non-zero, stderr names `patina.oss` as an
undefined variable, and the target is not silently dropped (CHK-020).

Given a root `[[auto_match]]` rule `when = "patina.profile == 'work'"`,
when `PATINA_REPO=T patina apply --yes` runs,
then profile resolution fails with a typed undefined-variable error
naming `patina.profile` (CHK-021).

Suggested files: `patina-core/src/profile.rs`,
`patina-core/src/apply/engine.rs`,
`patina-cli/tests/auto_match_predicates.rs`
</task-scenarios>
</task>

<task id="T-010" state="pending" covers="REQ-008">
## `patina add` writes the table matching the source kind, with kind-checked mode flags

Update `patina add` to detect whether the registered path is a file or a
directory and write the matching table-array: `[[file]]` for a file
source, `[[directory]]` for a directory source, mode defaulting to
`symlink` (REQ-008). Add a `--symlink-tree` flag to `AddArgs`
(`patina-cli/src/cli.rs:215`) in the existing `mode` group, and make
`--copy` valid for a directory source (recursive copy → `mode = "copy"`).
The mode flags are kind-checked: `--symlink-tree` on a file source and
`--template` on a directory source are rejected with a typed error
naming the incompatible flag and the source kind. In `cmd/add.rs`,
detect the source kind from the (tilde-expanded) target's filesystem
metadata before staging, replace the `AddMode`→`FileMode` mapping
(`add.rs:317-323`, `file_mode`) and the three-mode prompt
(`add.rs:255-273`) with kind-aware resolution, and route to a new
`[[directory]]` writer (T-011). A directory source must never emit a
`[[file]]` entry and vice versa.

This task depends on the T-011 writer for the `[[directory]]` emission
and on T-001's mode model. Update the existing `add.rs` unit tests
(`file_mode_maps_each_variant`, the success-envelope test, the mode
resolution tests, `add.rs:411-504`) and add coverage for the flag matrix
and the directory path. Add CLI integration tests CHK-016 and CHK-017.

<task-scenarios>
Given a tempdir repo and a regular file `F`,
when `patina add F --module m` runs against the repo,
then `T/m/patina.toml` contains a `[[file]]` entry and no `[[directory]]`
entry (CHK-016).

Given a tempdir repo and a directory `D`,
when `patina add D --module m --symlink-tree` runs,
then `T/m/patina.toml` contains a `[[directory]]` entry with
`mode = "symlink-tree"` (CHK-017).

Given a regular file source,
when `patina add F --module m --symlink-tree` runs,
then the command is rejected with a typed error naming `--symlink-tree`
and the file source kind.

Given a directory source,
when `patina add D --module m --template` runs,
then the command is rejected with a typed error naming `--template` and
the directory source kind.

Suggested files: `patina-cli/src/cli.rs`,
`patina-cli/src/cmd/add.rs`,
`patina-cli/tests/add_directory.rs`
</task-scenarios>
</task>

<task id="T-011" state="pending" covers="REQ-008 REQ-001">
## Add a `[[directory]]` manifest writer and update the file writer to the collapsed mode names

Extend `patina-core/src/config/writer.rs` to emit `[[directory]]`
entries and to write the collapsed mode names from T-001. Add an
`append_directory_entry` (mirroring `append_file_entry`,
`writer.rs:125`) that pushes a `[[directory]]` array-of-tables element
carrying `source`, `target`, and `mode` for the directory modes
(`symlink` / `symlink-tree` / `copy`). Update `mode_manifest_str`
(`writer.rs:217`) to the collapsed taxonomy: the removed `symlink-dir`
and `copy-tree` spellings must no longer be emitted (DEC-002); a
`[[file]]` writes `symlink` / `copy` / (no mode for template), a
`[[directory]]` writes `symlink` / `symlink-tree` / `copy`. Keep the
format/comment-preserving `toml_edit` approach (DEC-007) and the
exactly-one-of-`target` shape. Re-export the new writer fn from
`config/mod.rs` (`config/mod.rs:31-34`) so `cmd/add.rs` (T-010) can call
it.

Update the writer's existing tests (`append_each_mode_uses_the_parser_accepted_spelling`,
`writer.rs:308-321`, which currently asserts `symlink-dir` / `copy-tree`)
to the new vocabulary, and add round-trip tests proving an appended
`[[directory]]` entry re-parses through `parse_module_config_str` (T-001)
with the expected kind and mode, including a `symlink-tree` directory
and a recursive-`copy` directory.

<task-scenarios>
Given an empty manifest,
when `append_directory_entry` writes a `symlink-tree` entry and the
result is re-parsed,
then the parsed entry has directory kind and `symlink-tree` mode.

Given an empty manifest,
when `append_directory_entry` writes a recursive `copy` entry and the
result is re-parsed,
then the parsed entry has directory kind and `copy` mode.

Given a `[[file]]` entry appended via the writer,
when the result is inspected,
then it never contains the strings `symlink-dir` or `copy-tree`.

Given a `TemplateRender` file entry appended via the writer,
when the result is inspected,
then it emits no `mode` key.

Suggested files: `patina-core/src/config/writer.rs`,
`patina-core/src/config/mod.rs`
</task-scenarios>
</task>

<task id="T-012" state="pending" covers="REQ-001 REQ-002 REQ-003 REQ-004 REQ-005 REQ-006 REQ-007 REQ-008 REQ-009">
## Update docs and cross-SPEC superseded-by notes for the schema/predicate/variable changes

Refresh the user- and agent-facing docs so they no longer describe the
old single `[[file]]` table, the `symlink-dir` / `copy-tree` mode names,
or the narrow `[[auto_match]]` evaluator, and so they document the new
`[[file]]` / `[[directory]]` split, the collapsed mode names plus
`symlink-tree`, per-entry `when`, the unified MiniJinja predicate engine
(including undefined-variable-access errors at every `when` site), and
the repo-shared + per-profile `[variables]` / `[profiles.<name>.variables]`
layering. Update the relevant files under `docs/` and any
`README.md`/`patina.toml` schema reference that enumerates modes or the
entry schema (grep for `symlink-dir`, `copy-tree`, and `[[file]]` across
`docs/` and `README.md` to find every site). This change is required by
the "never let docs drift" hard rule because the observable schema and
predicate behaviour changed.

Per SPEC.md's "Cross-SPEC handoffs" (SPEC.md:758-786), add a
superseded-by note to the affected requirement bodies in the earlier
SPECs — SPEC-0001 REQ-005 (the single `[[file]]` table and its mode
allowlist) and REQ-008/REQ-009 (the narrow auto_match evaluator),
SPEC-0002 REQ-002 (`add` writing `[[file]]`), and SPEC-0001's
missing-source-at-materialize-time behaviour now promoted to plan time —
each pointing to SPEC-0004. These SPECs are locked; edit only their
requirement bodies to add the pointer note (a cosmetic cross-reference,
permitted under the locked-SPEC exception for edits that do not change a
requirement's meaning), not their `<requirement>` semantics.

Run the full `just lint` so the docs gate (`cargo doc -D warnings`)
catches any broken intra-doc links introduced by renamed types.

<task-scenarios>
Given the docs at HEAD after this task,
when `docs/` and `README.md` are searched for the strings `symlink-dir`
and `copy-tree`,
then no occurrence remains except in changelog/historical or
superseded-by context.

Given the schema reference doc,
when it is read for the entry schema,
then it documents both `[[file]]` and `[[directory]]` table-arrays, the
collapsed modes plus `symlink-tree`, the per-entry `when`, and the
repo-shared + per-profile variable layers.

Given the affected SPEC-0001 / SPEC-0002 requirement bodies,
when they are read,
then each carries a superseded-by note pointing to SPEC-0004.

Given the workspace,
when `just lint` runs,
then the `cargo doc -D warnings` gate passes with no broken intra-doc
links.

Suggested files: `docs/`, `README.md`,
`.speccy/specs/0001-*/SPEC.md`, `.speccy/specs/0002-*/SPEC.md`
</task-scenarios>
</task>
