---
spec: ../SPEC.md
depends-on: [runtime.md, configuration.md, history.md, remote-sources.md, deployment.md]
---

# Diagnostics and host integration

Diagnostics inspect configuration and machine state. Remediation is an explicit operation. Windows Defender management has its own ownership ledger and consent; ordinary dotfile deployment does not change antivirus exclusions.

## Environment diagnosis

### REQ-environment-diagnosis — Environment diagnosis

`patina doctor` reports known environment and configuration problems without remediating them. It uses the shared-read behavior in REQ-command-coordination. A successful diagnostic session exits 0 for information and warning findings; inability to perform the command is a generic failure.

| Finding code | Condition and meaning |
| --- | --- |
| `DOC-WIN-UNC` | The selected Windows repository uses a UNC path; report the symlink compatibility concern. |
| `DOC-WIN-DEVMODE` | Windows Developer Mode is disabled and the repository declares symbolic links. |
| `DOC-WIN-OSOLD` | The running Windows build predates Windows 10 1703's Developer Mode symlink support. |
| `DOC-NO-DEFAULT-REPO` | The machine has no usable default-repository pointer. A repository found through this invocation's environment or working directory does not remove the finding. |
| `DOC-NO-GIT` | The repository declares remote sources and Git is unavailable on `PATH`. |
| `DOC-IGNORED-DEPLOYED` | Previously deployed targets remain on disk but current ignore rules exclude them; a later apply will preview their removal. |

Human findings go to stderr. A clean session reports that no findings were found. JSON output has a `findings` array whose objects have `code`, `level`, and `message`, plus `path` when a path is relevant. Levels are `info`, `warning`, or `error`; the current finding set uses information and warnings. Findings have stable ordering.

Diagnosis is limited to these checks. It does not detect cloud-sync storage or certify filesystem durability. Best-effort discovery that cannot establish a repository-dependent finding does not prove the environment is healthy; command errors and unavailable evidence remain distinguishable from successful checks.

**Scenarios:**

- A repository resolves through `PATINA_REPO` but no default is recorded: report `DOC-NO-DEFAULT-REPO` and explain how to record the existing repository.
- A remote-backed repository needs an uncached pin and Git is unavailable: report `DOC-NO-GIT` rather than changing its declarations.
- A new ignore rule excludes a previously deployed leaf: warn without removing that leaf.
- The shared lock times out: warn and continue diagnostics without recovering pending deployment work.

### REQ-diagnostic-remediation — Diagnostic remediation

`doctor --fix` offers each supported remediation individually under exclusive coordination. It can record the current working directory as the default repository after validating its root marker, and on Windows it can enable Developer Mode through the elevation helper. Other findings remain reported without automatic changes.

Interactive confirmation accepts trimmed `y` or `Y`; other answers skip that remediation. `--yes` accepts remediation prompts, while the operating system still controls UAC. A noninteractive invocation without `--yes` exits 1 before remediation. A skipped fix is distinct from a failed fix.

In JSON mode, the command emits one result with `findings` and `remediations`. Each remediation has `code` and `result`, with `result` equal to `applied`, `skipped`, `unsupported`, or `failed`; a failed result includes a diagnostic `message`. Diagnostics outside that result remain on stderr. An empty finding set produces empty arrays.

**Failure behavior:** A remediation failure exits 1 and identifies the failed action. Earlier completed remediations remain in effect. A skipped optional fix does not by itself fail the session. Doctor does not infer that an elevation succeeded merely because its helper launched.

**Scenarios:**

- Confirm recording an existing root as default: publish its canonical path without replacing its manifest.
- Request default-repository repair from a directory without a valid root: exit 1 and do not publish that directory as default.
- Skip Developer Mode remediation: leave the setting unchanged and report the skip.
- Run `doctor --fix --yes --json` with no findings: emit one JSON document with empty `findings` and `remediations`, without a human-only success line on stdout.

## Journal inspection

### REQ-journal-inspection — Journal inspection

`patina debug journal <path>` decodes the named journal artifact into human-readable stdout without discovering or mutating a repository. A path ending in `.COMMIT` is interpreted as a committed record; other paths are interpreted as plans. A committed view identifies recorded targets and removed targets. A plan view identifies intended operations and their dispositions.

**Failure behavior:** A missing or unreadable file, corrupt body, or unsupported major version exits 1 and identifies the path. A major-version mismatch identifies the file's version and the supported version. Successful decoding exits 0. Failed decoding does not change the journal or trigger recovery.

**Scenarios:**

- Inspect a valid commit with removed targets: include those targets in the decoded view.
- Inspect a plan from a higher major version: identify both versions and refuse decoding.
- Inspect a corrupt file while an unrelated apply is pending: report the decode failure without recovering that apply.

## Windows Defender exclusions

### REQ-defender-desired-exclusions — Defender desired exclusions

On Windows, Defender management derives desired path exclusions from the selected deployment: one folder exclusion for the repository root and one exclusion for each declared target destination. Directory modes contribute the declared target directory once, not one exclusion per leaf. File modes contribute file exclusions. Equivalent paths collapse by case-insensitive identity, unified separators, and trailing-separator normalization.

Only exact drive-absolute paths are eligible. Reject wildcard paths, UNC paths, drive roots, and paths within `SystemRoot`, `ProgramFiles`, `ProgramW6432`, or `ProgramFiles(x86)`. Invalid derived candidates are omitted with a diagnostic. The elevated helper independently validates every requested path before changing settings; validation in the unprivileged caller is insufficient.

