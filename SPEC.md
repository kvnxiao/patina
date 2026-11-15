---
status: approved
areas:
  - specs/runtime.md
  - specs/configuration.md
  - specs/history.md
  - specs/remote-sources.md
  - specs/deployment.md
  - specs/repository-management.md
  - specs/diagnostics.md
---

# Patina specification

Patina deploys a centralized dotfiles repository to a user's machine. Declarative manifests select configuration for that machine, show the proposed changes, and materialize files as symbolic links, rendered templates, or copies. The specification defines the intended v1 contract. Approval establishes that contract; it does not establish that the current implementation satisfies it.

## Purpose and users

Patina serves developers setting up a new machine, maintainers changing existing configuration, and users sharing one repository across macOS, Linux, and Windows. Interactive users need understandable previews and explicit consent for deployment. Automation needs predictable noninteractive behavior and structured results. Users need to inspect drift, recover interrupted deployments, and reverse retained deployment history without silently destroying later edits.

The repository is the source of desired configuration. Per-machine state records what Patina applied, recovery material, the selected repository and profile, machine variables, and cached remote content. Git synchronizes repository content and remote pins between machines; Patina does not synchronize machine state.

## Functional requirements overview

The area files define the authoritative requirement blocks. This overview introduces their responsibilities without adding obligations.

| Area | Capabilities and when to read it |
| --- | --- |
| [Runtime](specs/runtime.md) | Command surface, output, exit codes, host state, coordination, and Windows privilege support. Read before implementing a command. |
| [Configuration](specs/configuration.md) | Repository discovery, manifests, variables, profiles, paths, templates, ignores, and target ownership. Read before resolving a deployment. |
| [History](specs/history.md) | Committed expectations, backups, interrupted work, rollback, retention, and drift classification. Read before changing persistent deployment state. |
| [Remote sources](specs/remote-sources.md) | Remote declarations, pins, inert content, cache lifecycle, update gates, and observations. Read before using third-party content. |
| [Deployment](specs/deployment.md) | Apply previews, consent, materialization, hooks, orphan removal, and command outcomes. Read before applying configuration. |
| [Repository management](specs/repository-management.md) | Initialization, adopting files, removing declarations, and promoting edits. Read before writing repository content or changing ownership. |
| [Diagnostics](specs/diagnostics.md) | Environment diagnosis, journal inspection, and Windows Defender exclusion management. Read before inspecting or changing host integration. |

## Reliability and compatibility overview

Deployment and removal recovery address process termination with the operating system's page cache intact. They converge through a subsequent authorized recovery operation. They do not promise that every target immediately has its final pre-command or post-command state at the instant a process dies.

Rollback preserves recoverable user edits and respects later ownership changes. Individual repository-file replacements preserve complete old or new files, while repository-management commands can partially complete across multiple steps. Output delivery and housekeeping are best effort after a committed operation; a successful exit does not prove that a consumer received every output byte.

All three operating systems are supported. Platform-specific paths and privilege mechanisms are explicit contracts. No throughput, latency, repository-size, or availability target is established by this specification.

## Non-goals

- Power-loss or kernel-panic durability, cloud-synchronized machine state, and automatic detection of cloud-sync paths.
- A background deployment service, drift notifications driven by filesystem monitoring, or a graphical interface.
- Configuration merging such as `merge-json` or `merge-toml`, an embedded scripting language, and native encryption.
- Nested manifest modules beyond the root and one module level, `on_change` or `on_drift` hooks, a `gc` command, or a global `--repo` flag.
- Automatic migration from other dotfile managers, cross-machine state synchronization, machine inventory, and dashboards.
- A JSON schema-version field, arbitrary prerelease journal-layout migration, or a guarantee that every external Git server supports fetching an arbitrary commit directly.
- Whole-command transactions for initialization, adoption, promotion, or rollback. Their narrower failure guarantees are specified in their owning areas.

## Constraints and operating assumptions

Patina is a Rust command-line application with a separate Windows elevation helper. It uses TOML configuration and MiniJinja-compatible templates and expressions. Git is an external dependency for remote operations that need repository access; a complete warm cache supports deployment without contacting upstream.

The dotfiles repository and machine state use local filesystems outside cloud-sync-managed paths, including locally stored OneDrive, Dropbox, iCloud, and Syncthing folders. Patina does not detect those paths. Recovery assumes available recovery material and filesystem operations that can eventually succeed. The caller provides UTF-8 paths and configuration text. Native filesystem path separators are permitted in reported absolute paths.

The local repository, local hooks, and the user selecting that repository are trusted. The caller coordinates external edits to sources and targets during mutations. Patina's command locks coordinate participating Patina operations; they do not lock editors, arbitrary hook processes, or other programs out of the filesystem. The detailed limits of ancestor-symlink handling are part of the path and recovery contracts; Patina does not promise universal confinement against a hostile process changing the filesystem.

