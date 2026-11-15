---
spec: ../SPEC.md
depends-on: [runtime.md, configuration.md, history.md, deployment.md, remote-sources.md]
---

# Repository management

These commands explicitly change repository declarations, source content, or the recorded ownership of targets. They do not implicitly run a deployment of unrelated entries.

## Setup and adoption

### REQ-repository-initialization — Repository initialization

`patina init [path]` creates a root `patina.toml` and records the canonical absolute repository path in `<state>/default_repo`. Without a path it uses the current working directory. A missing target directory is created. The scaffold has `[patina] root = true` and a parseable RFC 3339 `created_at` value. The timestamp is configuration content and is not included in command-result stdout.

Initialization does not prompt; `--yes` is accepted without changing this behavior. An existing manifest is refused before repository writes. A successful human result identifies the new root and the next adoption step. JSON success has string fields `initialized` and `default_repo`, naming the canonical repository and pointer-file paths.

**Failure behavior:** An existing manifest exits 1 and preserves it. Its JSON refusal has `error: "already_exists"`, `path`, and `message`. Directory creation, manifest publication, canonicalization, or pointer publication can fail independently. Failure reports which step did not complete; a created manifest can remain when pointer publication fails.

**Scenarios:**

- Initialize a new directory: create the marked root and a pointer that discovers it from a later unrelated working directory.
- Initialize a directory that already has `patina.toml`: exit 1 without replacing the manifest or updating the default pointer.
- Manifest creation succeeds but pointer publication fails: report the pointer failure and leave a complete valid manifest available for repair.
- Repeat equivalent successful initialization in isolated identical environments: stdout does not expose the newly generated `created_at` value.

### REQ-dotfile-adoption — Dotfile adoption

`patina add <path>` imports a file or directory into a selected immediate module and adds the corresponding manifest declaration. It copies content into the repository and does not replace or remove the original target; a later apply materializes the declaration.

The path can be absolute, home-relative with a leading `~`, or relative to the command's working directory. Stored targets under the resolved home use `~`-relative spelling; other targets are absolute. The source preserves the final filename, including a leading dot. Template mode appends `.tmpl` to that source filename and omits an explicit mode from the declaration.

The caller supplies `--module` and one mode flag. An interactive invocation prompts for either missing value; a noninteractive invocation refuses missing values. Once both are selected, adoption does not ask for another confirmation. `--yes` does not supply a missing module or mode. The module identifies one immediate child directory of the selected repository, not an arbitrary external destination.

| Input kind | Accepted mode flags | Declaration |
| --- | --- | --- |
| File | `--symlink`, `--copy`, `--template` | `[[file]]`. |
| Directory | `--symlink`, `--copy`, `--symlink-tree` | `[[directory]]`. |

For a top-level symlink input, adoption imports the content reached by the input. Directory import copies regular files and subdirectories; a file symlink supplies its referent's bytes. It does not promise to preserve an imported tree's symlink structure or to expand directory symlinks recursively.

**Failure behavior:** Refuse an already managed target, an incompatible kind/mode pair, the home directory itself, the repository or an ancestor of it, and an occupied derived repository-source path. A refusal does not overwrite that source or add the declaration. Read or copy failures can leave a partial imported directory; each completed file follows REQ-repository-file-publication. A later manifest error can leave imported content without a declaration.

**Scenarios:**

- Add `~/.zshrc` with `--module shell --copy`: copy it to `shell/.zshrc`, declare that source and home-relative target, and preserve the original file.
- Add a file with `--template`: create its source with the extra `.tmpl` suffix and declare no explicit mode.
- Add a directory with `--template`: exit 1 without staging it as a file template.
- The derived source is already occupied by a file, directory, or dangling symlink: refuse instead of replacing it.
- Supply `--module ../outside`: refuse a destination outside the immediate module structure.

### REQ-adoption-ignore-conflicts — Adoption ignore conflicts

Adoption checks whether an existing tree-mode declaration excludes the imported destination through its ignore rules. It refuses that conflicting adoption unless `--force` is supplied. `--force` overrides this refusal only; it does not supply confirmation, overwrite an occupied source, or bypass other validation.

When adopting a directory as a tree-mode deployment, report source leaves that the effective repository ignore rules will omit. A whole-directory symlink does not apply leaf filtering and does not report those leaves as omitted deployment content.

**Scenarios:**

- A tree rule ignores the derived source; ordinary add refuses, while the same valid add with `--force` can declare it.
- Supply `--yes` without `--force` for that conflict: the ignore refusal remains.
- Import a directory with a file omitted by a global ignore: tree-mode adoption warns; whole-directory symlink adoption does not claim that file will disappear through the link.

## Ownership changes

### REQ-file-removal — File removal

`patina remove <path>` stops managing one recorded file target and removes that destination from its `[[file]]` declaration. It supports a scalar `target` and one member of `targets`. Other destinations remain declared in their original order; removing the last destination removes the entry. Unrelated entries, settings, and comments remain.

Without `--purge`, the command replaces the target with a regular file made from the current recorded source. A local template source is rendered with current variables in the owning module's context. Remote source bytes remain literal, including a `.tmpl` suffix. The command does not promise historical last-applied bytes or preservation of the target's latest external edits. `--purge` removes the target instead.

The command requires a recorded target and an editable file declaration. Directory declarations and individual tree leaves require a manifest edit or an ignore-rule change; this command does not remove them. Removing a file does not delete its repository source, run hooks, apply new entries, or repair unrelated targets.

**Failure behavior:** An unrecognized target or unsupported declaration is refused with exit 1 before recovery or target changes. Target and declaring manifest changes are journaled together and recover together before an uncommitted removal is retried. Current source or template failures prevent a non-purge replacement; purge does not require reconstruction of replacement bytes. A completed removal remains an ownership change protected through older rollbacks.

