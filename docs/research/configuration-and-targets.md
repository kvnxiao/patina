---
date: 2026-11-14
questions:
  - Which configuration, discovery, profile, and variable rules determine a deployment?
  - Which source and target paths do file and directory modes manage?
  - How do conditions, templates, ignores, collisions, and status interact?
  - Where does the implementation differ from the confirmed v1 contract?
observed-in:
  - Patina commit 4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc
  - MiniJinja 2.23.0 as pinned in Cargo.lock
  - Supplied Windows preview probe using Python 3.15.1 and target/debug/patina.exe built at the inspected commit
recheck-when: Configuration parsing, variable loading, predicate evaluation, path resolution, tree enumeration, target ownership, or status classification changes.
---

# Configuration and managed targets

Patina derives a deployment from one repository root, the manifests in its immediate child directories, the active profile, and variable values. A file or whole-directory symlink claims its declared target. A tree-mode directory claims the leaves it materializes. That distinction determines collision validation, status membership, and removal of previously deployed targets.

The implementation supports the declared modes, module-local variable isolation, profile resolution, tree ignores, and target collision checks. The confirmed v1 design also requires an optional machine-wide `variables.toml`, strict string validation in every variable layer, and validation before new pin updates, cache materialization, or deployment. Those variable-loading requirements are not implemented at the inspected revision. Undefined boolean predicates and bare relative target paths also have specific implementation defects; neither defect changes the intended rule.

This document records evidence for [the specification](../../SPEC.md). It does not establish that the inspected tests passed. The only runtime result reported here is the supplied bounded predicate probe. Recovery and rollback are examined in [transaction research](transactions-and-recovery.md), and remote update gates in [remote-source research](remote-sources.md). This document covers their configuration dependencies.

## Evidence and applicability

**Source inspection at `4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc`:** The investigation read the root agent instructions, user guide, architecture and operating-environment documents, configuration/discovery/variable/template/planning/status implementations, and consequential assertions in their unit and integration tests. Source links identify files and symbols; line anchors refer to this revision.

**Confirmed design:** The machine-variable interface, validation scope, and recovery ordering below were confirmed for v1. They are intended behavior, not observations of the baseline executable. Other rules retain the documented intended configuration and target behavior unless an implementation discrepancy is explicitly identified.

**Reported runtime observation, 2026-11-14:** On Windows, a Python 3.15.1 harness invoked `target/debug/patina.exe`, built at the inspected commit, directly against an isolated repository, home, and state directory. A copy-file entry used `when = "not missing"`. `patina apply --json` exited `0` and emitted a plan row with state `create`; it did not materialize the target. This establishes the undefined-predicate defect for that expression and environment. It does not establish behavior on Linux or macOS, or during a confirmed apply.

## Repository and module discovery

