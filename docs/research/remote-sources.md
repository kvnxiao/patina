---
date: 2026-11-14
questions:
  - How do remote declarations, committed pins, and machine-local checkouts determine deployed content?
  - What do the update gate, command failures, locking, and cache pruning guarantee?
  - How do notification consumers distinguish successful observations from failed or stale checks?
  - Which remote-content trust guarantees differ from the inspected implementation?
observed-in:
  - Patina source and tests at 4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc
  - Git 2.56.0.windows.4 in isolated local repositories on Windows
recheck-when: Remote configuration, lock serialization, Git materialization, update orchestration, notification state, or cache retention changes; repeat platform-dependent probes when Git or supported operating systems change.
---

# Remote sources, updates, and notification evidence

Patina separates producing a remote pin from consuming it. The root manifest declares a repository, `patina remote update` selects a revision through the update gate, and `patina.lock` records that revision for every machine. Apply materializes the pinned revision in the machine-local cache and deploys selected paths through the ordinary target consent and journal flow. A cold-cache preview can fetch and materialize content even though it does not modify the repository or deployment targets.

**Observed in the inspected revision:** Accepted pins are saved independently, so a multi-remote update can partially succeed. Notification state records neither a successful-observation timestamp nor an unknown state, and failed checks can erase previously known pending updates. Git checkout disables several machine-local conversions but still processes committed attributes. An isolated probe confirmed that remote attributes can select a locally configured Git smudge command, which transforms content during checkout. Another probe confirmed failure to resolve annotated tags to commit objects.

**Settled requirements confirmed on 2026-11-14:** Independent pin updates remain independent: an accepted pin survives another remote's failure or declined confirmation, and `apply --update` proceeds with the resulting pins. Notifications preserve the last successful observation and accurately distinguish unknown, stale, and mismatching states. Remote content remains inert during materialization and removal, including attributes that could select locally configured commands. The inspected notification and filter behavior therefore identifies implementation gaps. Annotated tags remain supported by the intended branch-or-tag interface; the observed tag failure does not narrow that interface.

This document distinguishes source inspection, assertions found in tests, and executed Git probes. The Patina test suites were read, not executed during this investigation. The Git probes used temporary local repositories and did not exercise a rebuilt Patina executable. General apply transaction behavior, profile resolution, and target mutation rules are covered by their owning research and specifications; their remote-specific consequences are included here.

## Registry and entry selection

**Observed in source:** The root `patina.toml` declares an ordered array of `[[remote]]` tables. Remote inventory reads the root manifest without discovering or selecting modules, so update without a name covers declarations that no active entry uses. The registry fields are:

| Field | Inspected interpretation |
| --- | --- |
| `url` | Required string, trimmed and nonempty; passed to Git rather than parsed as a restricted URL scheme. |
| `name` | Optional string; trimmed when supplied, otherwise derived from the URL's final segment after removing a trailing `.git`. |
| `ref` | Optional string; trimmed, with an empty value treated as omitted. Omission selects `HEAD` upstream. |
| `min_age` | Optional duration overriding the repository default for this remote. |
| `[patina] remote_min_age` | Optional repository default; omission leaves the shipped 72-hour floor. |

Both `url` and `ref` reject a leading `-`, preventing their interpretation as Git options. Duration parsing accepts an ASCII whole number followed by lowercase `s`, `m`, `h`, or `d`, with surrounding whitespace allowed. It rejects signs, fractions, compound units, internal whitespace, missing units, and multiplication or integer overflow. Source: [remote configuration](../../patina-core/src/config/remote.rs), `RemoteSpec::from_raw`, `derive_name`, and `parse_duration`; [root parsing](../../patina-core/src/config/root.rs), `parse_root_config_str`.

A validated name permits ASCII letters, digits, `.`, `_`, and `-`. Empty names, `.` and `..`, trailing dots, DOS device stems, and the reserved names `notice`, `pending`, and `last_check` fail on every platform. DOS device stems include `CON`, `PRN`, `AUX`, `NUL`, `COM0` through `COM9`, and `LPT0` through `LPT9`, including extension-bearing forms. Name equality, ordering, and hashing use the repository's case-and-normalization fold; display retains the authored spelling. Since accepted names are ASCII, the normalization component does not admit Unicode names. Registry duplicates fail under this identity rule. Cache directories use the folded name; lock lookups, entry selectors, and named updates use the same identity.

