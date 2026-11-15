---
spec: ../SPEC.md
depends-on: [runtime.md]
---

# Configuration and target resolution

Configuration selects a repository, its immediate-child modules, an active profile, and the desired target entries. A module is a directory immediately below the repository root with its own `patina.toml`. Profiles select variables; they do not select modules.

## Repository and manifest requirements

### REQ-repository-discovery — Repository discovery

Commands that need a repository select the nonempty `PATINA_REPO` environment value first, otherwise the nearest ancestor of the invocation directory whose `patina.toml` declares `[patina] root = true`, otherwise the trimmed path in `<state>/default_repo`. An explicit invalid environment selection fails instead of falling through. The selected root is canonicalized and validated. A root manifest supplies repository settings, variables, profiles, automatic profile rules, and the remote registry; deployment entries and hooks belong in modules.

**Scenarios:**

- A valid default pointer and an invalid nonempty `PATINA_REPO` are present; apply reports the explicit selection error instead of deploying the default repository.
- The invocation directory is below a marked repository; discovery selects that ancestor before consulting the default pointer.
- The root manifest declares a file but no module declares it; that root entry does not produce a deployment operation.

### REQ-module-discovery — Module discovery

Discover modules in alphabetical name order. Reject a `patina.toml` nested below the immediate-child module level, including under a directory that has no module manifest. Do not descend directory symlinks during module discovery. Hidden ordinary directories follow the same depth rules.

**Scenarios:**

- Modules `zsh` and `git` exist; deployment resolves `git` before `zsh`.
- `scratch/nested/patina.toml` exists without `scratch/patina.toml`; discovery rejects the nested manifest.
- A child symlink points to a separate directory tree with manifests; discovery does not traverse it as a module tree.

### REQ-manifest-declarations — Manifest declarations

Module manifests accept `[[file]]`, `[[directory]]`, `[variables]`, and `[[hook]]`. Each file or directory entry has a string `source` and exactly one of string `target` or nonempty string-array `targets`. Optional fields are `mode`, expression string `when`, remote-name string `remote`, and tree-only string-array `ignore`. Fan-out applies one source independently to every listed destination in authored order. Reject missing required fields, wrong field types, both target forms, an empty targets array, and control characters U+0000–U+001F or U+007F in source or target paths. Diagnostics escape control characters.

The root `[patina]` table accepts `root`, repository-wide `ignore`, and `remote_min_age`. Modules do not declare their own remote registry or module-wide remote selector, and do not put ignore patterns in `[patina]`. Local `.tmpl` file entries select rendering implicitly and reject every explicit mode. Supported mode spellings and effects are:

| Declaration | Mode | Target entries |
| --- | --- | --- |
| Ordinary file, including remote files | `symlink`, the default | A symbolic link to the source at each destination. |
| Ordinary file, including remote files | `copy` | A regular file with the source bytes at each destination. |
| Local file with case-insensitive `.tmpl` extension | Omitted | A regular file with rendered bytes at each destination. |
| Directory | `symlink`, the default | One symbolic link to the complete source directory. |
| Directory | `symlink-tree` | Leaf symlinks under real destination directories. |
| Directory | `copy` | Leaf byte copies under real destination directories. |

Local directory sources ending in `.tmpl` are invalid. An entry-level `ignore`, even an empty array, is invalid for files and whole-directory symlinks. Undocumented mode names such as `template`, `copy-tree`, and `symlink-dir` are invalid. Unknown keys do not supply behavior or override these rules.

**Scenarios:**

- A file has `targets = ["~/a", "~/b"]`; both destinations participate in planning and collision checks.
- An entry supplies both `target` and `targets`; parsing fails before deployment.
- A local `rc.tmpl` entry specifies `mode = "copy"`; parsing fails instead of silently copying or rendering it.
- A whole-directory symlink entry supplies `ignore = []`; parsing rejects the unsupported field.

## Variable and expression requirements

### REQ-profile-selection — Profile selection

Select the active profile from nonempty `PATINA_PROFILE`, otherwise the first nonempty trimmed line of `<state>/profile`, otherwise the first true ordered `[[auto_match]]` rule, otherwise the empty profile name. Environment selection preserves its value verbatim. Each automatic rule has string `when` and `profile` fields and evaluates using built-ins only, with `patina.profile` undefined. A selected name need not have a declared `[profiles.<name>.variables]` table; an absent table contributes no variables. After selection, `patina.profile` is the selected string, including the empty fallback.

**Scenarios:**