Preparing an apply or status report can perform the ordinary validated planning and cache preparation needed to resolve declarations. It does not deploy those declarations. `defender clear` uses state and its ledger without requiring a valid or present repository.

**Scenarios:**

- A tree entry deploys forty leaves: its desired exclusion is its one target directory.
- Two declarations use equivalent path spellings: report one exclusion.
- A declared target is under a protected system directory: omit it with a diagnostic instead of excluding that directory.
- The repository has been deleted but the ledger has owned exclusions: clear can still preview their removal.

### REQ-defender-observation — Defender observation

`defender status` compares desired exclusions with the live Defender list when that list is readable. If Defender withholds the list, use the ownership ledger as limited evidence and identify that limitation. Do not request elevation solely to report status.

| State | Evidence |
| --- | --- |
| `owned` | Live list has the exclusion and the ledger records ownership. |
| `unmanaged` | Live list has the exclusion but the ledger does not record ownership. |
| `absent` | Readable live list does not have the desired exclusion. |
| `recorded` | Live list is withheld and the ledger records the exclusion. |
| `unrecorded` | Live list is withheld and the ledger does not record the exclusion. |

Normal JSON status has `repo_root`, `current_readable`, `exclusions`, and `stale`. Each desired exclusion has `path`, `kind` (`file` or `folder`), and `state`. Stale owned exclusions use `path` and `kind`. When the live query itself fails, report a warning and a desired-only view with `current_readable: false`; omit unsupported state claims rather than inventing them. That degraded JSON view has `repo_root` and `exclusions` with `path` and `kind`.

**Scenarios:**

- The live list is withheld and an owned exclusion was manually deleted: report ledger-derived `recorded`, without claiming the live exclusion was verified.
- An elevated status can read that same deletion: report `absent`.
- The live query fails: report desired paths with a warning and without an exclusion-state verdict.

### REQ-defender-ownership-consent — Defender ownership consent

`defender apply` previews additions, stale owned removals, and adoption of already-present desired exclusions. Every Defender or ledger ownership change requires confirmation or `--yes`, including adoption when the live exclusion list itself needs no change. JSON without `--yes` is always a preview. Human noninteractive execution without `--yes` previews without changing ownership or settings.

`defender clear` previews and removes Patina-owned exclusions only. An external exclusion remains externally owned until an apply explicitly adopts it with consent. Successful reconciliation records the selected desired ownership set. When settings are already correct but ownership changes, report that ownership change instead of claiming nothing changed.

No ordinary `patina apply` invocation implicitly opens Defender exclusions. Defender changes are not part of deployment rollback. The Defender ledger is machine-local state, not a synchronized list of settings to force onto other machines.

**Scenarios:**

- All desired exclusions already exist but are unowned; run human noninteractive Defender apply without `--yes`: preview adoption without writing the ledger.
- Confirm that adoption: record ownership; a later clear can remove those now-owned exclusions.
- An unrelated user exclusion is outside the desired and owned sets: apply and clear preserve it.
- Run ordinary dotfile apply with `--yes`: Defender settings and ownership are unchanged.

### REQ-defender-verified-change — Defender verified change

After consent, an exclusion change uses one elevated helper request. The helper applies additions and removals, rereads Defender state, and reports whether the requested result was verified. Only a verified applied result establishes successful setting changes for the ownership ledger. An adoption-only operation with readable live evidence does not require an unnecessary elevated write.

| Result | Exit | Meaning |
| --- | --- | --- |
| `previewed` | `0` | Reported proposed changes without consent to execute. |
| `up_to_date` | `0` | Settings and ownership already satisfy the requested result. |
| `applied` | `0` | Requested setting changes were verified, or consented ownership-only changes completed. |
| `declined` | `5` | Required command confirmation or UAC was declined. |
| `blocked` | `1` | The helper observed that Defender rejected the requested setting result. |
| `failed` | `1` | Applying the request failed for another identified reason. |
| `unconfirmed` | `1` | A reliable helper result was unavailable; the settings might have changed. |

JSON apply and clear results have `repo_root` (null for clear), `current_readable`, `to_add`, `to_remove`, `to_adopt`, `result`, and `detail`. Each proposed exclusion has `path` and `kind`. `detail` reports available failure information and is empty otherwise. Generic preparation or ledger-write failures follow the shared error contract.

**Failure behavior:** Missing confirmation of the helper result does not justify claiming Defender rejected the write. Preserve that uncertainty and permit an idempotent recheck or reconciliation. A ledger-write failure after a verified external change reports that partial completion; it does not falsely report that Defender was restored.

**Scenarios:**

- Defender accepts an API call but the elevated reread shows the intended exclusion absent: report `blocked`, exit 1, and do not claim successful ownership reconciliation.
- The helper result is lost after a possible setting change: report `unconfirmed`; a subsequent apply can reconcile the actual state.
- Setting changes are verified but the ledger cannot be saved: report the ledger failure and the possible need to reconcile ownership.
- Consent is declined: exit 5 without issuing the requested elevated setting change.

## Supporting evidence

See [command and platform research](../docs/research/commands-and-platform.md). Diagnosis, native elevation, unreadable Defender state, and verified helper results have different evidence boundaries; a source-level test of a classifier does not establish successful privileged changes on a managed Windows machine.