The URL derivation is a string operation, not URL validation: it trims trailing path separators and takes the suffix after `/`, `\`, or `:`. The comment that a bare host is necessarily underivable is stronger than this implementation; a suffix that itself passes name validation can become a name. Unknown fields are not denied by the inspected raw registry or lockfile deserializers. These parser observations do not independently establish compatibility requirements for misspelled or undocumented keys.

An entry without `remote` resolves relative to its module directory. An entry naming a remote resolves relative to that remote's pinned checkout; the module still supplies its authored mapping, variables, hooks, conditions, and ignore patterns. A blank selector fails parsing. A module-level `remote` value is rejected, including a table or array of tables, because declarations belong to the root. Entry conditions run before remote source resolution, so a remote used only by false conditions is not fetched and does not require a pin for that plan. An active undeclared selector or missing pin fails planning; the missing-pin diagnostic names `patina remote update <name>`. Sources: [module parsing](../../patina-core/src/config/mod.rs), `parse_module_config_str`; [apply planning](../../patina-core/src/apply/engine.rs), `RemoteRegistry` and `gate_and_resolve_entry`.

## Content trust and source boundaries

**Observed in source:** A checkout's `patina.toml` does not contribute configuration. Remote `.tmpl` sources use their ordinary declared file mode and never invoke MiniJinja during apply; local templates retain their normal behavior in the same module. Ignore rules come from the user's manifests, not remote `.gitignore` files. Root ignore patterns precede entry patterns, use case-insensitive matching, and anchor at the selected source directory. Sources: [entry tests](../../patina-core/tests/config_remote.rs), `the_template_policy_is_per_entry_within_one_module`; [ignore rules](../../patina-core/src/ignore_rules.rs); [remote apply tests](../../patina-cli/tests/remote_apply.rs), `a_patina_toml_inside_the_checkout_contributes_nothing`.

The resolver canonicalizes the checkout root and selected source, then rejects a source outside that root. This covers traversal and an escaping source symlink. Git uses `core.symlinks=false`, so symlink blobs normally become regular files with link target text. If another program introduces real symlinks into a cached directory source, the planner rejects deployed symlink leaves. Tree modes check leaves retained by manifest ignore rules; whole-directory symlink mode checks the entire tree. A selected file resolving through a real link to another location inside the checkout satisfies the containment test. Source: [apply engine](../../patina-core/src/apply/engine.rs), `resolve_entry` and `reject_symlink_leaves`.

Remote-backed targets cannot be promoted into the checkout: promote reports the remote name and refuses with exit code 1. A new pin produces a distinct checkout directory, allowing apply to change a symlink and rollback to restore its earlier source. This prevents Patina from changing a published checkout in place; it does not make the cache tamper-proof. An existing revision directory is accepted without comparing its contents with Git objects. Sources: [promote](../../patina-cli/src/cmd/promote.rs), `remote_backing`; [cache](../../patina-core/src/remote/cache.rs), `ensure_checkout`.

**Removal implementation gap:** Non-purge removal reconstructs the local file from the journaled source. In [remove](../../patina-cli/src/cmd/remove.rs), `reconstruct_content` selects MiniJinja solely because that source path ends in `.tmpl`, without checking remote origin or applied mode. A remote-backed file applied literally can therefore render or fail template validation during removal. `--purge` bypasses reconstruction. The confirmed inert-content rule instead requires literal current source bytes for remote removal. This gap was established by inspection, not an executed removal probe.

Remote tree expansion participates in target-collision checks. An upstream-added leaf can make a formerly valid plan fail. Duplicate canonical targets and a whole-directory symlink containing another target fail before target mutation. Tree modes claim materialized leaves, allowing other entries to populate distinct paths below the same directory. Comparisons fold case and normalization on every platform. Cache filling can already have occurred when collision validation fails. Sources: [apply engine](../../patina-core/src/apply/engine.rs), `plan`; [collision validation](../../patina-core/src/apply/collisions.rs).

## Lockfile schema and writes

**Observed in source:** `patina.lock` is TOML beside the root manifest. It requires `version = 1`; missing or different versions fail. The optional `remotes` table maps names to these entries:

| Field | Reader validation and use |
| --- | --- |
| `url` | Required string recording the accepted update's declaration. Consumer fetching uses the current manifest URL. |
| `ref` | Optional string recording the accepted update's declaration. Consumer fetching uses the current manifest ref. |
| `rev` | Required string of exactly 40 hexadecimal characters. Both hex cases parse; validation checks shape, not commit-object identity. |
| `updated_at` | Required string accepted by `jiff::Timestamp`; its epoch supplies the backdating floor. |

An accepted pin records the local update time in `updated_at`, rather than the candidate commit time. The writer generates UTC timestamps, but parsing accepts valid offsets and rendering preserves the stored string rather than normalizing it. Entries serialize in folded-name order with fixed field order and necessary string/key escaping. Absent ref fields are omitted. Fold-equivalent duplicate names fail. An absent file reads as an empty lockfile. Sources: [lockfile](../../patina-core/src/remote/lockfile.rs), `parse`, `render`, `save`; [Git](../../patina-core/src/remote/git.rs), `is_full_sha`.

Apply uses the locked revision with the current declaration's URL/ref. It still validates the whole lockfile when reading it; malformed metadata is not ignored. A changed URL/ref with an unchanged upstream SHA produces `AlreadyPinned`, without updating provenance fields or `updated_at`. Lock loading is lazy within apply planning, but remote inventory always loads it. Local-only apply also attempts stale-pin inspection; a malformed lock can produce a warning but does not fail that apply. Sources: [update](../../patina-core/src/remote/update.rs), `inventory` and `propose`; [apply CLI](../../patina-cli/src/cmd/apply.rs), `stale_pins`.

Saving writes the complete document to a same-directory `<filename>.<pid>.tmp` and renames it over the destination. Process termination leaves an old or new complete destination file. This is file replacement atomicity, not whole-command atomicity or power-loss durability. The helper does not fsync the staged file or use the Windows rename-retry wrapper. Failed writes can leave temporary files. Source: [filesystem helpers](../../patina-core/src/fsx.rs), `write_atomic`.

A write-capable apply removes undeclared pins after planning and before target confirmation, re-reading and saving under the exclusive lock. Declining the target diff does not restore them. Noninteractive apply without `--yes`, and every JSON apply, reports stale pins without rewriting them. Standalone update replaces selected entries but does not run this stale-pin removal. Source: [apply CLI](../../patina-cli/src/cmd/apply.rs), `rewrites_the_lockfile` and `prune_stale_pins`.

## Git dependencies and materialization

**Observed in source:** Patina invokes `git` from `PATH`. Authentication, credential helpers, SSH configuration, proxies, and URL rewrites therefore depend on the installed Git environment. Each subprocess disables terminal credential prompts with `GIT_TERMINAL_PROMPT=0`. Repository-location variables are removed, including `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, and related local-repository variables. `GIT_CONFIG`, `GIT_CONFIG_COUNT`, and `GIT_CONFIG_PARAMETERS` remain available, preserving environment-injected configuration such as CI credential rewrites. This does not establish a timeout for SSH, helpers, or other network processes. Source: [Git subprocess layer](../../patina-core/src/remote/git.rs), environment constants and `git_command`.