- Environment profile `work` and file profile `home` are present; `work` wins.
- Two automatic rules match; the first rule supplies the profile.
- An automatic rule reads an evaluated repository variable or `patina.profile`; evaluation fails as undefined.
- `PATINA_PROFILE=travel` has no corresponding table; the profile remains `travel` with an empty profile-variable layer.

### REQ-variable-resolution — Variable resolution

For each module, resolve string variables in this descending precedence: explicit `-v key=value` arguments, machine variables, active-profile variables, that module's variables, repository variables, and built-ins. Lower layers remain available for keys not overridden. Module variables do not affect another module. User variable values are literal strings; they are not recursively merged or rendered as templates.

Machine variables are read from optional `<state>/variables.toml`, under `[variables]`, independently of the selected repository. A missing file or missing table supplies an empty layer. Malformed TOML or a non-table `variables` value fails validation. Every user layer rejects non-string values and keys beginning with reserved `patina.`, even if a higher layer would override that binding. A user key named `patina` does not replace the built-in context object.

CLI bindings split at the first `=`. An empty value is valid; an empty key or missing separator is invalid. Repeated CLI keys use the last value. Commands without variable arguments use the same resolver with an empty CLI layer when they need configuration evaluation.

**Scenarios:**

- CLI, machine, and profile all define `editor`; the CLI value wins, while an unrelated machine value remains available.
- Two modules define different `theme` values; each module's template uses its own value.
- A repository variable is an integer and CLI overrides its name with a string; validation still rejects the repository value.
- Machine configuration has `[variables]` with `"patina.os" = "other"`; validation fails before new deployment work.
- `-v token=a=b -v empty=` supplies strings `a=b` and the empty string.

### REQ-builtin-context — Built-in context

Expose `patina.os` as `macos`, `linux`, or `windows`; `patina.arch` as the host target architecture string; and `patina.hostname`, `patina.user`, `patina.home`, and the selected `patina.profile` as strings. Failed hostname or username lookup produces an empty string. Home uses `USERPROFILE` on Windows and `HOME` elsewhere, with an empty value when unavailable. `patina.env.<NAME>` reads the named process environment variable; absent or invalid-Unicode values are undefined.

**Scenarios:**

- On a Windows host with `USERPROFILE` set, `patina.os` is `windows` and `patina.home` uses that path.
- An unset environment name is rendered directly; evaluation reports an undefined-variable error.
- The hostname query fails; template output using the hostname receives an empty string rather than a fabricated host name.

### REQ-condition-evaluation — Condition evaluation

Entry conditions, hook conditions, and automatic-profile rules use MiniJinja expressions and result truthiness. They are expressions without template delimiters. Evaluating an undefined variable fails, including boolean negation or truthiness operations that otherwise produce a defined boolean. Unevaluated short-circuit branches do not fail. Invalid syntax fails with an evaluator diagnostic.

A false entry condition excludes its source resolution, remote lookup, target claims, and deployment operation. Manifest schema validation still applies. Excluding a previously managed target changes current membership and can make it eligible for removal under the deployment rules.

**Scenarios:**

- `when = "not missing"` evaluates an undefined variable; planning fails instead of selecting the entry.
- `when = "false and missing"` does not evaluate the right operand and excludes the entry without error.
- A false-condition remote entry lacks a pin; it does not fetch the remote or fail for that missing pin.
- A false-condition entry has both target forms; manifest parsing still fails.

### REQ-template-rendering — Template rendering

Local template files use MiniJinja syntax, UTF-8 source text, and the owning module's resolved context. Rendering writes regular-file output to the exact declared target name. Undefined output fails, with the exception that an undefined condition in an `{% if %}` block can select its `{% else %}` branch; an untaken branch is not evaluated. Invalid UTF-8 and invalid template syntax fail before deployment target writes. Directory modes treat `.tmpl` leaves as ordinary content. Remote templates remain literal under the remote-content trust invariant.

**Scenarios:**

- A local template declares target `~/config.tmpl`; the rendered regular file keeps that exact name.
- A directory-copy source has `config.tmpl`; the destination has its literal bytes.
- A template directly outputs a missing variable; planning reports the missing name and does not deploy its output.
- An undefined if-condition has an else branch with literal text; rendering selects that branch.

## Path and tree requirements

### REQ-source-target-locations — Source and target locations

Resolve local sources relative to the module directory through existing filesystem links. Local sources can be absolute or resolve outside the repository. Source paths do not expand `~` or evaluate templates. After a true condition, a missing source or a source whose file/directory kind contradicts the declaration fails planning. Remote source containment follows the remote area.

