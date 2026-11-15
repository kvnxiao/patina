---
spec: ../SPEC.md
depends-on: []
---

# Runtime and command interfaces

This area defines the shared command interface, machine-state location, coordination, and privilege support. Individual command areas define their result schemas and effects.

## Functional requirements

### REQ-command-surface — Command surface

The `patina` executable accepts the following commands and options. `--help` describes the applicable command, and `--version` reports the application version. The global `--color auto|always|never` option is accepted before or after a subcommand and defaults to `auto`.

| Command | Arguments and options |
| --- | --- |
| `init` | Optional path; `--json`, `--yes`. |
| `add` | Path; `--module <name>`; one of `--symlink`, `--copy`, `--template`, `--symlink-tree`; `--force`, `--json`, `--yes`. |
| `remove` | Path; `--purge`, `--json`, `--yes`. |
| `promote` | Target; `--json`, `--yes`. |
| `apply` | `--yes`, `--force-deploy`, `--update`, `--json`, repeated `-v key=value`. |
| `status` | `--json`, repeated `-v key=value`. |
| `rollback` | `--yes`, `--json`. |
| `doctor` | `--fix`, `--yes`, `--json`. |
| `remote list` | `--json`. |
| `remote check` | `--hook`, `--json`. |
| `remote update` | Optional remote name; `--now`, `--yes`, `--json`. |
| `remote prune` | `--json`. |
| `defender status` | Windows only; `--json`. |
| `defender apply`, `defender clear` | Windows only; `--yes`, `--json`. |
| `debug journal` | Journal-file path. |

The remote `--json` option works before or after its nested verb. The debug namespace is omitted from the top-level help summary and does not accept `--json`. There is no `help` subcommand. Unknown commands, unknown options, missing required arguments, and conflicting `add` mode flags are usage errors.

**Failure behavior:** Usage errors exit 2 and emit a diagnostic without executing a command. Readable help and version requests exit 0 without repository discovery or target mutation.

**Scenarios:**

- Run `patina --color never remote list --json`: the parser accepts both options in their documented positions.
- Run `patina add file --copy --template`: parsing exits 2 before adopting the file.
- On Linux, request `patina defender --help`: the command is unavailable; Windows help exposes it.
- Request `patina --help` with no configured repository: help succeeds without initializing state.

### REQ-output-streams — Output streams

Commands write results and previews to stdout and prompts, warnings, and generic engine errors to stderr. A command-specific JSON result or typed refusal is one JSON document followed by a newline. Diagnostics emitted alongside it remain on stderr. Parse errors and generic engine failures are not required to become JSON envelopes merely because `--json` was supplied.

JSON field names and enumerated values in the owning command contract are compatibility requirements. Object-key order and insignificant JSON whitespace are permitted choices, provided output satisfies REQ-repeatable-output. Human wording is not a byte-for-byte compatibility contract unless a requirement explicitly names it.

Color does not change parsed JSON values or the meaning of command outcomes. `auto` uses terminal and `NO_COLOR` behavior; `always` requests ANSI styling and `never` suppresses it. Structured output names data that human formatting conveys through color.

**Scenarios:**

- A JSON status command reports a pending operation: stdout remains one JSON document and the warning appears on stderr.
- A malformed manifest causes an engine error during `apply --json`: the command exits 1 and identifies the cause on stderr; a success envelope is not emitted.
- Redirect human output with automatic color: ANSI styling is suppressed; `--color always` explicitly requests it.
- Inspect Defender JSON with color disabled: exclusion kind remains an explicit data field.

### REQ-command-exit-codes — Command exit codes

Commands report their terminal outcome with the following process exit codes. An exit code reports the command outcome under its area contract; it is not a blanket assertion that every possible side effect was reversed.

| Code | Outcome |
| --- | --- |
| `0` | Successful operation, successful preview, or completion of a command whose contract tolerates reported per-item problems. |
| `1` | Configuration, filesystem, journal, environment, or other generic failure. |
| `2` | Invalid CLI usage, or a required pre-apply hook failure. |
| `3` | A required post-apply hook failure whose target operations were reversed. |
| `4` | Exclusive command-lock acquisition timed out. |
| `5` | A command's required confirmation or elevation was declined. |

Shared-lock timeout warnings do not by themselves change a read-only command's successful exit. Declining one optional doctor remediation skips that remediation; it does not make the diagnostic session a declined command. Remote commands define how per-remote failures and confirmations determine the aggregate result.

**Scenarios:**

- A required pre-apply hook cannot run: apply exits 2 and performs no new deployment operations.
- A post-apply hook fails and reversal succeeds: apply exits 3; a reversal failure instead reports its incomplete state as a generic failure.
- A mutation cannot acquire the exclusive lock within its allowed wait: it exits 4 without performing that mutation.
- The user skips an optional doctor fix and the session otherwise completes: it exits 0.

### REQ-output-delivery-failure — Output delivery failure

Failure to write or flush application result output does not interrupt an already authorized operation, reverse committed work, or replace the operation's normal exit result. Output can be partial or unavailable. Diagnostics are best effort when their output stream also fails.