Consumer fetching requests the exact revision with depth 1. When that fails for a reason other than process spawning and an explicit tracked ref exists, it shallow-fetches that ref and checks whether the requested revision arrived. An omitted ref has no fallback. A successful exact-SHA fetch does not separately recheck `has_commit` in that branch; materialization still fails if the revision cannot be read. The fallback cannot retrieve an older pin when the server refuses direct SHA requests and a depth-1 current-ref fetch omits it. Warm checkouts avoid these Git calls. Sources: [Git](../../patina-core/src/remote/git.rs), `fetch_commit`; [cache](../../patina-core/src/remote/cache.rs), `ensure_checkout`.

Producer discovery uses `git ls-remote <url> <ref-or-HEAD>`. The selector accepts exact ref forms rather than arbitrary suffix matches. Its preference is a peeled short-name tag, a matching branch, a matching lightweight or unpeeled tag, then the exact requested ref. The annotated-tag probe below exposes a mismatch between this selector and the output actually requested. SHA parsing supports 40-character SHA-1 names; SHA-256 repositories are not established as supported.

For a changed candidate, producer fetching requests the tracked ref without a depth limit, then reads committer time and ancestry. It does not explicitly remove existing limits on fetched commit history with `--unshallow`. The inspected test covers a fast-forward from a shallowly fetched pin, not every interaction with existing shallow history. Discovery and fetch are separate calls: an upstream change between them can cause proposal failure if the discovered object was not fetched. There is no retry loop or Git subprocess deadline. Sources: [update](../../patina-core/src/remote/update.rs), `propose`; [Git](../../patina-core/src/remote/git.rs), `fetch_history`, `committer_time`, `is_ancestor`.

