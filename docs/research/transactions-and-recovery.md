---
date: 2026-11-14
questions:
  - What transaction, consent, recovery, and rollback guarantees does Patina provide?
  - How do hooks, concurrency, retained history, and output failures affect command outcomes?
  - Which baseline behaviors support the intended contract, and which require correction?
observed-in:
  - Patina source revision 4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc
  - Supplied Windows observations using Python 3.15.1 and the baseline debug CLI
recheck-when: Apply orchestration, hook execution, journal formats, command persistence, rollback compensation, retention, or lock behavior changes.
---

# Deployment transactions and recovery

Patina's deployment guarantee concerns recovery after process interruption. An uncommitted apply or removal is reversed from its journal and backups before an authorized recovering command performs new work. A committed operation remains committed. This guarantee does not imply that every command is an atomic transaction, that an interrupted process leaves every target immediately restored, or that arbitrary hook effects are reversible. The published guarantee excludes power loss and documents several concurrent-edit and symlink limitations.

The intended contract preserves command-specific consent and recovery ordering. It also requires consistent hook-failure handling, successful command outcomes after committed deployments despite output or housekeeping failures, complete individual repository-file replacements, and honest reporting when rollback compensation fails. Several baseline code paths do not meet those decisions. Their current behavior is evidence of a gap, not a compatibility obligation.

## Evidence and applicability

**Source inspection at revision `4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc`:** This report traces CLI orchestration, deployment classification and execution, backup publication, journal readers and writers, orphan recovery, rollback replay, status classification, advisory locks, and meaningful test assertions. Repository paths and line references below refer to that revision. The inspected working tree had the user's untracked `DISCUSS_VACUOUS_TEST_AUDIT.md`; it was not changed.

**Supplied runtime observations on Windows:** The coordinator ran two isolated probes using Python 3.15.1 and `target/debug/patina.exe` built from the baseline. Both used temporary repository, home, and state directories. One closed the read end of stdout before `apply --yes --json`; the copy target was deployed, the process exited `0`, and stderr was empty. The other applied a copy entry, then changed its repository source and its deployed target to different bytes; `remove --yes --json` left the newer repository-source bytes at the target. These are coordinator-supplied observations, not probes rerun for this report.

**Verification limit:** No complete passing baseline conformance run was available. The baseline lint gate stopped at `clippy::double_must_use` in `patina-elevate/src/lib.rs:116`; a subsequent integration run built the CLI but stopped at lock-helper executable-path inference in `add_cli`. Test descriptions below report inspected assertions, not a claim that the full suite passed. The supplied probes establish their particular Windows outcomes, not all operating-system or failure combinations.