After a committed deployment or removal, failures in history cleanup, recovered-copy cleanup, or remote-cache cleanup are reported as warnings where output is available. They do not turn the committed operation into a failed deployment. Publication of the operation's required persistent result is part of the operation, not housekeeping.

**Scenarios:**

- Stdout is closed before `apply --yes --json`; valid authorized deployment still completes, remains committed, and exits 0.
- A required hook fails while stderr is unavailable: the hook policy still determines reversal and exit status.
- A deployment commits but cannot prune an old recovered-copy directory: the deployment remains successful and the directory remains available for later cleanup.
- Promotion cannot write its required history record after replacing a source: it reports partial command failure under the repository-management contract, rather than treating the missing record as optional output.

## State and coordination

### REQ-machine-state-location — Machine state location

Patina resolves one machine-state directory for the user from the following nonempty environment settings. It does not place its deployment journal or target backups inside the dotfiles repository.

| Host | State directory |
| --- | --- |
| Linux | `$XDG_STATE_HOME/patina` when set; otherwise `$HOME/.local/state/patina`. |
| macOS | `$HOME/Library/Application Support/patina`. |
| Windows | `%LOCALAPPDATA%/patina`. |

State is shared across selected repositories on that machine. The directory provides `journal/`, `backups/`, `recovered/`, `remotes/`, `default_repo`, `profile`, `variables.toml`, and `lock` for the responsibilities that use them. Individual artifacts need not exist before first use. Recovery-copy and remote-cache details belong to their areas.

Resolving state can create required directories. Read-only command descriptions mean that the command does not deploy, recover, edit repository content, or change host settings; they do not prohibit coordination files or permitted cache and observation updates explicitly described by the command.

**Failure behavior:** A missing required environment setting, non-UTF-8 path, or failure to create or access required state returns a diagnostic and exit 1. Patina does not detect or silently relocate state from cloud-sync storage.

**Scenarios:**

- Linux has both `XDG_STATE_HOME` and `HOME`: state uses the XDG location.
- Windows lacks a usable `LOCALAPPDATA`: a state-dependent command fails instead of selecting the repository as state storage.
- A first status invocation needs a lock file: creating its machine-state coordination artifacts does not authorize target recovery.
- Two repositories are selected in separate invocations: both read the same machine-variable file unless the state-location environment changes.

### REQ-command-coordination — Command coordination

Deployment execution, recovery, rollback, initialization, adoption, removal, promotion, doctor remediation, remote update settlement, and remote pruning coordinate conflicting state changes through exclusive machine-state locking. The default exclusive wait is 60 seconds. `PATINA_LOCK_TIMEOUT_MS`, when set to a parseable nonnegative integer, selects that wait in milliseconds; absent or invalid values use the default.

Status and read-only doctor attempt a shared lock for five seconds. If that wait expires, they warn and continue without claiming a consistent locked snapshot. Concurrent shared readers are allowed. Process termination releases held locks without requiring deletion of a stale ownership record.

Remote inspection and checking use their remote-specific synchronization rules. Defender integration and external programs do not acquire a deployment lock merely by reading or modifying the same filesystem; the diagnostics area defines its effects without promising participation in deployment transactions.

**Failure behavior:** Exclusive timeout exits 4 before the protected operation. Failure to open or operate a lock for another reason exits 1. An unlocked read after shared timeout can report an operation as running or interrupted without selecting one explanation as fact.

**Scenarios:**

- One apply holds the exclusive lock; another waits and either proceeds after release or exits 4 when its wait expires.
- Status cannot obtain a shared lock within five seconds: it warns and reports the available state without recovering it.
- A process dies while holding a lock: a later command can acquire the lock after the operating system releases the process's handle.
- `PATINA_LOCK_TIMEOUT_MS=invalid` is supplied: the default wait remains in effect.

## Platform support

### REQ-windows-symlink-privilege — Windows symlink privilege

Before creating Windows symbolic links, Patina establishes that the process can create them through Developer Mode or sufficient existing privileges. A deployment that needs Developer Mode can invoke the separate elevation helper after deployment consent. The helper requests one UAC elevation and enables Developer Mode; the ordinary deployment process does not require persistent elevation.

Previewing an apply does not request elevation. A command that needs no new symbolic links does not require Developer Mode merely to report state. Doctor can explicitly offer the same remediation. Enabling Developer Mode is a host-setting change that persists beyond the deployment and is not reverted by deployment rollback.

**Failure behavior:** Declined required deployment elevation exits 5 and identifies the remediation path. A missing helper, failed launch, or completed helper that did not enable the setting is an error. The engine refuses a symbolic-link operation that still lacks privilege rather than substituting a copy silently.

**Scenarios:**

- Preview a symbolic-link deployment while Developer Mode is disabled: report the plan without launching UAC.
- Confirm deployment and accept the helper's elevation: enable Developer Mode, verify the result, then continue deployment.
- Decline required deployment elevation: exit 5 without creating the requested symbolic links.
- A copy-only deployment runs without symlink privilege: it does not request that privilege.

## Supporting evidence

See [command and platform research](../docs/research/commands-and-platform.md) and [transaction research](../docs/research/transactions-and-recovery.md). Windows behavior beyond the explicitly recorded local observations requires native-platform conformance evidence; source inspection does not establish that every elevation path succeeds.