Materialization uses a scratch index with `read-tree` and `checkout-index --all --force`. It sets `core.autocrlf=false`, `core.eol=lf`, `core.safecrlf=false`, `core.attributesFile=`, `core.symlinks=false`, and `GIT_ATTR_NOSYSTEM=1`. The scratch index is removed after success or failure. These settings disable the named machine-local conversions but do not disable committed `.gitattributes`. Attributes can still request line-ending changes or select a locally configured filter. Source: [Git](../../patina-core/src/remote/git.rs), `checkout_commit` and `run_with_index`.

**Settled requirement and implementation gap:** Remote-controlled attributes must not execute local commands during materialization. Raw tree enumeration and blob extraction provide a feasible way to avoid checkout conversion, as the probe below demonstrates. This does not require embedding a Git library, defining an external signature-trust service, fetching submodules recursively, or removing the local Git/network dependency. The earlier guide's deferral of attribute-blind materialization does not justify attribute-triggered command execution under the confirmed contract.

## Checkout cache and pruning

**Observed in source:** Fetched objects use `<state>/remotes/<folded-name>/repo.git`; each published checkout uses `<state>/remotes/<folded-name>/<rev>/`. Metadata files `notice`, `pending`, and `last_check` are siblings of remote directories. The first active entry needing a revision materializes it, memoized by remote identity within the plan. Apply planning and remove's ownership-resolution plan can fill the cache before confirmation; declining does not remove that checkout. Sources: [cache](../../patina-core/src/remote/cache.rs); [apply engine](../../patina-core/src/apply/engine.rs), `RemoteRegistry`.

A new checkout is written to `<rev>.partial.<pid>` and renamed into place. Before starting, the process removes staging with its own PID suffix. Distinct processes use separate staging directories and scratch indexes. If a peer has already published the destination directory, the later publisher discards its own staging tree. The bare fetch repository remains shared; staging does not serialize simultaneous planning-time Git fetches. The inspected code adds no per-remote lock or Git-lock retry for those fetches.

On Windows, publication retries raw OS errors 32 and 5 after 50, 100, 200, 400, 800, and 1600 milliseconds, for six retries after the first attempt. Other errors return immediately. Other platforms attempt the rename once. This approximately 3.15-second budget applies to checkout publication, not all cache operations or lockfile writes. Sources: [cache](../../patina-core/src/remote/cache.rs), `publish_checkout`; [retry helpers](../../patina-core/src/apply/retry.rs), `with_staged_rename_retry`.

Pruning reads source paths from every decodable `.COMMIT` record, preserving older checkouts needed by rollback. Declared remotes retain their current pins and referenced checkouts. If current pins could not be read, their entire caches remain. An undeclared remote loses its entire cache when no journal record references it; otherwise referenced checkouts and its bare repository remain. Any undecodable commit suspends all pruning. Record read errors return errors rather than authorizing deletion from partial knowledge. Reachability checks both supplied and canonical cache paths. Source: [cache](../../patina-core/src/remote/cache.rs), `prune_at`, `referenced_paths`, `is_referenced`.

Within retained remote trees, only recognized revision directories and scratch names are reclaimable. Scratch names need a full-SHA stem and an index or partial suffix. Their modification time must be at least 24 hours old; future or unreadable timestamps do not authorize deletion. The timestamp belongs to the staging root, and descendant writes do not necessarily refresh it. A checkout taking more than 24 hours can lose its staging directory. Whole removal of an undeclared, unreferenced remote happens before this age check and removes recent staging too. Unknown names inside retained remote directories remain.

Automatic pruning runs after a newly committed apply and journal retention. A prune failure is logged and does not undo the apply. The full-no-op execution path returns before this sweep, so the guide's statement that every successful apply sweeps is broader than the inspected implementation. `remote prune` runs the sweep explicitly under the exclusive lock and reports removals. Sources: [apply engine](../../patina-core/src/apply/engine.rs), `execute`; [remote CLI](../../patina-cli/src/cmd/remote.rs), `run_prune`.

