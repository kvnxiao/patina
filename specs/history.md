---
spec: ../SPEC.md
depends-on: [runtime.md, configuration.md]
---

# Deployment history and recovery

Machine-local history records target expectations, pre-change backups, and ownership changes. Recovery reverses incomplete journaled work. Rollback reverses one retained completed operation per invocation, subject to preserved ownership decisions.

## Persistence and recovery requirements

### REQ-journal-publication — Journal publication

Before journaled target mutation, publish a complete durable plan and complete backups of entries to be overwritten or removed. Preserve file, directory, and symbolic-link kinds, including dangling links. Publish a complete committed record only after the operation's required effects succeed. An interrupted publication does not expose a partial backup or partial terminal record as complete. A committed record records actual post-hook content expectations for managed targets and the dispositions needed to reverse the operation.

History uses unique monotonically ordered operation identifiers even when the wall clock repeats or moves backward. A `.plan` without either a `.COMMIT` or `.ROLLED_BACK` terminal file is pending. Terminal-file existence prevents recovery from treating committed or closed work as pending, even when that terminal body's contents cannot be decoded. A progress cursor is advisory; it does not override filesystem and backup evidence.

**Scenarios:**

- The process terminates while copying a backup; recovery does not restore the incomplete staged copy as a complete backup.
- Commit publication succeeds but pending-file cleanup fails; the operation remains committed and is not automatically reversed.
- Two operations observe the same clock time; their identifiers remain distinct and ordered.
- A progress cursor says a write finished, but the filesystem and complete backup show otherwise; recovery follows the filesystem and backup state.

### REQ-recovery-authorization — Recovery authorization

Recover pending work only through a command whose recovery policy authorizes mutation. Interactive human apply and `apply --yes` authorize recovery before planning the new deployment. `rollback` authorizes it after confirmation or `--yes`. `remove` and `promote` authorize it after their target eligibility checks and confirmation or `--yes`. A noninteractive apply preview, JSON apply without `--yes`, and a declined or refused rollback, remove, or promote do not recover.

Recovery of already journaled work uses recorded information independently of current variable configuration. After authorized recovery, validate variables before new pin updates, remote-cache preparation, or deployment. Invalid current variables can fail new work after old work has been recovered. Eligibility checks do not impose current-variable validation as a prerequisite to recovery.

**Scenarios:**

- An interrupted apply exists and the machine variable file is malformed; `apply --yes` recovers old work, then fails new-work validation before updating pins or fetching content.
- A noninteractive `apply` sees pending work; it reports the pending state and previews without reversing it.
- Interactive apply recovers old work and the user declines the subsequent new diff; the old work remains recovered and the new deployment does not run.
- The user declines rollback; pending targets and history are not changed by recovery.

### REQ-interrupted-work-recovery — Interrupted work recovery

Under the exclusive command lock, process pending operations in identifier order. Reverse recorded changes using complete backups and target state. Leave `unchanged` targets alone. Restore a complete backup when its live target differs or is absent, preserving a differing live entry first. If the live entry already matches its backup, avoid rewriting it. For a recorded creation without a complete backup, preserve and delete an existing live target; an absent target needs no action. For a recorded update or removal without a complete backup, leave the observed target state alone.

Remove pending history only after its reversal succeeds. A failure preserves the pending plan and usable recovery material for retry. Report restored work and preserved-copy locations when available. Recovery is eventual after process interruption under the operating assumptions in the root spec, rather than simultaneous atomic visibility across targets.

**Failure behavior:** An update target deleted externally before the backup pass can retain newly written bytes after recovery because its recorded update has no backup. External changes after backup and before writing are outside the coordinated-edit assumption. Recovery can follow symlink ancestors introduced outside the current-tree checks; it does not establish universal containment against hostile filesystem changes.

**Scenarios:**

- A pending creation has a live file and no backup; recovery saves the live file and removes the target.
- A pending update has a complete backup and changed live bytes; recovery preserves those bytes and restores the backup's content and entry kind.
- Recovery fails halfway through an operation; another authorized invocation can retry using the retained plan and backups.
- A target is already equal to its backup; retry does not replace it or create an unnecessary duplicate recovery copy.

