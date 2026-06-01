---
id: SPEC-0004
slug: patina-conditional-entries
title: Conditional entries, the file/directory schema split, recurse symlinks, and complete variable layering
status: in-progress
created: 2026-06-01
supersedes: []
---

# SPEC-0004: Conditional entries, the file/directory schema split, recurse symlinks, and complete variable layering

## Summary

Patina's headline promise is "same source produces the same result
everywhere" across macOS, Linux, and Windows. Today the engine cannot
keep that promise for any repository whose contents differ per OS.
`apply::plan` (`patina-core/src/apply/engine.rs`) enumerates every
module and emits **every** `[[file]]` entry unconditionally; the
resolved profile only sets the `patina.profile` variable value and
gates nothing. There is no per-entry conditional: `RawFileEntry`
carries only `source` / `target` / `targets` / `mode`. So an OS-only
declaration (a Windows window-manager config, a macOS keybinding file)
materializes on every OS, a per-OS *target* (an editor cache directory
that lives at a different path per platform) cannot be expressed, and a
"same target, different source per OS" file cannot be expressed at all.
This gap surfaced concretely while migrating a real cross-platform
dotfiles repository onto Patina.

This SPEC closes that gap and three adjacent ones discovered alongside
it, as one coherent completion of Patina's configuration model:

1. **A conditional `when` on managed entries.** An entry may declare a
   `when` predicate; it contributes to the plan only when the predicate
   is true on the current machine. `[[hook]]` and `[[auto_match]]`
   already carry `when` (evaluated by the shared MiniJinja `Engine` —
   see `apply::hooks::should_run`). This SPEC extends the same predicate
   to managed entries and **unifies** all four predicate sites on that
   one engine, retiring the narrow single-equality evaluator that still
   lives in `profile.rs` for `[[auto_match]]`.

2. **A `[[file]]` / `[[directory]]` schema split.** The single
   `[[file]]` table-array conflates file operations and directory
   operations: `mode` ranges over `symlink` / `symlink-dir` / `copy` /
   `copy-tree` / template, half of which only make sense for a file and
   half only for a directory. This SPEC splits it into two kind-typed
   table-arrays so illegal mode/source-kind combinations become
   unrepresentable and a source-kind mismatch is a typed plan-time
   error. Mode names collapse: with the table supplying the file/dir
   context, `symlink` and `copy` mean "symlink/copy this thing" in both
   tables, and the redundant `symlink-dir` / `copy-tree` names are
   removed.