## Update gate and confirmation

**Observed in source:** A candidate equal to the recorded revision short-circuits after upstream discovery, without fetching history, rechecking age, or refreshing `updated_at`. Changed candidates use committer time rather than author time. The effective minimum age is the remote override, then `[patina] remote_min_age`, then 72 hours. The checks are:

1. A committer timestamp more than one hour ahead of the local clock is rejected. Exactly one hour ahead is within tolerance.
2. A candidate not descending from the pin records a history-rewrite concern. A missing old object after fetching tracked history is treated as non-ancestry.
3. A committer timestamp earlier than the pin's `updated_at` records a backdating concern. Equal timestamps pass.
4. Unless this is the first pin or `--now` is set, the candidate is held until committer time plus minimum age. Equality with that eligibility time passes.

Cooldown overrides accumulated confirmation concerns; concerns appear when the candidate becomes eligible. A first pin bypasses age, not future rejection. `--now` also bypasses only age and emits a warning. Arithmetic saturates if duration or timestamp arithmetic exceeds the signed comparison range. Source: [gate](../../patina-core/src/remote/gate.rs), `evaluate` and `effective_min_age`.

`remote update --yes` accepts recoverable concerns, not future rejection or cooldown. Without it, noninteractive runs report that confirmation was unavailable and hold the pin; this does not count as a declined prompt. Interactive confirmation accepts only trimmed `y` or `Y`; other input or EOF declines. `apply --update` supplies `yes=false` independently of apply's own `--yes`. Source: [remote CLI](../../patina-cli/src/cmd/remote.rs), `run_update_all`, `settle`, `confirm`.

Committer timestamps are self-reported. Deliberate backdating can satisfy these checks, so neither source nor the confirmed contract establishes unforgeable commit age. The gate does not verify signatures against an external trust authority. Plain apply consumes an existing pin without repeating the gate. Producer confirmation and target-change consent are separate actions.

## Commands, partial success, and public output

**Observed in source:** Remote subcommands accept `--json` before or after the subcommand. JSON does not make standalone update, check, or prune read-only. Apply has a different policy: every JSON apply skips producer updates and lock rewrites.

| Command | Inspected work and output |
| --- | --- |
| `remote list` | Reads declarations, pins, and cached pending state without querying upstream. Human columns are `NAME`, `REF`, `REV`, and `URL`, with an optional pending suffix. |
| `remote check` | Queries declared upstreams with `ls-remote`, compares the dotfiles repository with its upstream, and rewrites notice state without downloading objects. |
| `remote check --hook` | Uses the 24-hour attempt throttle and suppresses ordinary check output; individual observation failures still return success after the loop. |
| `remote update [name]` | Proposes selected candidates, runs the gate, saves accepted pins, and reports outcomes. Omitted name selects every declaration. |
| `remote prune` | Removes entries according to pin and journal reachability, reporting sorted paths. |
| `apply --update` | Runs the producer only when lock rewriting is permitted, then applies the resulting pins. Producer failure warns without independently aborting apply. |

The current JSON surfaces are:

| Command | JSON fields at the inspected revision |
| --- | --- |
| `remote list --json` | `remotes`: rows with `name`, `url`, `ref`, `rev`, `updated_at`, and boolean `pending`. Absent ref and pin fields are null. |
| `remote check --json` | `pending`: name array; `repo_behind`: boolean; `failed`: rendered per-remote error strings. |
| `remote update --json` | `remotes`: rows with `name`, `action`, and nullable `rev`. Actions are `up_to_date`, `updated`, `held`, `declined`, `rejected`, or `failed`. |
| `remote prune --json` | `removed`: sorted path strings. |
| `status --json` | `remotes_pending`: name array alongside apply metadata, file rows, and drift counters. |

Update JSON does not include the human table's previous revision or cooldown eligibility timestamp. Human columns are `NAME`, `FROM`, `TO`, and `STATUS`; unchanged equal revisions render `TO` as `-`. Missing pins render `(unpinned)`; failed proposals without a candidate render `(unknown)`. Warnings and prompts remain separate from JSON. Updating an empty registry currently prints the human empty-registry message even with JSON enabled, whereas list emits an empty JSON array. Sources: [remote CLI](../../patina-cli/src/cmd/remote.rs), `run_list`, `run_check`, `run_update`, `Outcome::row`, `run_prune`; [status CLI](../../patina-cli/src/cmd/status.rs), `json_envelope`.