### REQ-recovered-copy-preservation — Recovered copy preservation

Before recovery discards a differing live entry, or rollback discards an entry that differs from its committed expectation, save a usable copy below `<state>/recovered/` and report its path. Copies use `<operation-id>.<pass>/<index>/<name>`, with numeric passes starting at one. A later changed retry preserves another version instead of overwriting an earlier copy. An existing identical copy can be reused. Preserve symbolic links as links, including their link flavor on Windows.

**Scenarios:**

- A copied target is edited after deployment; rollback saves the edit before restoring pre-deployment bytes.
- A failed recovery already preserved one version and the target is edited again; retry preserves both versions under distinct recovery passes.
- A dangling target link must be preserved; its recovery copy remains a dangling link rather than an attempted copy of its referent.

## Rollback and retained expectations

### REQ-history-rollback — History rollback

With consent, rollback first recovers pending work and then selects the latest effective retained operation. A deployment record restores removed targets before reversing managed entries in reverse application order. Restore pre-change content and entry kind for changed targets; delete creations that have no prior backup. Preserve unchanged targets and auxiliary parent directories that are not recorded target entries. Mode and timestamp bits are outside rollback fidelity.

A removal or promotion record is passed without changing targets and returns `record_only`; a subsequent invocation can continue to earlier history. A successful reversal closes the selected record and returns `rolled_back`. No available record is an error. Noninteractive rollback without `--yes` previews and exits successfully without recovering or requiring a history record. JSON preview returns `{"result":"previewed"}`; execution returns the corresponding `result` string.

**Scenarios:**

- An apply replaced a directory link with a copy tree; rollback restores the backed-up link kind and preserves differing live tree content before replacement.
- An apply left a target unchanged and that target is subsequently edited; rollback leaves the edit in place.
- The latest operation is a promotion; the first rollback returns `record_only` without changing files, and the next can reverse an earlier apply.
- There is no history; `rollback --yes` fails, while noninteractive rollback without consent remains a preview.

### REQ-rollback-failure-preservation — Rollback failure preservation

Rollback handles removed targets as one compensation group and each managed declaration's targets as another. Before reversing a group, retain snapshots sufficient to restore that group's state at the start of the attempt. On reversal failure, attempt that compensation. Earlier successfully reversed groups need not be redone; rollback is not a whole-command transaction.

If compensation also fails, return failure, identify affected paths, preserve usable snapshots and retryable history, and describe restoration as incomplete. A retry does not discard the only remaining usable snapshots before resolving the incomplete group. Do not close the operation as successfully rolled back until its required reversal completes.

**Scenarios:**

- Reversing the second target in a declaration fails and compensation succeeds; the declaration's first target returns to its pre-rollback state, and the command fails with retryable history.
- Compensation cannot restore that first target; the command identifies the incomplete restoration and retains its usable snapshot instead of claiming success.
- An earlier declaration was successfully reversed before a later group failed; the error does not promise that the entire rollback had no effects.

### REQ-ownership-history — Ownership history

Completed removals and promotions persist ownership changes independently of whether their own records have been passed by rollback. When reversing older records, preserve targets affected by later ownership changes. If restoring an ancestor directory or link would replace a protected descendant, skip that entire restore unit. Unrelated entries in that unit can consequently remain in their current state. Newer deployments of those targets remain reversible normally.

Retained records preserve cumulative ownership decisions needed by older history. A promotion updates the selected target's recorded expected bytes; a removal excludes its target from managed expectations. These records mark remaining managed targets unchanged and do not deploy new declarations or overwrite unrelated drift.

**Scenarios:**

- A target is removed, then an earlier apply is rolled back; the removed target remains unmanaged with its current bytes.
- A promoted target is edited again, then history is traversed past the promotion; that current content remains protected.
- Restoring an old directory backup would replace a protected promoted descendant; rollback skips the whole directory restoration.
- A removed target is later declared and applied again; rollback can reverse the newer apply while retaining the older removal decision when crossing it.