Expand target `~` and `~/...` from the current home; other tilde forms have no special meaning. Resolve relative targets against the invocation directory. Target identity resolves existing parent ancestors and missing lexical components while preserving the final target component: an existing leaf symlink identifies the location to replace, not its referent. Apply, status, removal, and collision checks use this same absolute location identity. Simplify Windows verbatim path prefixes for comparisons.

**Scenarios:**

- A module source `../../shared/rc` resolves to an existing local file outside the repository; it can supply a file entry.
- A file declaration resolves to a directory; planning reports the required declaration kind.
- Bare relative target `rc` is an existing symlink; copy deployment replaces that link without overwriting its referent, just as target `./rc` does.
- A relative target was applied from a directory and status runs there again; status matches the recorded absolute target identity.

### REQ-tree-enumeration — Tree enumeration

Tree modes enumerate included source-relative leaves in sorted order and create real intermediate destination directories as needed. They descend real source directories and treat non-directory entries as leaves; they do not expand local directory symlinks recursively. Empty source directories produce no target leaf. An entirely empty or ignored tree does not create an absent destination root.

**Scenarios:**

- A source tree has `b/rc` and `a/rc`; leaves are planned in source-relative sorted order.
- Every leaf is ignored and the target root is absent; apply does not create that root.
- A local source tree has a symlink leaf; tree enumeration returns the leaf instead of traversing the linked directory.

### REQ-ignore-patterns — Ignore patterns

For each tree entry, apply root `[patina] ignore` patterns followed by that entry's `ignore` patterns. Use case-insensitive gitignore-style matching anchored at the entry's source root, with later matches overriding earlier ones. An excluded directory prunes descent, so a negation below it cannot restore a leaf unless the ancestors are reachable. Apply no default ignore patterns and do not load `.gitignore` or `.patinaignore`. Valid root patterns do not filter explicit file entries or the content exposed by whole-directory symlinks.

Invalid patterns fail strict planning when an active entry compiles them, including file-only plans. Ignore rules control inclusion, not filesystem access: inventory for prior-target membership can read ignored directories. Recorded leaves newly excluded by ignores become removal candidates with reason `ignored`.

**Scenarios:**

- Root ignores `*.log` and an entry later negates `keep.log`; a reachable `KEEP.LOG` leaf is included.
- Patterns are `build/` and `!build/keep.txt`; the leaf remains excluded because its parent is pruned.
- A root ignore matches the name of an explicit file source; that file remains explicitly deployed.
- A previously deployed tree leaf becomes ignored; the next plan proposes its removal rather than silently abandoning it.

### REQ-target-collisions — Target collisions

Before displaying the deployment diff, reject duplicate active target identities and any claimed target strictly below a whole-directory symlink target. Files and whole-directory symlinks claim their declared destinations; tree modes claim their included leaves. Compare identities by Unicode NFC normalization, lowercase mapping, then NFC normalization on every platform. This comparison does not equate `ß` with `ss`. Report both conflicting declarations and the containing tree destination when relevant.

**Scenarios:**

- Two active entries claim case variants of the same location on Linux; planning rejects the collision.
- Two targets in one entry normalize to the same identity; planning rejects them.
- A copy tree and a separate file share a destination directory but claim different leaves; collision validation permits them.
- A new tree leaf overlaps that separate file; the next plan fails before target mutation.
- Two mutually exclusive conditions select the same target; only the selected entry claims it.

### REQ-tree-target-links — Tree target links

Treat a current tree root that is a symlink as a unit to replace, without classifying current leaves through that link. Reject an interior symlink between a declared tree root and a current included destination leaf. Links above the root resolve as part of its location. This gate covers current included leaves; it does not establish containment for every historical or externally changed path.

**Scenarios:**

- A tree target root points to an unrelated directory; planning classifies replacement of the link without reading destination leaves through it.
- A current included leaf would be written through a symlink below the tree root; planning rejects the interior link.
- The tree root's parent is a symlink; location resolution uses that parent before applying the interior-tree check.

## Supporting evidence

[Configuration and target research](../docs/research/configuration-and-targets.md) traces these contracts to the inspected manifests, source, tests, and confirmed machine-variable design. It records strict-undefined and relative-target defects, untested special-file behavior, and platform limits. [Transaction research](../docs/research/transactions-and-recovery.md) explains recovery ordering and historical-path limits.