**Settled behavior supported by source:** Accepted pins are independent. Proposals run in batches of up to eight threads and are processed in declaration order. One unreachable remote does not block settlement of other completed proposals. Each accepted candidate immediately rewrites the whole lockfile. Later declines or failures do not reverse accepted pins. Lockfile I/O failure propagates out of settlement: earlier successful writes remain, but later settlements and the final result table may not occur. A retry identifies previously accepted revisions as already pinned. Sources: [remote CLI](../../patina-cli/src/cmd/remote.rs), `propose_all`, `run_update`, `bump`; [update core](../../patina-core/src/remote/update.rs), `accept`.

Standalone update exits 1 if any reported outcome failed or was future-rejected, otherwise 5 if any prompt was explicitly declined, otherwise 0. Cooldown and noninteractive unasked holds exit 0. An unknown name exits 1 without rewriting the lock. Command-level discovery, parsing, locking, or persistence errors use the ordinary CLI error path. Check exits 1 for per-remote observation failures except in hook mode. Notice-write failures warn in ordinary commands but do not reverse accepted pins or make a completed hook fail.

`apply --update` continues after producer errors and nonzero results, including explicit gate declines, using the resulting on-disk pins. Warm cache can support apply after upstream failure; cold missing revisions can still fail the later plan. The warning about pins already committed is imprecise after partial success because accepted producer writes can be uncommitted. Declining the later target diff does not reverse them. Source: [apply CLI](../../patina-cli/src/cmd/apply.rs), `run_remote_updates`.

## Locking and ordering limits

**Observed in source:** Update acquires the exclusive state lock before inventory reads and retains it through network calls, confirmation, writes, and reporting. Prune also uses the exclusive lock. Its normal wait cap is 60 seconds, with the test-oriented `PATINA_LOCK_TIMEOUT_MS` override; shared acquisition uses five seconds. Git subprocesses have no deadline, so update can retain its exclusive lock indefinitely while Git waits. Sources: [remote CLI](../../patina-cli/src/cmd/remote.rs), `run_update_locked`; [locking](../../patina-core/src/lock.rs).

List and check attempt a shared lock only while reading the manifest and lockfile. Their helper warns and continues without it on any acquisition error; hook mode suppresses the warning. The lock is released before network access, avoiding contention from a slow check. A check can consequently publish an observation from an older pin after another command updates it. Cache population during planning also precedes exclusive apply locking. Atomic file replacement does not serialize these observations or concurrent Git fetches.

**Settled ordering requirement and observed difference:** When recovery of an interrupted apply is authorized, that old transaction is recovered before new work. New machine-variable inputs must then validate before new pin updates, cache population, or deployment. The inspected CLI checks `-v key=value` argument shape in `build_request`, but runs remote updates before `recover_before_planning` and before core profile and variable-layer validation. It therefore does not establish the confirmed ordering. Sources: [apply CLI](../../patina-cli/src/cmd/apply.rs), `run` and `build_request`; [apply engine](../../patina-core/src/apply/engine.rs), `build_planning_context` and `plan`. This cross-area requirement does not narrow preview or authorized recovery guarantees.

## Notification observations and freshness

**Observed in source:** `pending` stores newline-separated names, `notice` stores plain text for shells, and `last_check` stores an attempted check's integer epoch. Unreadable pending state reads as an empty set; an unreadable or empty notice reads as absent. Files are individually replaced atomically, but `publish` writes pending state and notice sequentially. Failure or concurrent publication can leave them inconsistent despite the source comment claiming they cannot disagree. Source: [notice state](../../patina-core/src/remote/notice.rs), `publish`, `read_pending`, `read_notice`.

List reads pending membership for each declaration. Status reports stored pending names without querying upstream. Documented fish, zsh, and PowerShell integrations print the notice and launch background hook checks; these are configuration examples, not an installed service. No notice integration was found in the investigated dotfiles repository's fish, zsh, or PowerShell files. Internally, `settle` recognizes the repository-origin notice by exact English text; that coupling does not establish the prose as a public API.