### REQ-history-retention — History retention

After a newly committed deployment, removal, or promotion, retain the ten newest committed operation identifiers and their backups. Preserve pending plans and their recovery material. Pruning does not expose an old retained plan as newly pending by deleting its terminal file first. After a newly committed apply, separately retain the ten newest recovered-copy directories ordered by operation identifier and numeric pass. A full no-op apply, standalone recovery, or rollback does not trigger that recovered-copy pruning.

**Scenarios:**

- Eleven completed operations and one pending operation exist; pruning retains ten completed operations and the pending operation's recovery material.
- Recovered passes `.9` and `.10` compete for retention; numeric ordering treats `.10` as newer.
- Repeated no-op applies run after recovery; they do not prune recovered copies through the new-commit retention path.

### REQ-history-format-compatibility — History format compatibility

Journal plans and committed records use a two-byte little-endian major-version envelope followed by a body encoded with the Postcard binary serialization format. Before v1.0, the major stays `1` while layouts can change without migration. A higher unsupported major is refused. Release compatibility at v1.0 requires an explicit major-version transition rather than treating prerelease layouts as stable.

A committed record is active until rollback passes or reverses it; that action closes the record. An undecodable active committed record can be skipped in favor of older readable history. An undecodable closed record fails history traversal because its persistent ownership information can be necessary. A terminal record's existence still prevents treating its plan as pending. The user guide defines the manual prerelease upgrade procedure; failure to decode old data does not authorize guessing its content.

**Scenarios:**

- A record declares a higher major than the binary supports; decoding reports the unsupported version.
- The newest active commit body is truncated but an older record is readable; history can select the older readable record without recovering the truncated terminal as pending.
- A closed record needed for ownership traversal is corrupt; traversal fails instead of discarding its possible ownership decisions.

## Status requirements

### REQ-recorded-target-status — Recorded target status

`status` compares targets from the latest effective committed history with their recorded expectations. Current declarations determine whether a recorded target is still managed. Classify a matching managed target as `clean`, an absent managed target as `missing`, a present mismatching managed target as `drifted`, and a present no-longer-managed target as `orphaned`. Omit a no-longer-managed target that is absent. Copy and template expectations require a regular file with the recorded bytes; symlink expectations compare the recorded link destination after Windows prefix simplification.

New declarations absent from history and source-only edits are not independently target drift. Status does not fetch a missing remote tree checkout to reconstruct its footprint; it warns and protects indeterminate recorded membership from being declared orphaned solely because that cache is unavailable. Pending-work diagnostics distinguish lock-confirmed interruption from a potentially running writer when reading without a lock.

**Scenarios:**

- A source file changes while its copied target still matches recorded bytes; status remains clean, while apply can propose an update.
- A recorded copy is replaced with an equal-content symlink; status reports drift.
- A declaration disappears but its target remains; status reports orphaned.
- A remote tree cache is missing; status does not download it or assert that all its recorded leaves are now unmanaged.

### REQ-status-json — Status JSON

`status --json` returns `last_apply`, `files`, numeric `clean`, `drifted`, `missing`, and `orphaned` counts, and `remotes_pending`. `last_apply` is null or an object with stored `at`, `user`, and `host` values. File rows have `path` and `state`; state is one of the four classifications above. `remotes_pending` is an array of remote names and follows the remote area's observation rules; supplemental observation metadata distinguishes its freshness. Repeating status against unchanged inputs does not insert a fresh invocation timestamp.

**Scenarios:**

- No effective deployment exists; JSON has null `last_apply`, an empty file list, and zero file-state counts.
- One recorded target is missing and another drifted; rows and counters agree exactly.
- A cached remote observation is stale; machine-readable output identifies that condition instead of presenting pending membership as a fresh network check.

## Supporting evidence

[Transaction and recovery research](../docs/research/transactions-and-recovery.md) records persistence ordering, compensation defects, retention, corruption behavior, and filesystem limits. [Configuration research](../docs/research/configuration-and-targets.md) distinguishes desired-content planning from recorded-state status and documents identity defects.
