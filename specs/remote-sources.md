---
spec: ../SPEC.md
depends-on: [runtime.md, configuration.md, history.md]
---

# Remote sources and observations

The root repository declares third-party sources and commits selected revisions in `patina.lock`. Updating a pin and deploying that pin are separate actions with separate consent. Machine-local checkouts supply content to the ordinary deployment flow.

## Registry and pinned content

### REQ-remote-registry — Remote registry

The root manifest accepts ordered `[[remote]]` tables with required string `url`, optional string `name`, optional string `ref`, and optional duration `min_age`. Trim URL, name, and ref values. URLs are nonempty Git locations; URLs and refs do not begin with `-`. An empty or omitted ref selects upstream `HEAD`. Without a name, derive it from the final URL segment after removing trailing separators and a trailing `.git`.

Names use ASCII letters, digits, `.`, `_`, and `-`. Reject empty names, `.` and `..`, trailing dots, DOS device stems including extension-bearing forms, and reserved names `notice`, `pending`, and `last_check`. DOS device stems are `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, and `LPT0`–`LPT9`, compared case-insensitively. Name identity is case-insensitive across declarations, selectors, lock entries, and cache directories; duplicate identities fail.

Minimum-age durations use an unsigned ASCII whole number followed by lowercase `s`, `m`, `h`, or `d`, with optional surrounding whitespace. Reject overflow, signs, fractions, compound units, and internal whitespace. Effective age is remote `min_age`, then root `[patina] remote_min_age`, then 72 hours.

**Scenarios:**

- Two declarations use names `Theme` and `theme`; registry validation rejects the duplicate.
- A remote omits its name and uses a URL ending in `/theme.git`; its derived name is `theme`.
- `min_age = " 3d "` is accepted, while `"1d12h"` is rejected.
- A remote name is `CON.txt` on Linux; validation rejects it under the cross-platform naming rule.

### REQ-remote-lockfile — Remote lockfile

`patina.lock` is TOML beside the root manifest, with required `version = 1` and an optional `remotes` table keyed by remote name. Each entry has string `url`, optional string `ref`, exactly 40 hexadecimal characters in `rev`, and a valid timestamp string in `updated_at`. That timestamp records the local wall-clock time when the pin update was accepted, not the candidate's committer time; an unchanged revision preserves it. Reject unsupported versions, malformed fields, and duplicate folded names. A missing file represents an empty lock. Writers use UTC timestamps and stable folded-name order; valid stored timestamps with offsets remain accepted.

Apply consumes the pinned revision through the current manifest URL and ref. Provenance fields in the lock do not silently replace current declarations. An active remote entry with no declaration or no pin fails planning; the missing-pin diagnostic identifies `remote update <name>`. A false-condition entry does not require a pin. Publish lock replacements as complete old or new files under process interruption.

**Scenarios:**

- Upstream advances after a pin is saved; ordinary apply still uses the recorded revision.
- The manifest URL changes while the locked revision remains usable there; apply uses the current URL with that revision.
- An active entry names an unpinned remote; planning fails and identifies the command that creates its pin.
- A process stops during lock replacement; readers see a complete old or complete new lockfile.
- A January 1 commit is pinned on January 10; `updated_at` records January 10. A later candidate dated January 5 triggers the backdating concern against that acceptance time.

### REQ-inert-remote-content — Inert remote content

Materialize pinned Git content without executing commands selected by remote-controlled attributes, hook files, manifests, or template text. Preserve blob bytes without checkout filters or line-ending conversion. Materialize Git symlink blobs as regular files containing their link text. Do not recursively fetch submodules. Local Git authentication and transport configuration remain available; repository-location environment variables do not redirect the operation into an unrelated local repository, and Git terminal credential prompts are disabled.

Resolve selected sources within the pinned checkout and reject escapes through absolute paths, traversal, or source symlinks. Reject real symlink leaves introduced into a remote directory's deployed content; tree modes check included leaves and whole-directory symlinks check the entire source tree. A selected file resolving inside the checkout remains within the containment boundary. Remote-backed content cannot be promoted into that checkout.

**Scenarios:**

- Committed `.gitattributes` selects a locally configured smudge command; cache materialization preserves the blob and does not execute that command.
- A remote file ends in `.tmpl`; both deployment and ordinary removal preserve literal content.
- A remote source path escapes the checkout through `..`; planning rejects it.
- A remote directory contains a real symlink leaf injected into the cache; an included leaf fails validation rather than reaching outside the selected content.

### REQ-remote-cache-lifecycle — Remote cache lifecycle

Store published revision checkouts under `<state>/remotes/<folded-name>/<rev>/`. A complete existing checkout supports offline consumption. A missing checkout can be fetched and materialized during planning, including previews, without deploying targets or changing pins. Publish each new checkout from complete staging; a partial checkout is not accepted as a completed revision. A new revision uses a distinct path so retained history can still refer to the old revision. Patina does not rewrite a published revision in place.

Use installed Git for necessary network and object access. If an exact-commit fetch is refused and a tracked ref is available, a ref fetch can supply that pin; fail if the required revision remains unavailable. Do not substitute a newer revision. An existing cache is not a tamper-proof store, and consumption does not promise an integrity rescan of every published file. Git operations have no specified network deadline.

**Scenarios:**

- A pinned checkout is complete and upstream is unavailable; apply can resolve it from cache.
- A preview needs a cold remote checkout; it can fill the machine cache while leaving repository pins and deployment targets unchanged.
- A server refuses the exact pin and the fetched ref does not include it; planning fails instead of deploying the latest tip.
- Two processes publish the same completed revision; the destination remains one complete checkout, and an unfinished staging directory is not consumed as that revision.

### REQ-remote-cache-pruning — Remote cache pruning

`remote prune` retains current pins and every checkout referenced by retained readable journal records. If pins cannot be read, retain the affected declared caches; an undecodable committed record suspends pruning rather than guessing its references. Remove an undeclared remote's entire cache only when history does not reference it. Within retained remote directories, prune unreferenced revision directories and recognized staging artifacts at least 24 hours old; unknown names and staging with future or unreadable timestamps remain. Whole removal of an undeclared, unreferenced remote is not subject to staging age.

Automatic cache pruning follows a newly committed apply and history retention. A full no-op does not trigger it. Automatic pruning failure warns without reversing the committed deployment. Explicit pruning reports its own failures and sorted removed paths.

**Scenarios:**

- An old revision is no longer pinned but a rollback record refers to it; pruning retains it.
- A committed record cannot be decoded; pruning does not delete caches based on incomplete reachability information.
- A retained remote has a one-hour-old staging directory; pruning does not remove that staging directory.
- An undeclared remote has no journal references; explicit pruning can remove its complete cache directory.

## Update requirements

### REQ-remote-update-gate — Remote update gate

`remote update [name]` resolves the selected branch, lightweight tag, annotated tag, or upstream default to a commit revision. Omitted name selects every declared remote, including unused declarations. An unknown name fails. An unchanged upstream revision is `up_to_date` and does not refresh pin timestamps or provenance. For a changed candidate, evaluate committer time and ancestry against the current pin:

1. Reject a committer timestamp more than one hour in the future; exactly one hour is permitted.
2. Identify a history-rewrite concern when the candidate does not descend from the pin, including when old history is unavailable to establish ancestry.
3. Identify a backdating concern when candidate time is earlier than the pin's `updated_at`; equality passes.
4. Hold an existing pin until candidate time plus effective minimum age. Equality is eligible. A first pin or `--now` bypasses only this age hold; `--now` warns.

An age hold takes precedence over confirmation concerns. Once eligible, a rewrite or backdating concern requires confirmation or standalone `remote update --yes`. Automatic confirmation does not bypass future rejection or the age hold. Plain apply consumes pins without re-running the gate. Self-reported commit timestamps do not establish unforgeable age or signature trust.

**Scenarios:**

- The first candidate is ten minutes old with a 72-hour floor; it can be pinned because first pins bypass age.
- That first candidate is two hours in the future; it is rejected even with `--now --yes`.
- An existing pin has an eligible rewrite candidate; noninteractive update without `--yes` holds it for unavailable confirmation.
- An annotated tag points to a commit; update evaluates and pins the peeled commit, not the tag-object identifier.
- A candidate is exactly at its minimum-age boundary; it is eligible.

### REQ-independent-pin-updates — Independent pin updates

Process update results in declaration order and save each accepted pin independently. A later proposal failure, rejection, hold, or declined prompt does not undo earlier accepted pins. A lockfile write failure stops further settlement and reports failure while preserving earlier completed writes. A retry recognizes already accepted revisions as current.

Standalone update exits `1` when a reported outcome failed or was rejected, otherwise `5` when a prompt was explicitly declined, otherwise `0`. Cooldown and noninteractive unasked confirmation holds are successful reported outcomes. `apply --update` continues with resulting pins after update failures or declines; its own `--yes` does not accept update-gate concerns. Declining the later target diff does not undo accepted pins. Every JSON apply skips producer updates and lockfile rewriting, even with `--yes`.

**Scenarios:**

- Updating three remotes accepts the first, fails the second, and accepts the third; the first and third pins persist and standalone update exits `1`.
- A lock write fails after one accepted pin was saved; that pin remains and later settlements do not run.
- `apply --update --yes` encounters an unconfirmed rewrite; the old pin remains while deployment can proceed using it.
- A user accepts a new pin but declines deployment; the lock retains the accepted pin while targets retain their prior content.
- `apply --json --yes --update` runs; it deploys existing pins without updating them.

### REQ-stale-pin-maintenance — Stale pin maintenance

A write-capable human apply removes lock entries whose remote declarations no longer exist, after successful planning and under the exclusive lock, independently of later target confirmation. Noninteractive apply without `--yes` and every JSON apply only report stale pins. An unusable unused lock can warn without failing a local-only plan; an active remote's malformed lock is a planning failure. Standalone update does not implicitly perform this stale-pin sweep.

**Scenarios:**

- Interactive apply removes an undeclared pin and the user then declines target changes; the pin removal remains.
- JSON apply with `--yes` sees the same stale pin; it reports the condition without rewriting the lock.
- A malformed lock is irrelevant to a local-only deployment; the command warns instead of treating it as a required remote source.

## Observation and output requirements

### REQ-remote-observations — Remote observations

`remote check` queries advertised upstream revisions without downloading objects and records observations for declared remotes and the dotfiles repository's upstream. `remote list` and `status` consume cached observations without querying the network. A successful observation records the compared revision, URL/ref identity, upstream revision, and observation time. Before the first success, the outcome is unknown. A failed refresh preserves the last success and marks it stale; it does not establish matching state or freshness.

Associate results with the inputs they compared. A changed pin, URL, or ref makes an older result stale for current inputs, and an older concurrent completion does not overwrite a newer successful observation as if it were current. Unequal revisions establish `different`; report ahead, behind, or diverged only when ancestry evidence establishes that relationship. Detached or unavailable repository comparison is unknown rather than confirmed current. Observations are advisory snapshots and do not promise continuous network freshness.

**Scenarios:**

- A successful check found a different remote revision and the next check fails; the cached difference remains, marked stale with its last successful time.
- All checks for a remote have failed; list reports unknown rather than no update.
- A slow check started before a pin update finishes afterward; its old-pin result is not presented as a current-pin comparison.
- Repository and upstream SHAs differ without an ancestry check; output says different rather than advising a pull on the assertion that the repository is behind.

### REQ-shell-update-notices — Shell update notices

Publish a plain-text shell notice at `<state>/remotes/notice` derived from available observations, including uncertainty that affects its advice. `remote check --hook` suppresses ordinary output and performs an automatic check when no valid attempt stamp exists, at least 24 hours have elapsed, or the stamp is in the future. Failed attempts can participate in throttling but do not refresh successful-observation time. Concurrent invocations need not provide an exact once-per-day guarantee. Individual observation failures in hook mode do not fail shell startup.

Manual checks are not throttled and also record an attempt. `--hook` takes precedence over `--json` for silent output. Notice publication failure warns where possible without reversing accepted pin updates. Shell integrations read the notice and invoke the hook explicitly; Patina does not install a background service.

**Scenarios:**

- A hook check ran one hour ago; another hook invocation is silent and does not perform the normal refresh.
- A stored attempt time is in the future; a hook check becomes due.
- Hook mode cannot reach one upstream; it remains silent and exits successfully while preserving stale prior observations.
- `remote check --hook --json` runs; the hook's silence policy applies.

### REQ-remote-json — Remote JSON

Remote JSON results use the following command envelopes. Arrays retain declaration order except `removed`, which sorts paths. Diagnostics and prompts go to stderr. JSON does not make standalone check, update, or prune read-only.

| Command | Fields |
| --- | --- |
| `remote list` | `remotes`: rows with `name`, `url`, nullable `ref`, nullable `rev`, nullable `updated_at`, boolean `pending`, and `observation`. |
| `remote check` | `pending`: name array; `observations`: rows with `name` and `observation`; `repository`: repository observation; nullable boolean `repo_behind`; `failed`: rendered observation-error strings. |
| `remote update` | `remotes`: rows with `name`, `action`, and nullable `rev`; action is `up_to_date`, `updated`, `held`, `declined`, `rejected`, or `failed`. |
| `remote prune` | `removed`: sorted path strings. |
| `status` supplemental fields | `remotes_pending`: name array; `remote_observations`: rows with `name` and `observation`. |

An `observation` object has `outcome` (`unknown`, `matching`, or `different`), boolean `stale`, nullable timestamp `observed_at`, nullable `compared_rev`, nullable `upstream_rev`, nullable `url`, and nullable `ref`. Unknown observations have null comparison data and no successful time. A retained stale observation keeps the inputs and result of its last success. `pending` and `remotes_pending` include cached differences for declared remotes, including stale differences whose metadata identifies their uncertainty. They are not assertions that upstream was checked during listing. `repo_behind` is null when ancestry is unestablished; it is true or false only when sufficient evidence establishes the relation.

Ordinary check exits `1` for observation failures and `0` otherwise; hook mode follows its silence and exit policy. Empty remote registries still produce command-shaped JSON, including `{"remotes":[]}` for update.

**Scenarios:**

- An empty registry is updated with `--json --yes`; stdout is one object with an empty `remotes` array, without an English empty-state message.
- Check finds unequal repository SHAs but has no ancestry evidence; `repository.outcome` is `different` and `repo_behind` is null.
- A stale difference appears in `remotes_pending`; the corresponding observation exposes its compared pin and successful timestamp.

## Supporting evidence

[Remote-source research](../docs/research/remote-sources.md) records the registry and lock schema, producer/consumer split, exact update boundaries, notification defects, attribute-execution probe, and annotated-tag probe. Raw extraction feasibility was demonstrated with local Git on Windows; a complete cross-platform extractor and every server-fetch policy were not tested.