Hook checks are due when no parseable stamp exists, at least 24 hours have elapsed, or the stamp is in the future. Throttled hooks do not rewrite the stamp. Manual checks also record it, affecting later hooks. Failed observation loops still record an attempt. Concurrent hooks can both pass the due check because neither checking nor publication exclusively claims the interval. The throttle therefore does not provide an exact cross-process once-per-day guarantee. Sources: [notice state](../../patina-core/src/remote/notice.rs), `hook_check_due`; [remote CLI](../../patina-cli/src/cmd/remote.rs), `run_check`.

The check loop builds pending state only from successful unequal comparisons. Failed remotes disappear from the new set, potentially erasing prior knowledge. If every remote fails and no repository mismatch is detected, human output can still claim every remote is pinned, alongside warnings and exit 1. Hook mode suppresses those warnings and returns 0. A successful update removes updated or already-current remotes from pending state; held, declined, rejected, and failed outcomes remain. An older concurrent check can republish a removed entry.

The repository comparison reads the branch's configured upstream remote and branch, falling back to `origin` and the local name when tracking is absent. Detached HEAD returns no difference. It compares local HEAD with the advertised SHA, treating failures as false. Thus `repo_behind` means unequal SHAs, not verified ancestry: locally ahead and diverged branches also receive the behind-origin notice suggesting `git pull && patina apply`. Repository-comparison errors are absent from the check command's per-remote `failed` array. Source: [Git](../../patina-core/src/remote/git.rs), `repo_differs_from_origin` and `try_repo_differs_from_origin`.

**Settled requirement and implementation gap:** Failed checks preserve the last successful observation and identify its freshness honestly. No successful observation means unknown, not confirmed matching. A later failed attempt marks the retained observation stale. Observations remain associated with the remote/ref and compared pin so changed inputs or an outdated concurrent result cannot be presented as knowledge of current inputs. An unequal SHA comparison supports mismatch, not ahead, behind, or divergence without further evidence. Human and machine-readable consumers need these distinctions; storage layout and exact prose remain implementation choices. Observation timestamps let cached output identify when a comparison succeeded without implying continuous network knowledge. Attempt throttling need not represent successful-observation freshness.

## Isolated Git probes

The probes ran on 2026-11-14 with `git version 2.56.0.windows.4`, through Git-for-Windows Bash. Each used local repositories in a temporary directory outside the project. Global and system Git configuration were redirected to `/dev/null`; relevant inherited repository/configuration variables were cleared. Fixture author and committer identities were supplied for local commits. No remote server, user Git configuration, real Patina cache, or deployment target changed. Results establish behavior of these Git invocations, not a rebuilt Patina CLI or other platforms.

### Annotated tag discovery

A local `main` branch had one empty commit and annotated tag `v1`. The commit SHA was `36116fe732c1ad97156a9b49e32f4eacacafc695`; the tag-object SHA was `16bd1649844b4c44ad363afc790d718a51e5afbf`. `git ls-remote <origin> v1` and `git ls-remote <origin> refs/tags/v1` each returned only the tag-object row. A comparison query with `v1*` also returned the peeled `refs/tags/v1^{}` row identifying the commit.

After fetching `v1` into a temporary bare repository, Patina's exact committer-time command, `git --git-dir <cache> show --no-patch --no-show-signature --format=%ct <tag-object-sha>`, printed the tag name, tagger line, annotation, and numeric epoch. That multiline output cannot parse as the integer expected by `committer_time`. Source inspection connects the observed output to a failed annotated-tag proposal under the current invocation. The unit test `ls_remote_peels_an_annotated_tag_to_its_commit` directly supplies a peeled row; it does not check whether the real invocation requests that row. Source: [Git layer and unit tests](../../patina-core/src/remote/git.rs).

### Committed attributes selecting a command

The fixture committed `.gitattributes` with `*.txt filter=probe` and `a.txt` with `source bytes` followed by a newline. A temporary bare repository fetched that branch. Checkout reproduced Patina's `read-tree` and `checkout-index` settings, including disabled external attributes, disabled autocrlf, LF preference, disabled real symlinks, and a separate scratch index. The checkout invocation added local configuration `filter.probe.smudge=touch ../filter-ran && cat`.

The resulting marker established that committed attributes selected and executed the configured smudge command. The command wrote only within the temporary fixture. The remote supplied the filter name, while local Git configuration supplied its command body. That limits the demonstrated attack condition but does not satisfy the confirmed inert-content requirement. Planning-time checkout uses this same command path before target consent.

### Raw blob extraction feasibility