**Scenarios:**

- Source, deployed bytes, and a later target edit differ; ordinary removal leaves current source bytes as a regular file and removes only the selected declaration destination.
- A file has `targets = ["~/a", "~/b"]`; remove `~/a`: preserve `~/b` in the declaration and do not rewrite its deployed content.
- Remove the sole remaining destination: delete the file entry while preserving unrelated configuration.
- A remote `.tmpl` source has `{{ missing }}` text: ordinary removal preserves those literal bytes rather than evaluating them.
- Terminate removal after editing its manifest but before commit: an authorized retry restores target and manifest before performing the new removal.
- Request removal of a tree leaf: refuse without executing a broader apply or silently editing the tree's ignore list.

### REQ-copy-promotion — Copy promotion

`patina promote <target>` reads a recorded local copy target, replaces its recorded repository source with the current target bytes, and records those bytes as the target's applied expectation. A tree-copy leaf is promoted individually. It does not deploy other targets, run hooks, or rewrite unrelated source files.

Promotion refuses symlink targets, rendered-template targets, remote-backed targets, unrecognized expectation kinds, and targets absent from recorded history. If authorized recovery changed the target or an ancestor, promotion refuses after that recovery instead of copying the restored bytes as though they were the edit the user selected.

**Failure behavior:** Refusals exit 1. A source replacement and the following history publication are distinct steps; source replacement can succeed before a later record failure. Report that partial result. Source replacement follows REQ-repository-file-publication. A completed promotion is protected through older rollbacks; newer applies remain reversible normally.

**Scenarios:**

- Edit a recorded copy target and promote it: the source has the edit and status compares against the newly recorded bytes.
- Promote one leaf of a copy-mode directory: only that leaf's source and expectation change.
- Promote a remote-backed copy: refuse without editing the cached checkout.
- Recovery restores an ancestor of the selected target: preserve available recovery copies and refuse promotion of the restored content.
- Source publication succeeds but record publication fails: exit 1 and identify the partial completion; do not report that status has accepted the new expectation.

### REQ-management-consent — Management consent and recovery

Remove and promote resolve their target, acquire exclusive coordination, and perform eligibility checks before asking for confirmation. `--yes` supplies confirmation. Otherwise interactive stdin accepts only trimmed `y` or `Y`; other input, EOF, and input failure decline. A noninteractive invocation without `--yes` declines with exit 5.

Refused and declined commands do not recover pending work and warn when it remains. After consent, recover authorized old work independently of current variable configuration. Validate new variable-dependent work before new cache preparation or mutation. Remove permits recovery of its interrupted declaration edit even when that edit already removed the declaration.

**Scenarios:**

- A pending apply exists and a promotion is refused because its target is a symlink: leave the pending operation unrecovered and report the reason.
- Decline a valid remove prompt: preserve target and manifest and leave pending recovery for a later authorized command.
- Accept removal after an interrupted removal deleted the declaration: recover that edit, then retry against the restored declaration.
- Current machine variables are malformed while an eligible confirmed operation needs recovery: recover the recorded old work, then reject the invalid new variable-dependent operation.

## File and result contracts

### REQ-repository-file-publication — Repository file publication

Initialization, adoption, and promotion publish each repository file as complete content. Process interruption leaves a complete prior file, a complete replacement, or continued absence when creating a previously absent file. It does not leave a truncated replacement. This guarantee applies separately to each file; it does not make an imported directory or a multi-step command atomic.

Errors identify the failed operation and the already completed steps that affect repair or retry. Existing source content is not assumed recoverable merely because its directory is a Git repository. Power-loss durability remains outside scope.

**Scenarios:**

- Interrupt promotion during source replacement: the source has either the complete previous bytes or the complete promoted bytes.
- Interrupt adoption while publishing its manifest: the manifest is complete old or new content, while imported files can already exist.
- A partially completed import is retried and its source path is occupied: report the occupied source instead of overwriting it silently.

### REQ-management-json — Management JSON results

Successful management commands emit the following JSON fields when `--json` is selected. All path fields are strings. `added`, `removed`, and `promoted` identify the command's named subject; `target` identifies the resolved deployed location where present.

| Command | Success fields |
| --- | --- |
| `init` | `initialized`, `default_repo`. |
| `add` | `added`, `module`, `mode`, `source`. |
| `remove` | `removed`, `target`, boolean `purged`. |
| `promote` | `promoted`, `target`, `source`. |

Typed init/add/remove refusals use string fields `error`, `path`, and `message`. Promotion refusals use `error`, `target`, and `message`. Refusal identifiers are `already_exists` for an existing initialization manifest; `already_managed`, `overreaching_target`, `source_exists`, and `ignored_conflict` for the corresponding adoption refusals; `not_managed` and `tree_leaf` for the corresponding removal refusals; and `not_managed`, `symlink_target`, `template_target`, `remote_backed_target`, `unpromotable_target`, and `recovered_target` for promotion. Additional validation and engine failures follow the shared error-stream contract. Adoption's reported `mode` is `symlink`, `copy`, `template`, or `symlink-tree`.

**Scenarios:**

- Successful purge removal produces `purged: true` and identifies both the named and resolved targets.
- Promotion refuses a remote-backed target: emit `error: "remote_backed_target"` and a target-specific message without a success result.
- An occupied initialization manifest produces `error: "already_exists"` with exit 1.

## Supporting evidence

See [command and platform research](../docs/research/commands-and-platform.md), [configuration research](../docs/research/configuration-and-targets.md), and [transaction research](../docs/research/transactions-and-recovery.md). Complete individual-file publication and multi-target removal are intended obligations; baseline code inspection does not establish their implementation.