3. **Recurse symlinks (`symlink-tree`).** A new directory mode that
   creates one symbolic link per leaf file of a source directory,
   leaving the intermediate target directories real — so a directory
   that is *shared* with a tool that writes its own runtime files (an
   editor cache, a media player's watch-history) can have its
   repo-tracked leaves linked without the whole directory becoming a
   symlink into the repo.

4. **Complete variable layering.** The resolver
   (`patina-core/src/variables/mod.rs`) defines repo-shared and
   per-profile layers, but `plan()` never populates them. This SPEC
   wires the root `patina.toml` `[variables]` table as the repo-shared
   layer and `[profiles.<name>.variables]` (active profile) as the
   per-profile layer, so a variable can be declared once at the repo
   root and overridden per profile.

The Windows symlink elevation flow (SPEC-0002, `patina-elevate.exe`)
already removes the need for repository-side Developer Mode bootstrap
scripts; this SPEC is the configuration-expressiveness half of making a
real multi-OS dotfiles repository a clean single-`patina apply`.

## Goals

<goals>
- A `[[file]]` or `[[directory]]` entry with `when = "patina.os == 'windows'"` materializes on Windows and is wholly absent from the plan (no operation, no diff line) on macOS and Linux.
- A module may declare, in one `patina.toml`, both a Windows-only entry and a macOS-only entry for the same logical config without either leaking onto the other OS.
- `[[directory]]` with `mode = "symlink-tree"` links each repo-tracked leaf file into a real target directory, leaving runtime files written by other tools in place and untracked.
- A `[[file]]` whose on-disk source is a directory (or a `[[directory]]` whose source is a file) fails `patina apply` at plan time with a typed error, before any mutation.
- A variable declared in the root `patina.toml` `[variables]` table is resolvable from every module's templates; a `[profiles.<name>.variables]` entry shadows it when that profile is active.
- All four `when` sites — `[[file]]`, `[[directory]]`, `[[hook]]`, `[[auto_match]]` — are evaluated by one MiniJinja engine, and existing `patina.<key> == '<literal>'` predicates over defined variables keep their current results.
- A `when` predicate that references a misspelled or otherwise undefined variable fails the apply with a typed error naming that variable, on every machine, rather than silently dropping the entry.
- Two consecutive `patina apply --yes` runs against unchanged source on one machine produce byte-identical stdout, including when `when`-gated entries are present (REQ-021 parity preserved).
</goals>

## Non-goals

<non-goals>
- No module-level or inherited default `when`. A `when` lives on a single entry and gates only that entry; there is no composition or override across a module. (See DEC-003.)
- No run-once / lifecycle hooks, no `on_change` / `on_drift` events. Provisioning that should run once (git-hook wiring, package-manager config) stays in repository setup scripts, not Patina.
- No elevated-hook path. Patina continues to never request elevated privilege on the user's behalf beyond the existing SPEC-0002 one-time Developer Mode UAC flow.
- No machine-provisioning or prerequisite-tool checks. `patina doctor` continues to inspect only Patina's own environment.
- No per-leaf `exclude` / ignore filter on `symlink-tree`, and no per-leaf `copy` mode. Both are plausible follow-ups, not part of this slice.
- No per-machine variable layer wiring. The resolver defines it; populating it is out of scope here (see Notes).
- No renaming of the `source` / `target` / `targets` / `mode` / `when` fields, and no change to the `.tmpl` template-render trigger.
</non-goals>

## User stories

<user-stories>
- As a multi-machine syncer, I want one dotfiles repository whose `patina apply` lands the right files on macOS, Linux, and Windows without manual per-OS bootstrap, so a fresh laptop is one command.
- As a maintainer, I want a window-manager config that only exists on Windows to simply not appear on my Mac, rather than materializing a stray file in `$HOME`.
- As a maintainer of a shared config directory (an editor cache, a media player), I want Patina to link my tracked files into it while leaving the tool's own runtime files alone.
- As an author writing a `patina.toml`, I want a directory-vs-file mistake to fail loudly at plan time with a message that names the right table, rather than producing a confusing IO error or silently doing the wrong thing.
- As an author, I want to declare a value like a signing key once at the repo root and override it per profile, rather than repeating it in every module.
</user-stories>

## Assumptions

<assumptions>
- The shared MiniJinja `Engine` already exposes a reusable boolean
  predicate entry point (`Engine::eval_when(expr, resolver) ->
  Result<bool>`) that `apply::hooks::should_run` calls for `[[hook]]`
  `when`. `[[file]]` / `[[directory]]` / `[[auto_match]]` reuse that
  exact path; this SPEC introduces no second predicate evaluator.
  Strengthening `eval_when` to error on *any* undefined-variable access
  (DEC-010) applies uniformly to all four sites, including `[[hook]]`:
  hook predicates over defined variables keep their current results, but
  a hook `when` that accesses an undefined variable now errors instead of
  silently evaluating false.
- Source-kind validation (file vs directory) reads the filesystem at
  plan time on the already-resolved source path. `plan()` already
  canonicalizes every source via `paths::canonicalize` during planning;
  the kind check is an `is_file()` / `is_dir()` and adds no extra IO
  pass. However, `paths::canonicalize` falls back to a *lexical*
  resolution for a non-existent path, so a missing source does **not**
  fail at canonicalization today — it currently surfaces later, in the
  executor at materialize time. This SPEC therefore adds an explicit
  plan-time existence-and-kind check (REQ-002, ordered per REQ-009): a
  `when`-true entry whose source is absent now fails during planning,
  before the lock / journal / any mutation. The executor's
  materialize-time existence check is retained as a TOCTOU backstop
  (DEC-008).
- `symlink-tree` orphan tracking and reaping reuse the existing journal
  + backup transactional machinery (the `ApplyRecord` / `ExpectedTarget`
  commit record, `backup_before_overwrite`, and the rollback replay
  path). No new on-disk persistence format is introduced; the recorded
  leaf set is expressed in terms of the existing per-target records.
- The active profile name is resolved before module planning (it
  already is, via `profile::resolve` in `plan()`), so the
  `[profiles.<name>.variables]` table for that name can be selected and
  pushed as the per-profile layer during the same `plan()` pass. Parsing
  `[profiles.<name>.variables]` from the root manifest is net-new work:
  the root manifest parser today deserializes only `[[auto_match]]`
  rules and no `[profiles.<name>]` section, so REQ-005 adds that
  deserialization rather than merely wiring an already-parsed table.
- Removing the narrow single-equality evaluator from `profile.rs` does
  not change any currently-passing `[[auto_match]]` result *over defined
  built-ins*, because the MiniJinja engine evaluates
  `patina.<key> == '<literal>'` to the same boolean. The migration
  additionally accepts the wider grammar (`!=`, `and`, `or`) that the
  narrow evaluator rejected with a hard error. One behavior changes
  deliberately (DEC-010): a predicate that *accesses an undefined
  variable* now errors instead of the narrow evaluator's silent-false —
  including an `[[auto_match]]` `when` that references `patina.profile`,
  which is unresolved during profile resolution and so is genuinely
  unavailable in that context (DEC-006).
- A directory source for `symlink-tree` is walked deterministically
  (sorted) so the recorded leaf set and the emitted plan are stable
  across runs, preserving the byte-identical-stdout guarantee.
</assumptions>

## Requirements

<requirement id="REQ-001">
### REQ-001: Managed entries are declared under kind-typed `[[file]]` and `[[directory]]` table-arrays

A module `patina.toml` declares managed entries under two table-arrays.
`[[file]]` entries describe a file source and accept `mode = "symlink"`
(the default when omitted) or `mode = "copy"`; a source whose filename
ends in `.tmpl` is rendered as a template and must not declare a `mode`.
`[[directory]]` entries describe a directory source and accept
`mode = "symlink"` (the default, an atomic whole-directory symlink),
`mode = "symlink-tree"` (one symbolic link per leaf file), or
`mode = "copy"` (a recursive directory copy). The mode strings
`symlink-dir` and `copy-tree` no longer exist; their behaviors are the
`[[directory]]` `symlink` default and `[[directory]]` `copy`
respectively. A mode outside a table's accepted set is a typed
parse-time error naming that table's accepted modes.

<done-when>
- A `[[file]]` entry with `mode` omitted resolves to a single-file symlink; with `mode = "copy"` resolves to a single-file byte copy.
- A `[[file]]` entry whose `source` ends in `.tmpl` resolves to a template render and is rejected if it also declares any `mode`.
- A `[[directory]]` entry with `mode` omitted resolves to an atomic whole-directory symlink (the prior `symlink-dir` behavior).
- A `[[directory]]` entry with `mode = "symlink-tree"` resolves to per-leaf symlinks; with `mode = "copy"` resolves to a recursive directory copy (the prior `copy-tree` behavior).
- A `[[file]]` declaring `mode = "symlink-tree"`, `"symlink-dir"`, or `"copy-tree"` is rejected with a typed error whose message names the accepted `[[file]]` modes (`symlink`, `copy`).
- A `[[directory]]` declaring `mode = "symlink-dir"` or `"copy-tree"` is rejected with a typed error whose message names the accepted `[[directory]]` modes (`symlink`, `symlink-tree`, `copy`).
- A `[[directory]]` entry whose `source` ends in `.tmpl` is rejected (template render is file-only).
- The exactly-one-of `target` / `targets` rule and the non-empty-`targets` rule apply identically to both tables.
</done-when>

<behavior>
- Given a module manifest with a `[[file]]` entry (`mode` omitted) and a `[[directory]]` entry with `mode = "symlink-tree"`, when the manifest is parsed, then the file entry resolves to a single-file symlink and the directory entry resolves to per-leaf symlinks.
- Given a `[[file]]` entry declaring `mode = "copy-tree"`, when the manifest is parsed, then parsing fails with a typed error naming the accepted `[[file]]` modes.
</behavior>

<scenario id="CHK-001">
Given a module `patina.toml` containing a `[[file]]` entry with
`source = "zshrc"`, `target = "~/.zshrc"` and no `mode`, when the
manifest is parsed, then the resolved entry has file kind and symlink
mode.
</scenario>

<scenario id="CHK-002">
Given a module `patina.toml` containing a `[[directory]]` entry with
`source = "mpv"`, `target = "~/.config/mpv"`, `mode = "symlink-tree"`,
when the manifest is parsed, then the resolved entry has directory kind
and per-leaf symlink mode.
</scenario>

<scenario id="CHK-003">
Given a module `patina.toml` containing a `[[file]]` entry that declares
`mode = "symlink-tree"`, when the manifest is parsed, then parsing fails
with a typed error and the message contains the substring `symlink-tree`
and the accepted `[[file]]` modes `symlink`, `copy`.
</scenario>
</requirement>

<requirement id="REQ-002">
### REQ-002: Source-kind mismatch is a typed plan-time error before any mutation

During `patina apply` planning, the kind declared by an entry's table is
validated against the kind of its source on disk. A `[[file]]` entry
whose source resolves to a directory, or a `[[directory]]` entry whose
source resolves to a file, fails with a typed error that names the
offending source path and the table it should use instead. The failure
occurs in the plan phase — before the advisory lock is taken, before the
journal is flushed, and before any backup or materialization — so a
mismatched entry mutates nothing. A source that does not exist on disk
fails the same way: a typed "source not found" error raised in the plan
phase before any mutation (DEC-008), rather than the prior
materialize-time failure. This plan-time check runs only on entries that
survive `when`-gating (REQ-009), so an entry gated off on the current OS
is never validated. The executor retains its own existence check as a
materialize-time TOCTOU backstop.

<done-when>
- A `[[file]]` entry whose source is a directory fails `patina apply` with a typed error naming the source path and directing the author to `[[directory]]`.
- A `[[directory]]` entry whose source is a file fails symmetrically, directing the author to `[[file]]`.
- A `when`-true entry whose source does not exist on disk fails `patina apply` with a typed "source not found" error raised during planning, before the lock / journal / any mutation.
- The mismatch error is raised during planning; no journal plan file, no backup, and no COMMIT record is written for the run.
- A non-interactive `patina apply` (no TTY) surfaces the same error and writes nothing.
- The executor's materialize-time source-existence check is retained as a TOCTOU backstop: a source deleted between plan and materialize still fails with a typed error, not a panic.
</done-when>

<behavior>
- Given a repository whose `[[file]]` entry points at a directory source, when `patina apply --yes` runs, then the process exits non-zero, stderr names the source path, and the state directory contains no new journal or backup artifacts for the run.
- Given a repository whose `[[directory]]` entry points at a file source, when `patina apply --yes` runs, then the process exits non-zero and stderr directs the author to `[[file]]`.
</behavior>

<scenario id="CHK-004">
Given a tempdir repository `T` with a module whose `[[file]]` entry has
`source = "confdir"` where `T/<module>/confdir` is a directory, when
`PATINA_REPO=T patina apply --yes` runs, then the process exits 1,
stderr contains the substring `confdir` and the substring `[[directory]]`,
and `T`'s state directory has no `*.plan` or `*.COMMIT` journal file for
the run.
</scenario>

<scenario id="CHK-005">
Given a tempdir repository `T` with a module whose `[[directory]]` entry
has `source = "gitconfig"` where `T/<module>/gitconfig` is a regular
file, when `PATINA_REPO=T patina apply --yes` runs, then the process
exits 1 and stderr contains the substring `[[file]]`.
</scenario>

<scenario id="CHK-018">
Given a tempdir repository `T` with a module whose `[[file]]` entry has
`source = "ghost"` and no `when` (so it is not gated off), where
`T/<module>/ghost` does not exist on disk, when `PATINA_REPO=T patina
apply --yes` runs, then the process exits 1, stderr names `ghost` as a
missing source, and `T`'s state directory has no `*.plan` or `*.COMMIT`
journal file for the run.
</scenario>
</requirement>

<requirement id="REQ-003">
### REQ-003: A `when` predicate on an entry gates its presence in the plan

A `[[file]]` or `[[directory]]` entry may declare a `when` predicate.
When the predicate evaluates true on the current machine, the entry
contributes its operations to the plan as it would without a `when`.
When the predicate evaluates false, the entry contributes **nothing** —
no operation, and no diff line — so it is indistinguishable in the plan
from an entry that was never declared. An entry with no `when` always
plans. For a multi-target entry (`targets = [...]`), the `when` gates
all targets together: either every target is planned or none is.

<done-when>
- An entry whose `when` is true on the current machine plans exactly the operations it would with no `when`.
- An entry whose `when` is false contributes zero planned operations and zero diff lines for that run.
- An entry with no `when` always plans, unchanged from current behavior.
- A multi-target entry with a false `when` plans none of its targets; with a true `when`, all of them.
- An entry whose `when` flips from true to false on a machine where its target was previously materialized is treated as a removed entry: the prior target is classified orphaned by `patina status` and reaped on the next `patina apply` (via the existing removed-entry path), with the pre-existing bytes backed up first.
- Two consecutive `patina apply --yes` runs against unchanged source on the same machine produce byte-identical stdout when the repository contains `when`-gated entries (REQ-021 parity).
</done-when>

<behavior>
- Given an entry with `when = "patina.os == 'windows'"` on a host where `patina.os` is `linux`, when planning runs, then the plan contains no operation for that entry and the rendered diff names it nowhere.
- Given the same entry on a host where `patina.os` is `windows`, when planning runs, then the entry's operations are present in the plan.
</behavior>

<scenario id="CHK-006">
Given a tempdir repository `T` with a module entry carrying
`when = "patina.os == 'definitely-not-this-os'"`, when
`PATINA_REPO=T patina apply --yes` runs, then the entry's target is not
created and the run's plan records zero operations for it.
</scenario>

<scenario id="CHK-007">
Given a tempdir repository `T` with a module entry whose `when` equals
`patina.os == '<the current OS family>'`, when
`PATINA_REPO=T patina apply --yes` runs twice, then the entry's target
is materialized and the second run's stdout is byte-identical to the
first.
</scenario>

<scenario id="CHK-019">
Given a tempdir repository `T` whose `[[file]]` entry has a true `when`
and has been applied so its target exists, when the entry's `when` is
then edited to a predicate false on this host, then `PATINA_REPO=T
patina status` classifies the target orphaned, and a subsequent
`PATINA_REPO=T patina apply --yes` removes the target after recording its
prior bytes in a backup.
</scenario>
</requirement>

<requirement id="REQ-004">
### REQ-004: One MiniJinja engine evaluates every `when` site; the narrow `[[auto_match]]` evaluator is removed

`when` predicates on `[[file]]`, `[[directory]]`, `[[hook]]`, and
`[[auto_match]]` are all evaluated by the shared MiniJinja `Engine`
under strict-undefined semantics, producing a boolean. The narrow
single-equality predicate evaluator currently in `profile.rs` (which
accepts only `patina.<key> == '<literal>'` and returns a hard
`UnsupportedPredicate` error for anything else) is removed; profile
auto-matching evaluates through the shared engine instead. Every
predicate the narrow evaluator accepted *over defined built-ins* yields
the same boolean under the engine (parity), and the wider grammar
(`!=`, `and`, `or`, expressions) that the narrow evaluator rejected now
evaluates. A `when` predicate that *accesses* any variable undefined at
every resolution layer is a typed error at every site (DEC-010),
replacing both the narrow evaluator's silent-false on an unknown
built-in and the engine's prior silent-false when an undefined variable
appeared inside a comparison; a variable on a short-circuited
(not-taken) `and` / `or` branch is not accessed and does not error.
`[[auto_match]]` predicates resolve against the built-in variable
context only — no active-profile and no user variable layers —
preserving today's profile-resolution order without circularity;
consequently an `[[auto_match]]` `when` that references `patina.profile`
(unresolved during profile resolution) accesses an undefined variable
and errors.

<done-when>
- A `[[file]]` / `[[directory]]` `when` is evaluated through the same engine path `[[hook]]` `when` uses (`Engine::eval_when`).
- A `when` that accesses an undefined variable fails the plan with a typed error naming the variable, whether the reference is bare (`missing_var`) or inside a comparison (`patina.oss == 'windows'`, a typo of `patina.os`) — never a silent false.
- A `when` whose undefined variable sits on a short-circuited (not-taken) `and` / `or` branch does not error, because the variable is never accessed.
- The undefined-access error applies uniformly to `[[file]]`, `[[directory]]`, `[[hook]]`, and `[[auto_match]]`; predicates over defined variables keep their current results.
- An `[[auto_match]]` rule with `when = "patina.os == 'linux'"` selects its profile on a Linux host exactly as before this SPEC.
- An `[[auto_match]]` rule using a previously-rejected shape (`patina.os != 'windows'`, or an `or` of two equalities) evaluates instead of erroring.
- `[[auto_match]]` predicate evaluation reads only built-ins (no profile or module layer); an `[[auto_match]]` `when` referencing a user-defined variable therefore accesses an undefined variable and errors (DEC-010), rather than resolving it from a layer that is not yet assembled.
- The `profile::ProfileError::UnsupportedPredicate` variant and the narrow evaluator function are gone from the codebase.
</done-when>

<behavior>
- Given an `[[auto_match]]` rule `when = "patina.hostname == 'tower'"` on a host named `tower`, when profile resolution runs, then the rule's profile is selected, identical to pre-SPEC behavior.
- Given a `[[file]]` entry `when = "patina.os == 'linux' or patina.os == 'macos'"` on a Linux host, when planning runs, then the entry is present in the plan.
- Given a `[[file]]` entry `when = "patina.oss == 'windows'"` (a typo of `patina.os`), when planning runs on any host, then the apply fails with a typed error naming `patina.oss`, rather than the entry silently vanishing from the plan.
</behavior>

<scenario id="CHK-008">
Given a tempdir repository `T` whose root `patina.toml` has an
`[[auto_match]]` rule `when = "patina.os == '<current OS family>'"` with
`profile = "p"`, when `PATINA_REPO=T patina apply --yes` runs, then the
resolved profile is `p`.
</scenario>

<scenario id="CHK-009">
Given a tempdir repository `T` with a `[[file]]` entry carrying
`when = "patina.os != 'definitely-not-this-os'"`, when
`PATINA_REPO=T patina apply --yes` runs, then the entry's target is
materialized (the inequality evaluates true and no `UnsupportedPredicate`
error is raised).
</scenario>

<scenario id="CHK-020">
Given a tempdir repository `T` with a `[[file]]` entry carrying
`when = "patina.oss == 'windows'"` (a misspelling of `patina.os`), when
`PATINA_REPO=T patina apply --yes` runs, then the process exits non-zero,
stderr names `patina.oss` as an undefined variable, and the entry's
target is not silently dropped.
</scenario>

<scenario id="CHK-021">
Given a tempdir repository `T` whose root `patina.toml` has an
`[[auto_match]]` rule `when = "patina.profile == 'work'"`, when
`PATINA_REPO=T patina apply --yes` runs, then profile resolution fails
with a typed undefined-variable error naming `patina.profile` (it is
unresolved during profile resolution), rather than the rule silently
failing to match.
</scenario>
</requirement>

<requirement id="REQ-005">
### REQ-005: Apply planning populates the repo-shared and per-profile variable layers

`plan()` loads the root `patina.toml` `[variables]` table as the
repo-shared variable layer and the active profile's
`[profiles.<name>.variables]` table as the per-profile variable layer,
both pushed into the resolver during planning. Resolution precedence
follows the existing resolver order: CLI overrides, then per-machine,
then per-profile, then per-module, then repo-shared, then built-ins.
Reserved `patina.*` keys in either table are rejected as they are for
every other user layer.

<done-when>
- A variable declared only in the root `patina.toml` `[variables]` table resolves inside any module's `.tmpl` template.
- When a profile is active, a key present in both the root `[variables]` table and that profile's `[profiles.<name>.variables]` table resolves to the profile's value (per-profile shadows repo-shared).
- A key present in a module's `[variables]` table shadows the repo-shared value (per-module beats repo-shared), unchanged from the documented order.
- A `patina.*` key in the root `[variables]` or any `[profiles.*.variables]` table is rejected with the existing reserved-key error.
</done-when>

<behavior>
- Given a root `[variables]` table defining `signingkey` and a module template referencing it, when planning renders the template, then the rendered output carries the root value.
- Given the same key also defined in the active profile's `[profiles.<name>.variables]`, when planning renders the template, then the profile value is used.
</behavior>

<scenario id="CHK-010">
Given a tempdir repository `T` whose root `patina.toml` declares
`[variables]` with `editor = "nvim"` and a module with a `.tmpl` source
referencing `editor`, when `PATINA_REPO=T patina apply --yes` runs, then
the materialized target contains `nvim`.
</scenario>

<scenario id="CHK-011">
Given that same repository plus an active profile `work` whose
`[profiles.work.variables]` sets `editor = "code"`, when
`PATINA_PROFILE=work PATINA_REPO=T patina apply --yes` runs, then the
materialized target contains `code`.
</scenario>
</requirement>

<requirement id="REQ-006">
### REQ-006: `symlink-tree` links each leaf file, leaving intermediate directories real

A `[[directory]]` entry with `mode = "symlink-tree"` walks its source
directory and creates one symbolic link per leaf file at the mirrored
path under the target. Intermediate target directories are created as
real directories on demand to host those links. Empty source
subdirectories are skipped: they produce neither a target directory nor
a link. A pre-existing real file at a leaf-target path is backed up via
the same `backup_before_overwrite` path every other mode uses, then
replaced by the leaf symlink.

<done-when>
- A `symlink-tree` over a source directory with nested files creates one symbolic link per source leaf at the mirrored target path, each pointing at its source file.
- Intermediate target directories created to host leaves are real directories, not symbolic links.
- An empty subdirectory under the source produces no corresponding target directory and no link.
- A leaf-target path that already holds a regular file is backed up before the leaf symlink replaces it.
- A subsequent `patina apply` against unchanged source is a no-op for the `symlink-tree` entry (idempotent).
</done-when>

<behavior>
- Given a source directory `d/` containing `a.conf` and `sub/b.conf`, when a `symlink-tree` entry targets `~/d`, then `~/d/a.conf` and `~/d/sub/b.conf` are symbolic links, `~/d` and `~/d/sub` are real directories.
- Given the source additionally contains an empty `empty/` subdirectory, when the entry materializes, then `~/d/empty` does not exist.
</behavior>

<scenario id="CHK-012">
Given a tempdir repository `T` with a `[[directory]]` `symlink-tree`
entry whose source contains `a.conf` and `sub/b.conf`, when
`PATINA_REPO=T patina apply --yes` runs, then `~/d/a.conf` and
`~/d/sub/b.conf` are symbolic links resolving to the source files, and
`~/d` and `~/d/sub` are real directories.
</scenario>

<scenario id="CHK-013">
Given the same entry where the target leaf `~/d/a.conf` already exists as
a regular file before apply, when `PATINA_REPO=T patina apply --yes`
runs, then the prior file's bytes are recorded in a backup and
`~/d/a.conf` is afterward a symbolic link to the source.
</scenario>
</requirement>

<requirement id="REQ-007">
### REQ-007: `symlink-tree` orphan leaves are reported by status and reaped on the next apply

An apply records the set of leaf-link targets each `symlink-tree` entry
materialized, using the existing commit-record machinery. When a source
leaf is later deleted from the repository, `patina status` classifies
the corresponding recorded target leaf as orphaned. The next
`patina apply` removes those orphaned leaf links. Reaping removes leaf
links only; it never removes an intermediate directory, even one that
becomes empty after its last leaf link is reaped, because Patina cannot
prove it owns a directory that may also hold files written outside
Patina.

<done-when>
- After a `symlink-tree` apply, the commit record contains one recorded target per materialized leaf link.
- After a source leaf is deleted, `patina status` reports the corresponding target leaf link as orphaned.
- A subsequent `patina apply` removes the orphaned leaf link whose source was deleted.
- Reaping a leaf whose parent directory becomes empty leaves that directory in place (no directory removal).
- A leaf still backed by a live source is never reaped.
</done-when>

<behavior>
- Given a materialized `symlink-tree` whose source `sub/b.conf` is then deleted, when `patina status` runs, then the target `~/d/sub/b.conf` is classified orphaned.
- Given that state, when `patina apply` runs, then `~/d/sub/b.conf` is removed and `~/d/sub` remains a directory.
</behavior>

<scenario id="CHK-014">
Given a tempdir repository `T` with an applied `symlink-tree` entry whose
source contained `sub/b.conf`, and that source leaf is then deleted, when
`PATINA_REPO=T patina status` runs, then the output classifies
`~/d/sub/b.conf` as orphaned.
</scenario>

<scenario id="CHK-015">
Given that same state, when `PATINA_REPO=T patina apply --yes` runs, then
`~/d/sub/b.conf` no longer exists, `~/d/sub` still exists as a directory,
and the surviving leaf `~/d/a.conf` is still a symbolic link.
</scenario>
</requirement>

<requirement id="REQ-008">
### REQ-008: `patina add` writes the table matching the source kind

`patina add <path>` detects whether the registered path is a file or a
directory and writes the matching table-array into the module's
`patina.toml`: `[[file]]` for a file source, `[[directory]]` for a
directory source. The emitted entry's mode defaults to `symlink`. A
directory source also accepts `--copy` (a recursive directory copy,
`mode = "copy"`) and `--symlink-tree` (per-leaf links); the default
remains the atomic `symlink`. The mode flags are kind-checked:
`--symlink-tree` on a file source and `--template` on a directory source
are rejected with a typed error naming the incompatible flag and source
kind.

<done-when>
- `patina add <file>` writes a `[[file]]` entry with `mode` defaulting to `symlink` (or `copy` when `--copy` is requested, consistent with the existing add flags).
- `patina add <dir>` writes a `[[directory]]` entry with `mode` defaulting to `symlink`.
- `patina add <dir> --symlink-tree` writes a `[[directory]]` entry with `mode = "symlink-tree"`.
- `patina add <dir> --copy` writes a `[[directory]]` entry with `mode = "copy"` (recursive copy).
- `patina add <file> --symlink-tree` and `patina add <dir> --template` are rejected with a typed error naming the incompatible flag and source kind.
- A `patina add` of a directory never emits a `[[file]]` entry, and a `patina add` of a file never emits a `[[directory]]` entry.
</done-when>

<behavior>
- Given a regular file at `~/.zshrc`, when `patina add ~/.zshrc --module zsh` runs, then `zsh/patina.toml` gains a `[[file]]` entry.
- Given a directory at `~/.config/mpv`, when `patina add ~/.config/mpv --module mpv` runs, then `mpv/patina.toml` gains a `[[directory]]` entry.
</behavior>

<scenario id="CHK-016">
Given a tempdir repository `T` and a regular file `F`, when
`patina add F --module m` runs against `T`, then `T/m/patina.toml`
contains a `[[file]]` table-array entry and no `[[directory]]` entry.
</scenario>

<scenario id="CHK-017">
Given a tempdir repository `T` and a directory `D`, when
`patina add D --module m --symlink-tree` runs against `T`, then
`T/m/patina.toml` contains a `[[directory]]` entry with
`mode = "symlink-tree"`.
</scenario>
</requirement>

<requirement id="REQ-009">
### REQ-009: The plan loop evaluates `when` before canonicalizing or kind-checking a source, and emits both tables in one deterministic order

For each managed entry, the plan phase processes it in a fixed order:
(1) evaluate the entry's `when` predicate, if any — when it is false the
entry is dropped immediately and contributes no operation, diff line,
canonicalization, or source validation; (2) canonicalize the source;
(3) validate the source's existence and kind (REQ-001, REQ-002). A
`when`-false entry is therefore never canonicalized or kind-checked, so
an entry gated off on the current machine cannot raise a missing-source,
kind-mismatch, or canonicalization error from a source that is absent or
differently shaped on this OS — the property that lets one cross-OS
repository apply cleanly everywhere.

Entries are emitted in one deterministic order across both
table-arrays: all `[[file]]` entries in declaration order, then all
`[[directory]]` entries in declaration order. Entry indices are assigned
over this full declared sequence as a single monotonic space (files
first, then directories), independent of any `when`; a `when`-false
entry occupies its index but emits no operations or commit records. This
keeps the rendered plan, `--json` output, and the commit record's
per-entry grouping stable and collision-free across the two tables
(DEC-009, REQ-021).

<done-when>
- A `when`-false `[[file]]` or `[[directory]]` entry is dropped before its source is canonicalized or kind-checked; an absent or wrong-kind source on the current OS for a `when`-false entry raises no plan error.
- A `when`-true entry (or one with no `when`) is canonicalized and existence/kind-checked exactly as today.
- The plan emits all `[[file]]` operations before all `[[directory]]` operations, each block in declaration order.
- Entry indices form a single monotonic space across both tables (all `[[file]]`, then all `[[directory]]`); no `[[file]]` and `[[directory]]` entry share an index.
- Two consecutive `patina apply --yes` runs against unchanged source produce byte-identical stdout with both tables populated and some entries `when`-gated (REQ-021 parity).
</done-when>

<behavior>
- Given a macOS-only `[[directory]]` entry (`when = "patina.os == 'macos'"`) whose `source` does not exist on a Linux host, when planning runs on Linux, then the entry is dropped and no missing-source or kind error is raised.
- Given a manifest with two `[[file]]` entries and one `[[directory]]` entry, when the plan renders, then both file operations appear before the directory operation.
</behavior>

<scenario id="CHK-022">
Given a tempdir repository `T` whose module declares a `[[directory]]`
entry with `when = "patina.os == 'definitely-not-this-os'"` and a
`source` that does not exist on disk, when `PATINA_REPO=T patina apply
--yes` runs, then the process exits 0, no target is created for the
entry, and stderr contains no missing-source or `[[file]]` / `[[directory]]`
kind error.
</scenario>
</requirement>

## Decisions

<decision id="DEC-001">
Split the single `[[file]]` table-array into kind-typed `[[file]]` and
`[[directory]]` tables rather than keep one table with a mixed mode set.
The mode taxonomy already cleaves cleanly by source kind, and the split
makes mode/source-kind illegal states unrepresentable at parse time and
enables a clear source-kind validation error at plan time
(REQ-002). The alternative — keeping one table and merely renaming it
(e.g. `[[entry]]`) — was rejected because it preserves the mixed mode
set, offers no source-kind validation, and keeps the redundant
`-dir` / `-tree` mode-name suffixes.
</decision>

<decision id="DEC-002">
Collapse the mode names so the table supplies the file/dir context:
`symlink` and `copy` mean "symlink/copy this thing" in both tables, and
the redundant `symlink-dir` and `copy-tree` strings are removed. Only
`symlink-tree` keeps a distinct name, because per-leaf linking is a
genuinely different operation from the atomic whole-directory `symlink`
default. Because Patina is pre-release with no external users, removing
the old mode strings outright is preferred over carrying aliases.
</decision>

<decision id="DEC-003">
`when` lives on a single entry only; there is no module-level or
inherited default `when`. A module default would force a composition
rule between the module predicate and an entry predicate, and every
choice (logical AND, or replace) is a footgun: AND silently makes an
entry that disagrees with the module always-false, and replace silently
ignores the module gate. Per-entry-only `when` has zero composition
semantics to define and keeps a single, local source of truth for
whether an entry applies. The cost — repeating a `when` on the handful
of entries in a single-OS module — is small and explicit.
</decision>

<decision id="DEC-004">
A `when`-false entry is absent from the plan entirely — it produces no
planned operation and no diff line — rather than appearing as a skipped
no-op. This keeps the rendered plan and `--json` output free of entries
that did nothing, and preserves the byte-identical-stdout / idempotency
guarantee (REQ-021): the plan for a given machine is exactly the set of
entries whose predicates hold.
</decision>

<decision id="DEC-005">
`symlink-tree` owns leaf links only. Directories are treated as real,
unmanaged filesystem: created on demand to host a leaf (as the
copy/symlink modes already create parent directories), never mirrored
when the source subdirectory is empty (REQ-006), and never reaped when a
target directory becomes empty after its leaves are removed (REQ-007).
The invariant is that Patina never deletes a directory it cannot prove
it owns, because that directory may also hold files written by the tool
the directory belongs to.
</decision>

<decision id="DEC-006">
Unify all `when` evaluation on the existing MiniJinja `Engine` and
remove the narrow single-equality evaluator from `profile.rs`. One
evaluator across `[[file]]` / `[[directory]]` / `[[hook]]` /
`[[auto_match]]` means one grammar, one strict-undefined behavior, and
one place to reason about predicates. `[[auto_match]]` evaluates against
built-ins only, because profile resolution runs before the user
variable layers are assembled and an auto-match predicate that depended
on a profile-scoped variable would be circular.
</decision>

<decision id="DEC-007">
A foreign regular file at a `symlink-tree` leaf target is backed up and
replaced via the existing overwrite path, not treated as a hard error.
Patina already backs up any pre-existing target before overwriting it
(the never-overwrite-without-backup guarantee), surfaces the change in
the diff-and-prompt before applying, and can restore it via rollback.
Making `symlink-tree` leaves a special error case would be inconsistent
with every other mode for no safety gain.
</decision>

<decision id="DEC-008">
A `when`-true entry whose source does not exist is a typed plan-time
error, raised before the advisory lock, journal, and any mutation — not
the prior materialize-time failure. `plan()` already resolves every
surviving source, so the existence check is free there, and failing in
the plan phase means a doomed run never takes the lock or flushes a
journal. This changes the behavior established in SPEC-0001, where a
missing source surfaced only at materialize time because
`paths::canonicalize` falls back to lexical resolution for a
non-existent path and does not itself fail. The executor's
materialize-time existence check is retained as a TOCTOU backstop for a
source deleted between plan and materialize; the two checks bracket a
time window rather than redundantly guarding one instant.
</decision>

<decision id="DEC-009">
Managed entries occupy one monotonic `entry` index space across both
table-arrays — all `[[file]]` entries in declaration order, then all
`[[directory]]` entries — and the plan emits the file block before the
directory block. The commit record's per-entry grouping (the
`entry: u32` field that drives atomic per-entry rollback) requires
globally unique indices; naive per-array indexing would collide
(`[[file]]` #0 vs `[[directory]]` #0). A single files-then-directories
space keeps the `u32` journal field unchanged (no versioned wire-format
change) and makes the rendered plan, `--json`, and rollback grouping
deterministic (REQ-021). Preserving the original interleaved declaration
order across the two tables was rejected: serde-parsed structs lose
cross-table document order, so recovering it would require an
order-preserving parse for no observable benefit.
</decision>

<decision id="DEC-010">
A `when` predicate that accesses any variable undefined at every
resolution layer is a typed error, at every predicate site (`[[file]]`,
`[[directory]]`, `[[hook]]`, `[[auto_match]]`), rather than silently
evaluating false. Under the engine's `SemiStrict` semantics a bare
undefined reference already errors, but an undefined variable inside a
comparison (`patina.oss == 'windows'`) evaluates to a defined `false`,
so a misspelled `patina.*` key would silently drop the entry on every
machine — a "never surprise the user" footgun. The unified evaluator
(DEC-006) therefore reports an undefined-variable access regardless of
position; a variable on a short-circuited (not-taken) `and` / `or`
branch is never accessed and does not error, so OS-guarded predicates
stay valid. This narrows the `[[auto_match]]` parity guarantee relative
to the removed narrow evaluator (which silently treated an unknown
built-in as the empty string): parity holds for predicates over defined
built-ins; a predicate referencing an unavailable variable (e.g.
`patina.profile` during profile resolution) now errors. Hook predicates
over defined variables keep their current results.
</decision>

## Open Questions

_None at authoring time; the framing was atomized and approved via
`/speccy-brainstorm` before this SPEC was written._

## Changelog

<changelog>
| Date | Author | Summary |
| --- | --- | --- |
| 2026-06-01 | kvnxiao | Initial SPEC: conditional `when` on managed entries, `[[file]]`/`[[directory]]` schema split with collapsed mode names, `symlink-tree` recurse mode with orphan reaping, unified MiniJinja predicate evaluator, and repo-shared + per-profile variable-layer wiring. |
| 2026-06-01 | kvnxiao | Pre-decompose review refinements: add REQ-009 (`when`-before-validation ordering + deterministic files-then-directory emission); REQ-002 promotes missing-source to a plan-time error (DEC-008); DEC-009 pins the single monotonic entry-index space; DEC-010 makes undefined-variable access in any `when` a typed error at every site (incl. hooks), narrowing the `[[auto_match]]` parity claim; REQ-008 adds `add <dir> --copy` and flag-matrix rejections; REQ-003 documents `when`-flip reaping; corrected the missing-source and profile-variable-parsing assumptions. |
</changelog>

## Notes

### Cross-SPEC handoffs

This SPEC changes parts of the schema and behavior first established in
earlier specs without superseding them wholesale (hence
`supersedes: []`):

- **SPEC-0001 REQ-005** defined the single `[[file]]` table-array and
  its mode allowlist (`symlink` / `symlink-dir` / `copy` / `copy-tree` /
  implicit template). REQ-001 here replaces that schema with the
  `[[file]]` / `[[directory]]` split and the collapsed mode names. The
  file-mode executors from SPEC-0001 (`apply::materialize`) are reused;
  only the parsing and mode dispatch change, plus the new `symlink-tree`
  executor.
- **SPEC-0001 REQ-008 / REQ-009** and `profile.rs` shipped the narrow
  `[[auto_match]]` predicate evaluator as a placeholder until the
  MiniJinja `when` engine (referred to as "T-008") landed. That engine
  has since landed for `[[hook]]` (`apply::hooks::should_run`); REQ-004
  here completes the migration by routing `[[auto_match]]` through it
  and deleting the placeholder.
- **SPEC-0002 REQ-002** defined `patina add` writing a `[[file]]` entry.
  REQ-008 here updates `add` to emit `[[file]]` or `[[directory]]` by
  source kind.
- **SPEC-0001** surfaced a missing source only at materialize time.
  DEC-008 / REQ-002 here promote it to a plan-time error (before the
  lock / journal), keeping the executor's existence check as a
  materialize-time TOCTOU backstop.

When this SPEC ships, the affected requirement bodies in SPEC-0001 /
SPEC-0002 should carry a superseded-by note pointing here.

### Deferred: per-machine variable layer

The resolver (`variables/mod.rs`) defines six layers; this SPEC wires
the two that planning omitted (repo-shared, per-profile). The
per-machine layer remains unpopulated by `plan()`. Wiring it (a
per-machine variables file under the state directory) is a clean
follow-up and is intentionally out of scope here — the migration that
motivated this SPEC needs repo-shared and per-profile, not per-machine.

### Rejected design alternatives

- **`[[entry]]` rename keeping one mixed table** — see DEC-001.
- **Module-level / inherited `when`** — see DEC-003.
- **Per-OS via profile-scoped module discovery** (a profile lists which
  modules are active) instead of `when` — rejected because whole-module
  granularity cannot express a per-OS *target* on one logical config or
  a same-target/different-source-per-OS file, and it would require new
  gated-discovery machinery; per-entry `when` is finer and reuses the
  landed engine.
- **Narrow `==`-only `when`** carried forward instead of full MiniJinja
  — rejected in favor of unifying on the one engine (DEC-006), which
  also removes the standing `UnsupportedPredicate` placeholder.
- **Splitting this work into two specs** (schema/predicate/variables;
  then recurse) — rejected because both edit the same `engine::plan`
  entry loop, so one spec with one vocabulary avoids forking that hot
  path twice.