[Repository discovery](../../patina-core/src/discovery/repo.rs#L189), in `resolve_repository_root_with`, selects a repository in this order:

1. A nonempty `PATINA_REPO` value. An invalid value fails immediately without falling through to another repository.
2. The first ancestor of the current working directory whose `patina.toml` parses with `[patina] root = true`.
3. The repository path recorded in `<state>/default_repo`.

A selected root must be a directory with a readable, parseable `patina.toml` and the root marker. The returned repository path is canonicalized. The walk-up search skips candidate manifests whose minimal root projection cannot be parsed. Reading the persisted default trims the entire file and accepts it only if the resulting path validates. An absent, unreadable, blank, or invalid pointer does not select a repository. Failure after all sources are exhausted reports the attempted discovery sources. See [`validate_repo_root`, `walk_up_for_root`, and `read_persisted_default`](../../patina-core/src/discovery/repo.rs#L330).

[`discover_modules`](../../patina-core/src/discovery/modules.rs#L101) requires the root marker again, examines immediate child directories, and returns modules sorted by their directory names. A child without a `patina.toml` is not a module. A module declaring `root = true` fails. A manifest at depth two or greater below the repository root fails even when its immediate ancestor is not itself a module. The scan does not traverse directory symlinks. It examines ordinary hidden directories too and does not apply deployment ignore patterns to discovery. A failed recursive `read_dir` inside `check_no_deep_manifests` currently returns successfully, so rejection of every inaccessible deeper manifest is not an established guarantee.

The root supplies root configuration; deployment entries and hooks are read only from discovered module manifests. Root `[[file]]`, `[[directory]]`, and `[[hook]]` sections are not processed as deployments. No profile selects a separate module directory set: every discovered module is parsed, and entry or hook predicates decide applicability. The comment in `profile.rs` about profile-scoped modules does not identify an implemented module-selection schema.

Consequential assertions appear in [repository discovery tests](../../patina-core/tests/repo_discovery.rs), especially `env_var_pointing_at_non_root_directory_errors`, `walk_up_finds_root_from_subdirectory`, and `persisted_default_is_consulted_when_other_sources_fail`; and in [module discovery tests](../../patina-core/tests/module_discovery.rs), covering alphabetical order, depth-two rejection, unexpected root markers, and skipped non-module directories. The inaccessible-directory and directory-symlink boundaries were inspected in source, not exercised here.

## Manifest schema and defaults

Root and module parsers deserialize different projections of the same TOML format. Unspecified schema keys generally pass through as ignored keys rather than producing an unknown-field error. This permissiveness is distinct from field-type errors and explicit placement checks. See [`RawRoot`](../../patina-core/src/config/root.rs#L201), [`RawModule`](../../patina-core/src/config/mod.rs#L206), and [`RawEntry`](../../patina-core/src/config/file_entry.rs#L496).

### Root configuration

| Root declaration | Fields and defaults | Consumer |
| --- | --- | --- |
| `[patina]` | Boolean `root` must be `true` for discovery. Optional `ignore` is an array of strings, default empty. Optional `remote_min_age` is a duration string. | Discovery, tree filtering, remote update policy. |
| `[variables]` | Optional repository-shared variable table. | Module templates and predicates after layering. |
| `[profiles.<name>.variables]` | Optional table for a named profile. A bare `[profiles.<name>]` contributes an empty table. | Active-profile variables. |
| `[[auto_match]]` | Required string fields `when` and `profile`; rules retain declaration order. | Profile resolution. |
| `[[remote]]` | Required `url`; optional `name`, `ref`, and `min_age`. Name derives from the URL when omitted; absent or blank `ref` uses the default branch. | Remote registry and pinned source selection. |

The age fallback is remote `min_age`, root `remote_min_age`, then 72 hours. Remote durations use the supported `s`, `m`, `h`, and `d` notation. Root parsing validates every profile variable table, including inactive profiles, and rejects duplicate remote identities. Remote naming, URL, duration, and update rules are documented in [Remote sources](../REMOTE_SOURCES.md); the parsing boundaries are [`parse_root_config_str`](../../patina-core/src/config/root.rs#L149) and [`RawRemote`](../../patina-core/src/config/remote.rs#L411).

### Module deployment declarations

`[[file]]` and `[[directory]]` share these fields:

| Field | Accepted shape and meaning |
| --- | --- |
| `source` | Required string path, relative to the declaring module or selected remote checkout. |
| `target` | One string destination; mutually exclusive with `targets`. |
| `targets` | Nonempty array of string destinations; mutually exclusive with `target`. Order is retained. |
| `mode` | Optional string selected from the table-specific modes below. |
| `when` | Optional MiniJinja expression string. Absence means the entry applies. |
| `remote` | Optional root-registry name. Whitespace is trimmed; an explicitly blank name is rejected. Absence selects the module's local tree. |
| `ignore` | Optional string-pattern array for `symlink-tree` and directory `copy`; default empty. |

[`ManagedEntry::from_raw_file`, `from_raw_directory`, and `resolve_targets`](../../patina-core/src/config/file_entry.rs#L292) reject these inputs:

- Both `target` and `targets` are supplied.
- Neither destination field is supplied.
- `targets` is empty.
- A source or any destination has an ASCII control character, including `U+0000` through `U+001F` or `U+007F`.
- An explicit mode is not accepted for that entry kind.
- A local file source ending in `.tmpl` also declares any explicit mode.
- A local directory source ends in `.tmpl`.
- A file or whole-directory symlink declares a nonempty `ignore` list.
- An entry explicitly selects a blank remote name.

The control-character error reports the first offending code point and escapes the path. Spaces and non-ASCII characters remain valid. Empty individual source or target strings are not explicitly rejected by this parser; downstream path and source checks determine their result. This is not a documented guarantee that an empty path is useful.

The documented placement rule forbids an `ignore` key on files and whole-directory symlinks. The implementation tests whether its vector is empty, so `ignore = []` passes in those positions. This is a narrow parser discrepancy. Module `[patina] ignore` is rejected even when empty. Module `[remote]` and `[[remote]]` declarations are rejected. Module auto-match and profile sections are not consumed.

[File-entry tests](../../patina-core/tests/config_file_entry.rs) assert defaults, fan-out, explicit template-mode rejection, control-character diagnostics, and ignore placement. [Root parser tests](../../patina-core/src/config/root.rs#L240) assert shared/profile tables, reserved keys, bare profiles, remote declarations, duration errors, and duplicate names.

### Hook declarations

A module `[[hook]]` has required string fields `event` and `command`; optional string fields `shell` and `when`; and optional boolean `must_succeed`, default `true`. Accepted events are exactly `pre_apply` and `post_apply`; other names fail. Command and explicit shell strings are retained verbatim. Predicates use the declaring module's resolver; the command itself is not a template. Shell selection, execution, failure handling, and transaction timing belong to the apply contract. Evidence: [`HookEntry::from_raw`](../../patina-core/src/config/hook_entry.rs#L58), [hook schema tests](../../patina-core/tests/config_hook_entry.rs), and [apply hooks](../USER_GUIDE.md#apply-hooks).

## Profile and variable resolution

### Profile selection

[`profile::resolve`](../../patina-core/src/profile.rs#L213) chooses one profile with this precedence:

1. Nonempty `PATINA_PROFILE`, accepted verbatim without trimming.
2. The first nonempty trimmed line of `<state>/profile`.
3. The `profile` string from the first matching root `[[auto_match]]` rule.
4. An empty string when no source selects a profile.

An absent persisted profile file falls through; other read failures are errors. A named profile need not have a corresponding variable table. Such a name still becomes `patina.profile`, but contributes no profile variables. There is no `--profile` CLI flag. Root auto-match rules are parsed before selection; malformed rule fields can therefore fail even when an environment variable would select the profile.

Auto-match predicates evaluate against built-ins alone. Repository, module, profile, machine, and CLI variables are unavailable. `patina.profile` is undefined because selection has not produced it yet. A variable in an unevaluated short-circuit branch is not accessed. Evidence: [`load_auto_match_rules`](../../patina-core/src/profile.rs#L293), [profile resolution tests](../../patina-core/tests/profile_resolution.rs), and [CLI auto-match tests](../../patina-cli/tests/auto_match_predicates.rs).

### Variable layers and values

The resolver's precedence, from highest to lowest, is CLI overrides, machine variables, the active profile, the declaring module, repository-shared variables, and built-ins. [`Resolver::get`](../../patina-core/src/variables/mod.rs#L250) implements this order independently of the order in which layers are supplied. Repeated bindings within a layer replace an earlier binding; repeated `-v key=value` options therefore use the last value. CLI splitting uses the first `=`, preserving additional equals signs and empty values. The CLI rejects an empty key or missing separator. See [`parse_overrides`](../../patina-cli/src/cmd/apply.rs#L673).

Each module receives a clone of the broader resolver, then its own `[variables]` table. Its variables do not become available to another module through discovery order. Profile and CLI layers still override module values. This scoping applies to entry predicates, template rendering, and hook predicates. Evidence: [`module_contexts`](../../patina-core/src/apply/engine.rs#L547) and [variable layer tests](../../patina-cli/tests/variable_layers.rs), particularly `each_module_renders_under_its_own_variables_table` and `a_hook_when_reads_its_own_module_binding`.

**Baseline implementation:** Bindings are strings. TOML parsers preserve arbitrary values in variable tables, but [`table_to_layer`](../../patina-core/src/apply/engine.rs#L1110) keeps only string-valued members. Numbers, booleans, arrays, dates, and nested tables are silently dropped. A string such as `"false"` remains a nonempty string for MiniJinja truthiness; it is not converted to a boolean. No recursive table merging or variable-value template expansion is implemented. [`reject_reserved_keys`](../../patina-core/src/variables/mod.rs#L70) rejects keys beginning exactly with `patina.` in user layers. The bare `patina` context object is selected specially by [`PatinaContext::get_value`](../../patina-core/src/template/strict.rs#L141), so a user binding named `patina` does not replace it.

**Confirmed design:** Every user variable layer must reject non-string values and reserved `patina.` names instead of dropping invalid values. This includes repository, module, profile, and machine TOML tables; CLI values already arrive as strings. Invalid input fails new-work validation even if a higher layer would override it. This is stricter than the baseline TOML projection.

### Machine variables and validation order

**Confirmed design:** The optional machine-wide file is `<state>/variables.toml`, with bindings under `[variables]`. A missing file or missing table contributes an empty layer. The file is independent of the active repository. Its values override profile, module, and repository values; explicit CLI values override it. Auto-match remains built-ins-only.

The file uses the existing [state-directory locations](../OPERATING_ENVIRONMENT.md#where-patina-stores-state): `$XDG_STATE_HOME/patina` or `$HOME/.local/state/patina` on Linux, `$HOME/Library/Application Support/patina` on macOS, and `%LOCALAPPDATA%/patina` on Windows. Only Linux has the documented XDG override. [`state_dir::compute_root`](../../patina-core/src/state_dir.rs#L145) defines these paths. State is shared by repositories on a machine, so the same machine variables apply when discovery selects another root.

**Implementation gap:** [`build_planning_context`](../../patina-core/src/apply/engine.rs#L854) never loads a machine-variable file or calls `with_per_machine`. The library exposes the layer and its precedence, and the [user guide](../USER_GUIDE.md#variables) promises it, but neither the filename nor a loader exists in baseline production code.

**Confirmed ordering:** Authorized recovery is independent of current variable configuration and precedes new-work validation. After recovery, variable configuration must validate before new pin updates, remote-cache materialization, or deployment. A malformed machine file must not prevent recovery of already journaled work. This distinguishes recovery effects from new deployment effects.

**Implementation gap:** [`cmd::apply::run`](../../patina-cli/src/cmd/apply.rs#L108) currently performs eligible `--update` work before recovery and before constructing the plan. Machine-file loading and strict variable validation are absent. Adding validation only inside current planning would leave pin updates before validation. State-directory creation is also an existing effect of [`state_dir::resolve_with_env`](../../patina-core/src/state_dir.rs#L128); the confirmed new-work boundary does not establish that baseline previews avoid all state writes.

### Built-in names

| Built-in | Baseline value and availability |
| --- | --- |
| `patina.os` | `macos`, `linux`, or `windows` on supported hosts; other compiled targets use Rust's raw OS name. |
| `patina.arch` | Rust's target architecture string, such as `x86_64` or `aarch64`. |
| `patina.hostname` | OS hostname query; empty string when the query fails. |
| `patina.user` | OS username query; empty string when the query fails. |
| `patina.home` | `USERPROFILE` on Windows and `HOME` elsewhere; empty string when unavailable. |
| `patina.profile` | Undefined during selection; afterward the selected name, including the empty fallback. |
| `patina.env.<NAME>` | Process environment value at lookup time; unset or invalid-Unicode values are undefined. |

The first five facts are captured when [`Builtins::current`](../../patina-core/src/variables/builtins.rs#L49) constructs the context. Environment lookups are dynamic. [Variable tests](../../patina-core/tests/variables.rs) assert precedence, reserved-name rejection, environment behavior, and profile injection. No performance guarantee follows from the snapshot implementation.

## Conditions and template output

The same MiniJinja grammar serves entry `when`, hook `when`, auto-match rules, and local template bodies. Predicate results use truthiness rather than requiring a boolean result. Conditions are expression text, not a `{{ ... }}` template. Undefined output and invalid expressions produce errors with the missing variable name or evaluator diagnostic. The template exception permits an undefined condition in an `{% if %}` block to select its `{% else %}` branch; content in an untaken branch is not evaluated. These rules are documented in [`template::Engine`](../../patina-core/src/template/mod.rs#L1) and asserted in [template strictness tests](../../patina-core/tests/template_strict.rs).

An entry's false predicate is checked before source canonicalization, source existence/kind validation, and remote checkout lookup. It contributes no operation, collision claim, or current managed target. Parse-time schema validation still applies because modules are parsed before predicates are evaluated. A previously recorded target excluded by a changed condition can become orphaned and enter the next consented removal plan. Evidence: [`gate_and_resolve_entry`](../../patina-core/src/apply/engine.rs#L1326), [conditional-entry tests](../../patina-cli/tests/conditional_entries.rs), [source-kind validation](../../patina-cli/tests/source_kind_validation.rs#L122), and [condition-driven orphan tests](../../patina-cli/tests/symlink_tree_orphans.rs#L198).

A local file declaration with a case-insensitive `.tmpl` extension selects rendering implicitly. The target is written exactly as declared; Patina does not remove a suffix from the target name. Output is a regular file. Template input is read as UTF-8, so invalid UTF-8 input fails. The executor renders once for the targets supplied to one render call and writes the same bytes to each. Planning renders separately to classify targets, and the human diff can render again; no immutable rendered result is retained across planning, preview, and execution. Evidence: [`classify_entry`](../../patina-core/src/apply/engine.rs#L1405), [template executor](../../patina-core/src/apply/template.rs#L24), and [diff renderer](../../patina-cli/src/output/diff.rs#L54).

Directory traversal does not give `.tmpl` leaves implicit rendering. Directory `copy` copies their bytes and `symlink-tree` links them as leaves. Remote `.tmpl` sources are ordinary bytes because the local implicit-template rule is disabled. A remote directory named with `.tmpl` is therefore accepted. Remote content does not supply configuration or executable template instructions. See [`TemplatePolicy`](../../patina-core/src/config/file_entry.rs#L273) and [Remote sources](../REMOTE_SOURCES.md).

### Undefined boolean predicate defect

The intended predicate rule rejects evaluated undefined variables while preserving short-circuit branches that are never evaluated. Baseline [`Engine::new`](../../patina-core/src/template/mod.rs#L131) configures MiniJinja `SemiStrict`, which permits undefined truthiness checks. [`coerce_when_result`](../../patina-core/src/template/mod.rs#L193) rejects an undefined final value but accepts a defined boolean even if obtaining it accessed an undefined variable. The dependency is pinned to MiniJinja 2.23.0 in [Cargo.lock](../../Cargo.lock#L559).

Source inspection of that dependency's `UndefinedBehavior::is_true` and VM `Not` and `JumpIfTrueOrPop` instructions explains why `not missing` and `missing or true` can produce `true`. The reported Windows preview probe observed the first expression succeed and plan a copy target. Existing tests assert rejection of a bare undefined predicate, undefined equality operands, and an undefined right-hand operand of `true and missing`; they do not cover the defined-boolean result. This remains a defect under the intended rule, not an additional predicate exception.

## Source and target paths

A local source is joined to the module directory and canonicalized through the filesystem. Absolute paths, `..`, and local symlinks can resolve outside the module or repository; [`resolve_entry`](../../patina-core/src/apply/engine.rs#L1587) explicitly retains this latitude. It does not expand a source's `~` or render source/target paths as templates. A remote source must resolve within its pinned checkout. Remote directory sources reject included symlink leaves; ignored leaves of tree modes do not trigger that check, while a whole-directory symlink checks the unfiltered tree.

After an entry's predicate succeeds, a missing source fails planning. A `[[file]]` whose canonical source is a directory and a `[[directory]]` whose source is not a directory fail with diagnostics naming the matching declaration kind. The baseline file-kind check accepts any non-directory metadata, not only regular files. FIFO, device, socket, and other special-file behavior was not exercised and should not be inferred from regular-file tests. Evidence: [`source_metadata` and `validate_source_kind`](../../patina-core/src/apply/engine.rs#L1745), [source-kind tests](../../patina-cli/tests/source_kind_validation.rs).

Target `~` and `~/...` expand to the current home. `~user`, an interior tilde, and a leading tilde followed by a backslash have no special handling in [`expand_tilde`](../../patina-core/src/paths.rs#L308). Relative targets resolve against the invocation's working directory, so one spelling can identify different locations between invocations. The [user guide](../USER_GUIDE.md#declaring-dotfiles) recommends absolute or home-relative targets for this reason.

The intended target-location rule resolves the parent chain while preserving the final target component. An existing target symlink identifies the location to replace, not its referent. Links in ancestors above the declared target are resolved as part of the filesystem layout. Missing parents are permitted; paths use lexical resolution until an existing anchor is available. Windows verbatim prefixes are simplified. Evidence: [`canonicalize` and `resolve_location`](../../patina-core/src/paths.rs#L114), [path tests](../../patina-core/tests/path_canon.rs), and `resolve_location_does_not_follow_a_leaf_symlink` in `paths.rs`.

### Bare relative target defects

`resolve_location` falls back to full-path canonicalization without a usable parent/leaf split. For bare relative `rc`, the parent is empty. If `rc` is an existing symlink, that fallback follows the leaf to its referent. `./rc` takes the protected branch. This is a source-traced violation of the target-location rule; no runtime reproduction was performed here.

Status has a related identity mismatch. [`insert_declared_entry`](../../patina-core/src/apply/engine.rs#L973) expands tilde but otherwise supplies declared targets directly to [`manage_key`](../../patina-core/src/status/mod.rs#L303), which leaves a bare relative target relative. The journal stores the absolute planned target. Their keys therefore differ, so a previously applied bare-relative target can be reported orphaned. Both defects require preserving intended absolute target identity; referent writes and false orphan reports contradict the documented model.

## Modes, enumeration, and ignores

| Declaration | Accepted mode | Materialized entries |
| --- | --- | --- |
| Local or remote ordinary `[[file]]` | `symlink`, the default | One symbolic link at each target. |
| Local or remote ordinary `[[file]]` | `copy` | One regular byte-copy file at each target. |
| Local `.tmpl` `[[file]]` | No explicit mode | One regular rendered-output file at each target. |
| `[[directory]]` | `symlink`, the default | One link to the complete source directory at each target. |
| `[[directory]]` | `symlink-tree` | One link per included leaf, with real intermediate directories. |
| `[[directory]]` | `copy` | One byte-copy file per included leaf, with real intermediate directories. |

Enum names `SymlinkDir` and `CopyTree` are not accepted manifest spellings. Explicit `template`, `symlink-dir`, and `copy-tree` modes are rejected. The declaration table supplies source kind. [Schema tests](../../patina-core/tests/config_file_entry.rs) assert accepted and rejected names; [symlink-tree tests](../../patina-cli/tests/symlink_tree.rs) assert leaf links and real intermediate directories.

[`walk_files`](../../patina-core/src/apply/mod.rs#L410) returns sorted source-relative leaf paths. It descends real directories and treats non-directory entries, including symlinks, as leaves. Empty directories produce no leaf and are not materialized by tree modes. Local symlink leaves are not recursively traversed; their precise behavior depends on the chosen copy/link executor and host filesystem. Directory symlinks inside local source trees and special-file leaves were not tested here.

Root `[patina] ignore` patterns precede each entry's `ignore` patterns. Matching is case-insensitive on all platforms, anchored at the individual entry's source root, and uses gitignore-style glob rules. Later matches override earlier ones. Negation can restore a path only if the directory walk reaches it: `build/` followed by `!build/keep.txt` still excludes the leaf because descent stops at `build`. No default patterns apply. Patina does not read `.gitignore` or `.patinaignore` to supplement manifests. Evidence: [`ignore_rules::build` and `prunes`](../../patina-core/src/ignore_rules.rs#L74) and [ignore documentation](../USER_GUIDE.md#excluding-generated-files-with-ignore).

Pattern compilation errors name the offending pattern and fail strict planning. Repository patterns are compiled for each active entry, including modes that do not enumerate a tree; an invalid root pattern can therefore fail a file-only plan. A valid root list does not filter an explicit file or contents exposed by a whole-directory symlink. Conditions excluding every entry can avoid entry-level pattern compilation.

Planning and status distinguish removed sources from currently ignored sources. [`split_source_leaves`](../../patina-core/src/apply/engine.rs#L1031) walks without filters and partitions leaves using the same ancestor-pruning rule. Consequently, planning and status can read inside ignored source directories, although execution prunes them before descent. An inaccessible ignored directory can still fail strict planning. This follows the source and [architecture explanation](../ARCHITECTURE.md#tree-enumeration-and-ignore); an ignore rule is not an access-control boundary.

An empty included-leaf set does not create an absent target directory and classifies as unchanged. Recorded leaves newly excluded by patterns become removal candidates labelled `ignored`. A later negation preserves a source leaf only if its ancestors remain reachable. [Ignore tests](../../patina-cli/tests/ignore_lists.rs) assert case-insensitive matching, root/entry precedence, directory pruning, labelled removals, unreachable negations, and an all-ignored tree that settles without repeated pending work.

## Collision and tree-target validation

Only active entries claim targets. Fan-out destinations participate individually, including duplicates within one entry. Files and whole-directory symlinks claim declared locations; tree modes claim included leaves. Independent entries may therefore share a tree's target directory when they do not claim the same leaf. A new source leaf can make a previously valid configuration collide.

[`validate_targets`](../../patina-core/src/apply/collisions.rs#L155) rejects duplicate identities, then rejects any other claimed target strictly below a whole-directory symlink target. Diagnostics identify both module/source declarations and the parent tree target for a conflicting tree leaf. Validation precedes the displayed diff and deployment writes, although earlier source resolution may have fetched a remote cache entry.

Identity comparison resolves existing parent ancestors while preserving leaf locations, then applies Unicode NFC normalization, lowercase mapping, and NFC normalization again. Comparison is case-insensitive and normalization-insensitive on all platforms. This is not full Unicode case folding: `ß` remains distinct from `ss`. Status membership uses the same folding function. Evidence: [`comparison_key`](../../patina-core/src/apply/collisions.rs#L201), [`caseless::fold`](../../patina-core/src/caseless.rs#L25), and collision unit tests for case, normalization, parent spellings, fan-out, and leaf-symlink identity.

[CLI collision tests](../../patina-cli/tests/target_collisions.rs) assert rejection before target creation, mutually exclusive predicates sharing one target, and a separate file below a copy-tree target when no source leaf overlaps it. A file-target ancestor/descendant conflict is not rejected by the whole-directory containment rule; later filesystem operations can fail. The evidence does not establish rejection of arbitrary path containment.

A current tree root that is a symlink is classified for replacement as a unit; leaves below it are not read through the link for classification. A link between the root and any current included leaf is rejected as `InteriorSymlink`. Ancestors above the declared root may resolve through links. The interior check derives paths from included leaves, so it does not inspect excluded subtrees or every historical target. Evidence: [`classify_target`](../../patina-core/src/apply/engine.rs#L1457), [`verify_interior_dirs_real`](../../patina-core/src/apply/mod.rs#L363), [mode-switch tests](../../patina-cli/tests/mode_switch.rs), and [symlink-tree tests](../../patina-cli/tests/symlink_tree.rs). Orphan removal and revert paths have further limitations described in [the architecture](../ARCHITECTURE.md#target-kind-and-mode-edits) and [transaction research](transactions-and-recovery.md).

## Planning and status observe different baselines

Planning emits active files before active directories, preserving declaration order within each group across alphabetically ordered modules. Every declared entry occupies its index even when its predicate is false. Fan-out targets retain declaration order; tree leaves are sorted. Removed recorded targets are appended in sorted target order. Evidence: [`plan`](../../patina-core/src/apply/engine.rs#L418), [`assemble_plan_operations`](../../patina-core/src/apply/engine.rs#L1264), and unit tests for file/directory order and skipped-entry indices.

Apply classification compares a target with what the current source and resolver would materialize. Missing targets are `Create`, matching targets are `Unchanged`, and other existing entries are `Update`. Matching content requires a regular file with the expected bytes, so an equal-content symlink does not match a copy or rendered file. Matching symlinks use recorded or desired link text after Windows verbatim-prefix simplification; arbitrary alternative relative-link spellings are not equivalent. Tree classification compares each leaf. Evidence: [`classify_leaf`](../../patina-core/src/apply/classify.rs#L47), [`content_matches` and `symlink_matches`](../../patina-core/src/status/classify.rs#L120).

Status compares recorded targets against expectations stored by the last apply or record edit. It does not report every newly declared but unapplied target. Current configuration determines membership; recorded expectations determine drift:

- `clean`: A still-managed target matches its recorded entry kind and bytes or link destination.
- `drifted`: A still-managed target exists but does not match that expectation.
- `missing`: A still-managed recorded target is absent.
- `orphaned`: A recorded target exists but current configuration no longer manages it.

Dropped targets already absent are omitted. Without a committed record, status has no recorded file rows. A source or manifest mode edit alone need not produce drift when the target still matches its recorded expectation. The architecture text implying a mode edit necessarily makes status drifted is broader than [`status::report`](../../patina-core/src/status/mod.rs#L168) and [`classify`](../../patina-core/src/status/classify.rs#L62). The recorded-state model should govern that wording. [Status tests](../../patina-cli/tests/status_cli.rs) assert clean counters, edited copies, missing targets, removed declarations, link replacement, and one row per fan-out target.

Status and doctor rebuild membership through [`current_managed_targets`](../../patina-core/src/apply/engine.rs#L934). They evaluate current conditions and expand tree leaves, but do not render templates or enforce strict source-kind validation. Missing or unreadable local trees contribute no leaves. Invalid ignore patterns fall back to an empty matcher. Remote directory entries without a usable cached checkout contribute indeterminate roots, preserving membership of recorded leaves below them and producing a warning. Registry or lockfile lookup failures on this path do not necessarily fail status. A remote file declaration still contributes its target without enumerating the checkout.

Apply and status accept repeated `-v` overrides. An override-gated target can remain managed with the original values, become orphaned under another defined value, or cause an undefined-variable error without a fallback. Apply derives deployment operations and orphan membership from the same resolver; it does not recalculate removals without its overrides. [Override-gated removal tests](../../patina-cli/tests/override_gated_reap.rs) assert these distinctions.

The next apply can remove recorded targets dropped by declarations, conditions, source leaves, or patterns. [`detect_orphans`](../../patina-core/src/apply/engine.rs#L2538) excludes these cases:

- The target is already absent or is a real directory.
- The target is a current tree root.
- The target is below a current tree root replaced by a non-directory.
- Another managed ancestor now covers the target.

Newly ignored leaves have reason `ignored`; other removals have reason `unmanaged`. This explains why a directory symlink replacing a former tree must protect its source from removal. Transaction backup, consent, and rollback behavior is defined separately.

## Variable consumers and command side effects

| Consumer | Resolver and relevant baseline effects |
| --- | --- |
| `apply` | Full context with CLI overrides; evaluates entries, renders templates, and later evaluates hook predicates. Active remotes can populate the cache during planning. Eligible updates and recovery precede planning. |
| `status` | Shared context with CLI overrides; recomputes membership without fetching remotes. State resolution and lock handling can create state entries. |
| Human apply diff | Uses each operation's module resolver and can render templates again. |
| `remove` | Full plan without CLI overrides; can fetch remotes before confirmation. Reconstructs templates with the current owning module resolver, or the broader resolver when no owner is found. |
| Defender `apply` and `status` | Full plan without overrides; conditions and templates must resolve and planning can fetch remotes. Defender `clear` does not plan. |
| `doctor` | Ignored-target query uses shared membership context without overrides. Query errors suppress that finding rather than fail the whole report. |
| Remote-management commands | Parse root remote configuration directly; do not build a deployment variable resolver. |
| `add` | Parses manifests for declarations and ignores without evaluating conditions or constructing the resolver. |
| `promote` | Uses recorded source/target information without constructing the resolver. |

Evidence: [`build_planning_context`](../../patina-core/src/apply/engine.rs#L854), [`cmd::status::run`](../../patina-cli/src/cmd/status.rs#L37), [`cmd::remove::run` and `reconstruct_content`](../../patina-cli/src/cmd/remove.rs#L99), [`cmd::defender`](../../patina-cli/src/cmd/defender.rs#L143), [`deployed_but_now_ignored`](../../patina-cli/src/cmd/doctor.rs#L463), [`remote::inventory`](../../patina-core/src/remote/update.rs#L68), [`cmd::add`](../../patina-cli/src/cmd/add.rs#L445), and [`cmd::promote::run`](../../patina-cli/src/cmd/promote.rs#L90). These dependencies identify consumers of the machine-variable layer; they do not require unrelated recovery or remote-inventory commands to depend on that file.

## Remaining evidence limits

This investigation did not run the full test suite or platform matrix. Existing assertions establish which contracts are deliberately checked, not that the checkout passes them. The supplied predicate preview is the only executed configuration example cited here.

Permission failures, inaccessible ignored directories, directory-symlink source leaves, special files, path changes during execution, and every supported filesystem's Unicode identity behavior were not runtime-tested. Source branches do not justify stronger guarantees for these conditions. Target-location defects are source-traced; the undefined `not missing` defect has the additional Windows preview observation.

The machine-variable filename, table, precedence, strict typing, and validation order are confirmed intended behavior with identified baseline gaps. No machine-variable loader or acceptance tests exist at this revision. Refresh this evidence when that implementation changes; the baseline's silent value dropping and absent loader are not compatibility obligations.