Prerelease on-disk layouts can change without migration. The supported layout and major-version rejection behavior are defined in the history area. Build-toolchain migration is separate from the product contract; package manifests declare the build compatibility baseline.

## Shared terms and responsibilities

| Term | Meaning |
| --- | --- |
| Repository | The selected directory with a root-marked `patina.toml`, its immediate modules, and optional committed `patina.lock`. |
| Module | An immediate repository subdirectory with its own `patina.toml`. |
| Entry | One file or directory declaration, optionally selecting several targets or a remote source. |
| Source | Local repository content or content from a pinned remote checkout. |
| Target | The filesystem location at which Patina materializes an entry or one tree leaf. The location is distinct from the destination of a symlink already occupying it. |
| Machine state | Patina's per-user, per-machine state directory, referred to as `<state>`. |
| Plan | The resolved intended operations and their preview classifications. Computing a plan does not authorize executing it. |
| Committed operation | A completed operation whose persistent record establishes the resulting managed expectations. |
| Pending operation | Journaled work without a completed commit or completed recovery. |
| Recovery | Reverting pending work before a command begins its own authorized mutation. |
| Rollback | Explicitly reversing retained committed deployment history. |
| Recorded expectations | The target kinds, source associations, and applied content or link identities recorded by the latest usable committed history. |
| Drift | A live target differing from its recorded expectation; it does not mean that an unapplied source change exists. |
| Remote pin | The repository's selected immutable commit for a named third-party source. |

The configuration resolver owns desired entries. Deployment owns target materialization. History owns recorded expectations and recovery material. Remote management owns pins and remote observations. Repository-management commands own explicit repository edits. Host integration owns privileged setting changes and its ownership ledger.

## Invariants

### REQ-repository-authority — Repository authority

Repository content and explicit machine configuration determine desired deployment. Recorded expectations describe past application and do not silently rewrite desired source content. Repository writes are limited to the command-defined initialization, adoption, removal, promotion, and pin-maintenance behavior.

**Scenarios:**

- A copy target is edited externally; status reports its recorded-state difference without writing that edit into the repository.
- A remote pin changes in `patina.lock`; an ordinary apply deploys that pin without substituting the current upstream tip.
- An apply without update behavior renders a local template; the repository template remains unchanged.

### REQ-repeatable-output — Repeatable output

The same command and effective inputs produce byte-identical stdout. Stable presentation order does not depend on hash iteration, process identifiers, random identifiers, or the current invocation time. Stored timestamps that are part of the requested data, such as an observation time or last-apply metadata, are permitted; unrelated run metadata is excluded.

**Scenarios:**

- After deployment converges, two unchanged `apply --json --yes` invocations emit identical stdout.
- Repeating a remote listing without changing pins or cached observations emits identical stdout.
- Repeating status against the same recorded history and target state reports the same stored last-apply metadata rather than a new current timestamp.

### REQ-remote-content-trust — Remote content trust

Third-party repositories supply content, not executable configuration. Their manifests, templates, attributes, and stored hook files do not authorize command execution or extend the local repository's declared deployment. Local declarations and their consent rules remain authoritative.

**Scenarios:**

- A remote source has `patina.toml` with additional target declarations; only the local manifest's selected content is deployed.
- A remote `.tmpl` file has template syntax; materialization preserves it as content instead of evaluating it.
- A remote attribute selects an installed smudge command; preparing the remote cache does not run that command.

### REQ-reported-state-evidence — Reported state evidence

Reports distinguish observed state from cached, inferred, or unavailable state when that distinction changes the action a user can safely take. Failed observation does not establish that a remote is current, that a host setting is absent, or that an incomplete restoration succeeded.

**Scenarios:**

- A remote check fails after an earlier successful observation; the earlier observation remains identified as stale.
- Defender withholds its live exclusion list; the report identifies ledger-derived state instead of claiming live verification.
- Rollback cannot restore a target or compensate that failure; it reports incomplete restoration and preserves the remaining recovery material.

## Conformance and supporting evidence

A conforming implementation satisfies every requirement and invariant in this spec and its area files, the stated constraints and non-goals, the exact names and formats in external contracts, and the transitions in each state or flow diagram that lists every transition. Present-tense statements in these parts are mandatory. Uppercase SHOULD marks a recommendation an implementation may decline with a documented reason; uppercase MAY marks an allowed option. Text labeled Example or Note, background, requirements overviews linking authoritative blocks, supporting evidence, other diagrams, linked research, and public decisions are informative.

The [research index](docs/research/README.md) links repository evidence, bounded observations, accepted contract choices, and investigation limits. The research distinguishes intended behavior from observed discrepancies. Passing structural checks or existing implementation tests does not establish full conformance to this specification.