**Intended-contract evidence:** The approved reconstruction decisions distinguish desired behavior from the baseline where they differ. This report records the implications and supporting observations; the specification owns the requirements. Existing product claims come from [the product and quality guidance at the inspected revision](https://github.com/kvnxiao/patina/blob/4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc/AGENTS.md), [the user guide](../USER_GUIDE.md), [the architecture description](../ARCHITECTURE.md), and [the operating-environment constraints](../OPERATING_ENVIRONMENT.md).

## Transaction boundaries by command

| Command | Intended persistence boundary | Baseline evidence and limitations |
| --- | --- | --- |
| `apply` | Journaled deployment changes recover to their prior state until the committed record is published. | `apply/engine.rs:1926–2129` journals, backs up, writes, runs post-hooks, removes orphans, and commits. Ordinary execution errors can leave a pending plan for a later recovering command. |
| `remove` | Its target and declaring manifest recover together before commit. A committed removal is an ownership change that ordinary rollback passes. | `journal/removal.rs:40–82` journals both paths and backs up both before either write. `cmd/remove.rs:171–174` attempts recovery on an execution error. |
| `init` | Individual file writes preserve a complete old or new file after process interruption; the command may partially complete. | `cmd/init.rs:83–93` writes the manifest, then the default-repository pointer. Neither write has a command recovery plan; baseline writes are direct. |
| `add` | Individual repository-file replacements preserve complete old or new bytes; source import and manifest editing may partially complete. | `cmd/add.rs:309–330` copies the source before reading, editing, and directly writing the manifest. A failure can leave an imported source without its declaration. |
| `promote` | Individual source replacement preserves complete old or new bytes; source replacement and recording the new expectation may partially complete. | `cmd/promote.rs:123–126` directly writes the repository source, then commits its hash. A failure can update the source while retaining the older recorded expectation. |
| `rollback` | Restore retained history with per-entry compensation and explicit incomplete-failure handling. There is no whole-command atomicity guarantee. | `rollback/mod.rs:180–233` restores removed targets, then reverses managed entries, then closes the record. Earlier groups can already be reversed when a later group fails. |

The approved repository-edit policy requires complete individual files across process interruption without requiring a new journal for the whole `init`, `add`, or `promote` command. Baseline direct writes in those commands, and `discovery/repo.rs:303–313` for the default pointer, do not establish that file-level guarantee. A direct write can truncate the old file before completing the new bytes. This is an implementation gap; retaining multi-step partial completion does not permit partially written replacement files.

The broad quality statement that every overwrite is backed up so rollback can restore it cannot describe every repository edit. The command-specific documentation explicitly says rollback passes removal and promotion records, while init and add do not create deployment history. Deployment-target backups and interrupted-removal recovery have defined scope; a Git repository does not by itself preserve uncommitted source bytes. Extending automatic command recovery to every repository-editing command was not selected.

## Consent, recovery, and validation ordering

### Apply

**Observed source behavior:** `cmd/apply.rs:113–177` builds the request, optionally updates remotes, recovers when the invocation may execute, builds its plan, processes stale pins, checks pending work for previews, renders the diff, obtains consent, checks Windows symlink prerequisites, and executes. The interactive human path may recover before presenting its new deployment prompt. Declining that later prompt therefore does not undo an already completed recovery.

| Invocation | Recover prior interrupted work? | Execute new deployment? |
| --- | --- | --- |
| Human apply with `--yes` | Yes, before planning. | Yes, subject to validation and execution failures. |
| Human apply on interactive stdin without `--yes` | Yes, before planning and the deployment prompt. | After an accepted prompt; a full no-op skips the prompt. |
| Human apply on noninteractive stdin without `--yes` | No. | Preview only. |
| JSON apply without `--yes` | No, even with interactive stdin. | Preview only. |
| JSON apply with `--yes` | Yes. | Yes; hook stdout is redirected to stderr. |

The prompt accepts a trimmed response of `y` or `Y`; other responses, EOF, and input-read errors decline it. Human deployment refusal exits `5`; a noninteractive or JSON preview exits `0` when planning succeeds. A preview with pending work reports the filesystem as the interrupted operation left it and warns instead of recovering. Evidence: `cmd/apply.rs:180–245`, `:376–409`, and `:491–514`.

**Approved ordering:** Authorized recovery of previously journaled work is independent of current variable configuration. After that recovery, variable configuration must be validated before new pin updates, remote-cache work, or deployment begins. This separates authorization to restore an already recorded operation from validation of a new request. Baseline `build_request` precedes recovery, remote update precedes recovery and planning, and the full variable context is built during planning; those paths need reconciliation with the approved ordering.

Recovery itself reads plans, backups, and target paths rather than re-rendering current sources. An invalid current source, template, profile, or variable configuration must not be treated as evidence that a prior operation is unrecoverable. Command-specific target eligibility and consent still apply to remove and promote before their recovery authorization.

### Rollback, remove, and promote

Rollback recovers only after `--yes` or interactive confirmation. A noninteractive rollback without `--yes` prints a generic preview and returns before reading history or recovering; a declined rollback exits `5`. Once authorized, the core acquires the exclusive lock, recovers pending operations, reports recovery, removes abandoned rollback staging, and reads the active history record. It can therefore report successful recovery and then fail because no committed apply remains. Evidence: `cmd/rollback.rs:43–111`; `rollback/mod.rs:180–213`; CLI `apply_crash_recovery.rs:331–378`.

Remove and promote acquire the exclusive state lock and perform their target checks before recovery. Refused or declined commands do not recover. Remove identifies a permissible manifest edit before asking for consent and allows a recorded interrupted removal to recover a declaration that operation already deleted. After recovery it recomputes the plan when necessary, then reconstructs replacement content and executes its journaled removal. Promote refuses after recovery if that recovery changed the selected target or an ancestor; it does not silently promote the restored bytes. Evidence: `cmd/remove.rs:107–174`, `cmd/promote.rs:100–126`, and CLI `apply_crash_recovery.rs:380–501`.

These ordering differences are part of the intended interface. A single rule saying every mutating command recovers at startup would change when declined or invalid commands write.

### Preview writes outside deployment targets

The strong established preview boundary is that previews do not change repository declarations or deployed targets and do not recover interrupted work. Planning may fill a cold remote cache. State-directory resolution also creates state subdirectories, and status opens or creates the state lock file. Consequently, blanket phrases such as status writes nothing are broader than the baseline implementation. Evidence: `state_dir.rs:114–136` and `:182–186`; `lock.rs:209–219`; `status/mod.rs:168–192`. Tests that snapshot a previously initialized state directory do not establish behavior on an entirely fresh state directory.

## Deployment state and idempotency

Planning computes a desired materialization and a disposition for each target: `Create` for an absent target, `Update` for an existing mismatching entry, and `Unchanged` for a matching entry. A content target must be a regular file with matching bytes; a symlink whose destination has those bytes still differs in kind. A symlink target matches the recorded destination after Windows verbatim-prefix normalization. Evidence: `apply/classify.rs`; `status/classify.rs:88–128`; `apply/engine.rs:1405–1571`.

Tree entries have an aggregate disposition and in-memory per-leaf dispositions. An absent tree root with materialized leaves is `Create`; a present root with any changed leaf is `Update`; a tree with all matching leaves is `Unchanged`. An absent root with no included leaves is also `Unchanged`, so an empty or fully ignored tree does not produce repeated work. The durable pending plan records the aggregate, while a committed record stores individual materialized leaves and their dispositions. This difference matters: interrupted tree recovery restores the root-level backup, whereas committed rollback can skip individual unchanged leaves.

Operations are assembled in deterministic file-entry order followed by directory-entry order across resolved modules. Multi-target declarations retain their target order. Removed-target operations are appended in target-path order. The plan and its managed-target set use the same selected profile, module variable scopes, and command-line overrides; orphan selection does not independently reevaluate conditions with a different variable context. Evidence: `apply/engine.rs:418–544`, `:1264–1324`, and `:2538–2616`.

A full no-op requires every durable operation to be `Unchanged`, no removal operation, and an existing committed expectation record. It returns before resolving hook shells, running hooks, publishing a new plan, writing targets, or creating a new backup cycle. The first empty apply still establishes a baseline record. A partial update does not rewrite unchanged targets; the new commit still records them so status and later orphan handling retain their ownership. Evidence: `apply/engine.rs:1971–1980`, `:2234–2354`, and `:2453–2470`.

The idempotency assertions are substantive: `skip_if_satisfied.rs:98–166` compares journal filenames, backup-cycle filenames, committed bytes, and commit modification time before and after a satisfied reapply. The same test file covers mixed unchanged, created, and updated entries. `deterministic_stdout.rs:27–121` primes the deployment, then compares complete stdout bytes from two subsequent human or JSON applies. These tests support stable repeated satisfied runs, not identical output between the initial changing apply and its later no-op.

At execution time the exclusive lock prevents another cooperating Patina writer from interleaving target writes. It does not freeze source files, external editors, or the previously displayed plan. Execution checks for pending plans and a full no-op again, but does not rebuild every classification or verify a source-content snapshot under the lock. Copies and templates can reread sources. Published concurrent-edit limitations remain relevant; serialization is not evidence that every byte written was immutable since preview.

## Hooks and deployment failures

Hooks are collected in module iteration order and declaration order within each module. Each phase filters that sequence by `event`, then evaluates its optional `when` expression with the declaring module's variables. The command string is passed verbatim to a shell; it is not a template. The default shell is `bash` on Linux and macOS and `pwsh` on Windows. PowerShell executable names, including paths ending in `pwsh` or `powershell`, use `-Command`; other shells use `-c`. Evidence: `apply/engine.rs:487–491`, `:2637–2669`; `apply/hooks.rs:181–312`.

Hook processes inherit stdin, environment, and the invoking process's working directory; the baseline does not set a per-module child working directory. Human apply inherits hook stdout and stderr. JSON apply redirects hook stdout to stderr so the application's stdout remains one JSON document. Hook execution waits synchronously for the child. There is no configured hook timeout, automatic hook retry, or separate hook cancellation protocol in `run_hook`; a long-running hook can retain the deployment lock until it exits.

| Failure or result | Approved outcome | Baseline evidence |
| --- | --- | --- |
| Detectable invalid hook configuration | Reject before deployment changes. | Explicit shells are checked up front, but omitted defaults are not checked there and hook predicates are evaluated during their phase. |
| Required pre-hook invocation or nonzero-exit failure | Abort before planned target writes; report the pre-hook failure. | Nonzero exits return `Aborted` and exit `2`; invocation errors currently propagate as generic errors. |
| Required post-hook invocation or nonzero-exit failure | Reverse deployment changes; report the post-hook failure. | Nonzero exits reverse completed materializations and exit `3` when reversal succeeds; invocation and predicate errors currently bypass that immediate reversal. |
| Optional hook invocation or nonzero-exit failure | Warn and continue. | `must_succeed = false` and `--force-deploy` downgrade nonzero exits only; invocation errors still propagate. |
| Successful hook | Continue the phase in declaration order. | Exit status is classified as successful. |

`must_succeed` defaults to true. `--force-deploy` treats hooks as optional for that invocation; it does not authorize accepting invalid configuration or suppressing warnings. Baseline explicit-shell validation happens before any hook executes, including for an explicit shell on a hook whose predicate would be false. The selected prevalidation policy should be expressed in observable configuration and execution outcomes without treating these implementation details as a requirement to retain late predicate failures.

Two warning paths are defective under the approved policy. The pre-hook wrapper creates a warning vector and discards it (`apply/engine.rs:2623–2633`). The human path prints collected post-hook warnings through `report_result`, but the JSON path chooses a result field and returns without reporting them (`cmd/apply.rs:491–514`). Consistent optional-hook warnings therefore require both phases and both output modes.

Post-hooks run after materialization but before orphan removal. A required post-hook failure therefore reverses completed materializations without having removed the planned orphans. Successful or tolerated hooks permit orphan removal and commit. Post-hooks may themselves edit targets; committed content expectations are read from live files after hooks, so the resulting bytes become the recorded baseline. Arbitrary effects of shell commands outside journaled targets are not described by the deployment journal and have no established automatic-reversal guarantee.

Meaningful test evidence includes CLI `apply_cli.rs:35–88`, which checks exit `3` and deletion of a fresh copied target after a required post-hook fails, then checks exit `0`, surviving deployed bytes, and a warning under force-deploy. Its output tests assert distinct shell-generated stdout and stderr markers and parse the entire JSON output. Core `hooks_must_succeed.rs` tests exit classification, explicit-shell error identity, and selector evaluation; those tests do not exercise every CLI warning path or missing-default-shell failure.

## Journal and backup publication

The journal uses three primary files per operation ID: a pending `.plan`, an advisory `.progress`, and a terminal `.COMMIT`; rollback additionally writes `.ROLLED_BACK`. Plans and committed records use a two-byte little-endian major-version envelope followed by a postcard body. The baseline major is `1` under an explicit pre-release no-migration policy; a higher major is refused rather than decoded. Older bodies using the same major can still be undecodable. Evidence: `journal/plan.rs:1–34`, `journal/record.rs:164–225`, and the pre-release compatibility section of the user guide.

An operation ID has a UTC timestamp prefix and a fixed-width 20-digit sequence. Allocation occurs under the exclusive lock and considers existing journal, backup, and recovered-copy names. If the clock has not advanced beyond the newest recognized ID, the sequence increases; exhausting the sequence returns an error. Wall-clock apply metadata is stored separately. This prevents same-second or backward-clock operations from reusing history IDs. Evidence: `journal/mod.rs:387–441`; its frozen/backward-clock and exhaustion assertions at `:857–895` and `:978–1013`.

The baseline apply publishes persistence in this order:

1. Encode and write the complete plan, sync the plan file, and sync the journal directory.
2. Create the unsynced progress cursor.
3. Back up every target that may be overwritten or removed, outermost first.
4. Perform materializations, recording completions in the progress cursor; run post-hooks, then perform removals.
5. Build the committed record, write it to a staged sibling, sync that file, rename it to `.COMMIT`, and sync the journal directory.
6. Delete plan and progress files, then prune retained history and caches.

Recovery decides that an operation is committed from terminal-sentinel existence, without decoding the sentinel body. Staging the complete commit body before its final rename prevents process interruption during body writing from publishing a truncated terminal record. A failed final cleanup can leave the plan beside a valid COMMIT; it remains committed and must not be reverted. Evidence: `journal/mod.rs:206–262` and `:301–335`.

A backup mirrors the target's absolute path below `backups/<id>/`; Windows drive prefixes map to drive-letter directories and UNC prefixes use `__unc__/<host>/<share>`. Files, directories, and symbolic links are cloned with their entry kinds preserved, including dangling links and Windows directory-link flavor. A backup is copied into a `.partial.<pid>` sibling and renamed only after the complete copy succeeds. Recovery treats incomplete staged siblings as absent backups and removes them. Evidence: `backups/mirror.rs:55–75`, `journal/probe.rs:91–155`, and `fsx.rs`.

The backup pass skips a target inside another target already backed up. A directory snapshot already records the nested entry; a backed-up link is restored as a link rather than traversed to write a nested backup. All backups precede the first materialization. Existing real directories selected only for orphan removal are not backed up because the removal phase never removes them. `Unchanged` aggregate operations are omitted from the backup pass. Evidence: `apply/engine.rs:2396–2432`.

The progress cursor appends a little-endian 32-bit index plus a completion marker. It is not synced per operation and is not authoritative during recovery. The core crash tests deliberately supply misleading progress information and assert that filesystem and backup state determine the reversal (`recovery_crash.rs:426–448`). The journal-sync tests assert plan and staged-commit file syncs, directory syncs, and zero progress syncs; the commit-publication observer asserts the final sentinel is absent while its complete staged body is synced (`journal_fsync_shape.rs:100–148`, `:302–342`). These checks establish call ordering, not hardware power-loss durability.

## Recovery decisions and retained live copies

An orphan journal plan has neither a COMMIT nor a ROLLED_BACK sentinel. A live writer also temporarily has that shape, so only a lock-protected read establishes that the operation was interrupted. Recovery processes orphan IDs in sorted order. It decodes each plan without resolving current manifests and removes the plan and progress only after all reversals succeed. A failure leaves the plan eligible for a later retry. Evidence: `journal/recovery.rs:179–267`.

| Recorded operation and observed backup | Recovery outcome |
| --- | --- |
| `Unchanged` | Leave the target alone. |
| Complete backup exists and live entry already matches it | Leave the target alone; retain any relevant prior recovery-copy report. |
| Complete backup exists and target differs or is absent | Preserve a differing live entry, then restore the backup. |
| No complete backup, disposition `Create`, live target exists | Preserve the live entry, then delete it. |
| No complete backup, disposition `Create`, live target absent | Do nothing. |
| No complete backup, disposition `Update` or operation `Remove` | Leave the target in place or absent as found. |

The last rule distinguishes an unstarted overwrite from a creation because the backup pass precedes writes. It also has the published vanished-update limitation: a target classified `Update` can disappear before backup, then be written by apply; recovery sees no backup and leaves those written bytes. That limitation must not be generalized into a guarantee that every absent backup means the executor never wrote.

A target path whose backup traversal encounters an ancestor symbolic link stashed in the backup cycle is reversed as that ancestor link. This prevents restore operations from following the stashed or restored link into source files. Recovery and rollback preserve link kind; they do not flatten a symlink into destination bytes. Evidence: `journal/recovery.rs:294–333`; `rollback/replay.rs:345–378`; CLI `apply_crash_recovery.rs:897–950`.

Before recovery overwrites or removes a differing live entry, it keeps that entry under `recovered/<id>.<n>/<index>/<name>`. The index is the pending plan operation's position. Copy numbering starts at one and increases beyond the highest existing pass number. A retry can reuse an earlier matching copy and report it; a changed entry is copied to a new pass and the earlier copy is reported before the new one. No successful new pass is intended to overwrite an earlier kept copy. Evidence: `journal/recovery.rs:342–420`; `recovery_crash.rs:321–408` and `:450–549`.

The recovery-copy implementation finds the first earlier copy for an operation when deciding reuse. The tests cover a prior copy, a changed retry, and an earlier failed recovery; they do not establish every sequence of three or more changed and unchanged retries. The documented preservation outcome is stronger than an inference that every retry is deduplicated against the most recently written copy.

Recovery warnings name the reverted operation count and the paths of preserved copies. An error can prevent a complete recovery report from returning even when earlier entries have already been restored; durable plans, backups, and copies remain the basis for retry. Rollback differs by reporting each kept copy through a callback before replacing that entry.

## Commit outcomes, cleanup, and output

**Approved outcome:** Once a deployment is committed, failures while reporting its result or performing housekeeping do not change its successful deployment outcome. Available diagnostics should identify the cleanup failure without implying that the deployment was reversed. This includes journal-file cleanup after commit publication, history pruning, recovered-copy retention, and remote-cache cleanup.

Baseline history pruning and remote-cache pruning already warn without reversing the committed apply. Two paths disagree with the approved rule: `Journal::commit` propagates a plan/progress deletion error after publishing COMMIT, and `execute` propagates recovered-directory garbage-collection errors after commit. On remove, such a commit-cleanup error reaches the caller's recovery attempt; recovery sees the COMMIT and correctly does not reverse it, but the command still returns failure. Evidence: `journal/mod.rs:259–262`, `:444–448`; `apply/engine.rs:2093–2120`; `cmd/remove.rs:171–174`.

Publication failures must be distinguished from post-publication housekeeping. A failed commit-body write or rename can leave an uncommitted plan that recovery reverses. A failure after the terminal sentinel becomes visible has different state and retry implications. Directory-sync behavior also differs by platform, and power-loss durability remains excluded; an error message alone is insufficient evidence that no deployment committed.

`StreamReporter` deliberately discards stdout and stderr write and flush errors (`output/reporter.rs:123–191`). The supplied closed-stdout Windows probe confirms a committed copy deployment can exit `0` with no JSON received by its reader. This observation supports best-effort result delivery in that condition. It does not establish that every stdout failure is handled identically on other platforms, and it does not make the JSON document a transaction receipt whose successful delivery is required for commit.

## Removal, promotion, and history traversal

**Approved removal content:** A non-purge removal leaves a regular file reconstructed from the current repository source. Template sources are rendered with current inputs in the owning module's variable context. It does not promise to preserve the latest externally edited target bytes or reproduce historical output from the prior apply. The supplied Windows probe distinguishes all three byte sets and observes current-source content. Baseline `cmd/remove.rs:180–199` corroborates the rule; older user-guide wording describing last-applied content is inaccurate under the approved decision.

Remove backs up the actual pre-command target and manifest before replacing either. This backup supports recovery of an uncommitted removal; it does not make a committed remove undoable by ordinary rollback. Tests at CLI `record_only_commits.rs:209–269` interrupt removal before target mutation, after target mutation, after manifest mutation, and after commit. Their assertions distinguish recoverable pending removals from completed ownership changes, including purge.

Remove and promote publish records whose remaining targets are marked `Unchanged` and whose reaped list is empty. A promotion updates the chosen target's expected content hash; a removal omits that target. Both also append persistent ownership metadata. These operations do not run apply hooks, deploy newly declared targets, or rewrite unrelated drifted targets. Evidence: `journal/mod.rs:301–384`; CLI `record_only_commits.rs:69–145`.

Rollback passes one removal or promotion record by writing ROLLED_BACK and changing no targets. Later rollback invocations continue through earlier history. Persistent ownership edits newer than the record being reversed protect overlapping targets; a removed target remains unmanaged and a promoted target retains its current bytes, including later user edits. Status still compares a promoted target with the recorded promoted bytes. Applies made after the ownership edit remain reversible normally.

Protection is based on path overlap, not just exact strings. Restoring an ancestor directory or backed-up directory symlink could replace a protected descendant, so rollback skips that whole restore unit. Unrelated entries in that skipped unit may also remain in their current state. Evidence: `journal/mod.rs:528–614`; `rollback/replay.rs:100–180`; `record_only_commits.rs:342–490`.

Each new record stores cumulative ownership edits so retention of only ten operations does not discard still-relevant removal or promotion effects. After all retained records are passed or reversed, promoted expectations may remain while rollback reports no prior apply. Readding a removed target and applying it creates normally reversible newer deployment history; crossing the earlier removal preserves its ownership decision. Tests at `record_only_commits.rs:390–459` and `:472–522` assert these distinctions.

## Rollback fidelity and compensation

Rollback first restores targets the apply removed, then reverses managed entries in reverse apply order. The removed-target set is one compensation group; targets belonging to a managed declaration are another group. `Unchanged` targets are excluded. A changed target with a backup is restored; a created target without one is deleted. Reaped targets without a backup are left alone. The original record indices remain the identifiers for kept copies. Evidence: `rollback/mod.rs:216–233`; `rollback/replay.rs:100–250`.

Before reversing a group, the baseline snapshots its current entries into a staging directory. On a reversal error, it attempts to restore already processed entries, including the failing entry, from those snapshots. Successfully reversed earlier groups are not automatically redone. The existing tests inject an unusable parent for a second target and assert that a first target returns to its pre-rollback bytes; the removed-target test similarly checks that the first restored removal becomes absent again. Those tests cover compensation that succeeds, not a secondary compensation failure (`rollback/replay.rs:691–722`, `:812–835`).

**Approved failure boundary:** If compensation also fails, rollback returns failure, identifies affected paths, preserves usable snapshots and retryable history, and does not claim the complete group was restored. There is no whole-rollback atomicity guarantee. The baseline violates this boundary: `roll_forward` discards restore errors, staging removal is attempted regardless, and error text asserts successful restoration. A subsequent rollback also deletes leftover staging before beginning. Evidence: `rollback/replay.rs:440–476`; `rollback/mod.rs:95–121`, `:236–251`.

Before rollback replaces a live entry that differs from the committed expectation, it saves a recovery copy. The expectation is a recorded link destination or a regular-file hash; for a reaped target, the expected state is absence. An entry already matching its backup is not copied or rewritten. A replaced tree root restores as its backed-up link; its current directory is copied first unless all its files and links are matching recorded leaves. These rules preserve external edits during normal reversal and retries without treating an unchanged entry as part of the rollback.

Published rollback fidelity covers recorded targets' content and entry kind, with mode and timestamp bits excluded, unchanged targets preserved, and later ownership edits protected. Creation or removal of auxiliary parent directories is not separately represented in committed target records. The broad phrase filesystem matches pre-apply state should not be used as evidence of complete directory-tree equality beyond those recorded entries without additional decisions and verification.

## Retention and corrupted history

After a newly committed apply, removal, or promotion, history pruning retains the ten newest committed operation IDs and their backups. This includes applies that only created targets and ownership-only records. It preserves pending plans and their backups. Plans are deleted before terminal sentinels, and backups after successful journal cleanup, so interrupted pruning cannot make a retained old plan appear newly pending. Evidence: `journal/mod.rs:450–486` and `:632–650`.

After a newly committed apply, recovered-copy retention separately keeps the ten newest recovered directories, ordered by operation ID and numeric retry suffix. A full no-op, standalone recovery, or rollback does not trigger this apply-time pruning. This retention limits the duration of kept-copy availability; preserving a recovery copy is not a permanent archive guarantee. Evidence: `backups/retention.rs:42–85`; `apply/engine.rs:2097–2101`; `engine.rs:3122–3156`.

History readers distinguish corruption from interruption. An active COMMIT body that is truncated or undecodable can be skipped in favor of an older readable record. A corrupt closed record fails instead, because it can contain persistent ownership edits still needed by older history. Higher major versions are refused. A terminal file's existence still prevents orphan recovery, even when its body cannot be decoded. Therefore corrupted committed state is not automatically reversed as an interrupted transaction. Evidence: `journal/mod.rs:553–614`; `journal/recovery.rs:206–234`.

The pre-release format policy means a baseline record from an older layout can stop participating in status, ownership, orphan removal, or rollback despite a shared major of `1`. The user guide documents a manual upgrade procedure using the old binary to recover first, retaining desired files, discarding old journal history, then recording state with the new binary. That compatibility policy is separate from process-crash recovery.

## Locks, filesystem retries, and status

Mutating operations use an advisory exclusive lock at the per-machine state directory. The normal exclusive acquisition limit is one minute; `PATINA_LOCK_TIMEOUT_MS`, documented in source as a test override, accepts a nonnegative integer millisecond value and otherwise falls back to the default. Acquisition polls every 25 milliseconds. A conflicting exclusive or shared holder can cause exit `4`; other lock I/O errors are generic failures. The OS releases the lock when the handle or process closes. Evidence: `lock.rs:60–95` and `:209–270`.

Status uses a shared lock with a five-second timeout. On timeout it warns and reads without the lock, accepting a potentially changing view. A pending-plan check rereads under a shared lock when one is available and otherwise reports running-or-interrupted work. Apply preview planning itself is not a single lock-protected snapshot of all sources, targets, and history. The process tests in `lock_concurrency.rs` establish nonoverlapping helper lock windows, timeout results, shared-holder coexistence, and lock release after abort; they do not by themselves establish that two real commands cannot act on differently aged plans.

Windows write helpers retry sharing violations with delays of 50, 100, 200, 400, 800, and 1600 milliseconds, then make a final attempt. Staged renames additionally retry access-denied errors because a scanner can temporarily open the freshly staged entry. Other errors propagate immediately; Linux and macOS use a single attempt. These retries are local filesystem behavior, not retries of whole commands or hooks. Evidence: `apply/retry.rs:1–145`; core `fs_retry.rs`.

Status classifies targets from the latest effective committed record rather than building a fresh desired-content baseline. Current declarations determine whether each recorded target is still managed. A matching recorded target is clean; an absent managed target is missing; a present mismatching target is drifted; a present target no longer managed is orphaned. A removed-and-absent target is omitted. New declarations absent from history and source-only edits are not independently reported as target drift. Evidence: `status/mod.rs:168–239`; `status/classify.rs:57–128`; CLI `status_cli.rs:118–268`.

Target identity uses parent canonicalization with a verbatim leaf, then the shared case and Unicode fold. It avoids following a target symlink into its source when comparing ownership across time. The fold can merge distinct spellings on a case-sensitive filesystem, conservatively leaving an old target rather than reaping a live respelled target. Missing remote tree checkouts make their footprint indeterminate so recorded leaves are protected; status warns instead of fetching that tree to establish its contents.

## Orphan removal and published filesystem limits

Orphan removal compares the prior effective target record with the current managed set. A candidate must still exist, must not be a real directory, and must not be governed by a current target. It excludes current tree roots and recorded descendants under a current tree root that is no longer a real directory. Recorded descendants of a newly managed whole-directory target are also excluded, preventing a reap through that new link into the source. Reasons are `ignored` when a current source leaf is excluded by ignore rules, otherwise `unmanaged`. Evidence: `apply/engine.rs:2538–2616`.

The planned removal set is displayed before consent and serialized into the durable plan. The executor removes only that set, after post-hooks. It backs up a candidate before removal, tolerates an already absent target, and refuses to remove a real directory even if one replaced the planned leaf after planning. Intermediate directories can consequently remain after all their tracked leaves are removed. CLI `symlink_tree_orphans.rs:144–250` asserts removal of an orphaned leaf, preservation of its sibling and directory, and backup before removal after a condition becomes false.

The published limits remain part of applicability rather than evidence that all filesystem races are handled:

- Power loss and kernel panic are outside the v1 guarantee. Backups are not synced before overwrite; Windows does not provide the same journal-directory sync behavior. Process-interruption recovery assumes surviving cached writes and usable retained state.
- A user edit after the one-pass backup and before target writing can be overwritten without appearing in either the backup or a recovery copy.
- An update target deleted before backup can retain newly written bytes after recovery because the recorded update has no backup.
- The interior-symlink check covers directories above current tree leaves. Orphan removal, rollback, and recovery can follow other symlink ancestors planted after deployment, including a removed source leaf beneath a linked intermediate directory.
- Current tree-root links have specific protection through root replacement, backup exclusion, and ancestor-link restoration; those protections do not imply a general rejection of every ancestor link.
- State and repository paths must remain off cloud-sync mounts. The baseline does not detect those paths or provide a warning.

At the inspected revision, these conditions are documented in `AGENTS.md` under Known unknowns and in `ARCHITECTURE.md:284–311` and `:374–440`. The current spec states the supported operating boundary. Extending symlink checks, guaranteeing snapshot freshness against external writers, or adding power-loss durability would change the supported boundary and was not inferred from the existing journal mechanism.

## Sources and evidence boundaries

The main implementation sources are [apply orchestration](../../patina-cli/src/cmd/apply.rs), [the deployment engine](../../patina-core/src/apply/engine.rs), [hook execution](../../patina-core/src/apply/hooks.rs), [journal publication and history](../../patina-core/src/journal/mod.rs), [orphan recovery](../../patina-core/src/journal/recovery.rs), [journaled removal](../../patina-core/src/journal/removal.rs), [backup publication](../../patina-core/src/backups/mirror.rs), [rollback orchestration](../../patina-core/src/rollback/mod.rs), [rollback replay](../../patina-core/src/rollback/replay.rs), [status](../../patina-core/src/status/mod.rs), and [locking](../../patina-core/src/lock.rs).

The consequential inspected tests are [CLI crash recovery](../../patina-cli/tests/apply_crash_recovery.rs), [core recovery decisions](../../patina-core/tests/recovery_crash.rs), [rollback fidelity](../../patina-cli/tests/rollback_fidelity.rs), [ownership-history traversal](../../patina-cli/tests/record_only_commits.rs), [satisfied-apply behavior](../../patina-cli/tests/skip_if_satisfied.rs), [deterministic stdout](../../patina-cli/tests/deterministic_stdout.rs), [journal publication](../../patina-core/tests/journal_fsync_shape.rs), and [lock-process behavior](../../patina-core/tests/lock_concurrency.rs). Several tests construct filesystem states or inject termination after a completed operation; they do not enumerate interruption at every filesystem syscall.

The evidence does not establish successful compensation after arbitrary secondary I/O failures, complete-file interruption behavior for baseline direct repository writes, hook timeout behavior beyond the observed unbounded wait, or full conformance on all supported platforms. Those limits remain distinct from the approved intended contract. No inference in this report turns an implementation defect into a required historical behavior.