Against that attributed file in the bare repository, a subsequent invocation ran `git --git-dir <cache> -c core.autocrlf=true -c 'filter.probe.smudge=touch raw-filter-ran && cat' cat-file blob FETCH_HEAD:a.txt`. It returned the original bytes and did not create the distinct marker. `git ls-tree -rz --full-tree FETCH_HEAD` enumerated ordinary blobs for `.gitattributes` and `a.txt`. Installed Git help separately identifies `--filters` and `--textconv` as explicit conversion modes for `cat-file`.

This supports raw tree enumeration plus unfiltered blob extraction as a feasible approach. It does not test a complete extractor. An implementation must still validate paths and object kinds, preserve binary bytes, handle operating-system filename restrictions, retain symlink target text as regular-file bytes, and maintain staging and failure behavior. Gitlinks retain the existing nonrecursive scope; the finding does not authorize nested repository downloads. No complete extractor was tested on Windows, macOS, or Linux, and no performance or large-repository bound was measured.

## What the inspected tests establish

These are assertions found in test source, not claims that the tests passed in this investigation:

- [Remote configuration tests](../../patina-core/tests/config_remote.rs) assert declaration parsing, missing or malformed fields, module-level rejection, nonblank selectors, and per-entry template behavior. Configuration and lockfile unit tests cover duration boundaries, name validation, folded identity, unsupported versions, malformed revisions/timestamps, and serialization round trips.
- [Git integration tests](../../patina-core/tests/remote_git.rs) assert nested exact-SHA materialization, warm-cache operation after deleting the origin, missing-ref failure, committer-time extraction, ordinary fast-forward versus orphan ancestry, and differently named tracking branches. They do not establish annotated-tag discovery through the real invocation or arbitrary shallow-history correctness.
- [Update integration tests](../../patina-core/tests/remote_update.rs) assert candidate revisions and outcomes for first pins, cooldown, eligible fast-forwards, rewrites, backdating, future timestamps, bypass, accepted lock metadata, and unreachable origins. Pure gate tests assert exact thresholds and concern ordering.
- [Remote CLI tests](../../patina-cli/tests/remote_cli.rs) assert first-pin creation, unchanged pins, cooldown/bypass output, withheld rewrites without confirmation, declared-but-unused updates, notice creation and ordinary successful-update clearing, hook silence and stamp preservation, stale-pin preview versus mutation, and warm-cache apply after unreachable update. They do not establish retained observations after failed checks or race-free notice publication.
- [Remote apply tests](../../patina-cli/tests/remote_apply.rs) assert deployed content, pinned symlink destinations, inert checkout manifests, traversal rejection, false-condition avoidance, local/remote coexistence, malformed unused-lock tolerance, cold-cache failures, warm-cache operation, autocrlf neutralization, repeat output identity, pin-change rollback, and rejection of a fabricated cached symlink leaf. Autocrlf assertions do not establish inert committed attributes or literal `.tmpl` removal.
- [Cache pruning tests](../../patina-core/tests/remote_cache_prune.rs) assert current-pin retention, older commit references, undeclared-cache removal, folded directory identity, unknown-pin preservation, recognized scratch handling, and suspension on undecodable records. Cache unit tests cover peer-published checkout reuse, recent staging preservation, age-based removal, and Windows publication after a held handle closes. They do not establish all concurrent-fetch behavior or safety beyond the 24-hour staging threshold.

## Applicability limits and sources

The source baseline is commit `4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc`; links identify corresponding modules and tests in this checkout. [The remote-source guide](../REMOTE_SOURCES.md) supplies the documented producer/consumer model, trust claims, commands, and attribute/timestamp limitations. [The agent guide at the inspected revision](https://github.com/kvnxiao/patina/blob/4ee7b39f0f4fbcc0e9279d6eaf7b73702d1aa0fc/AGENTS.md) states that revision's cross-platform, consent, and process-termination expectations. Documented claims were reconciled with inspected source and probes rather than treated as implementation proof.

These observations and confirmed requirements do not introduce recursive fetching of submodules referenced by Git tree entries (gitlinks), an external signature-trust service, or independence from local Git authentication and network behavior. Process-termination atomic file replacement does not establish power-loss durability. The investigation did not exercise hostile concurrent cache modification, servers refusing particular fetch protocols, all filename edge cases, or network timeouts. Confirmed inert-content and notification requirements remain requirements where source differs; these limitations do not retire them.
