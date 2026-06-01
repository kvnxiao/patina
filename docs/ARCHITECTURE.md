# Patina Architecture

This document orients contributors to how Patina is built: the crate
boundaries, the on-disk journal format, the phases an `apply` moves
through, and the recovery primitives that make a mid-apply crash safe.

It cross-links the SPEC-0001 requirements that pin each behaviour so a
contributor can jump from the narrative to the authoritative
`<requirement>` block in `.speccy/specs/0001-patina-core-engine/SPEC.md`.

## Engine layers

Patina is a three-crate Cargo workspace. The `patina-core` /
`patina-cli` split (SPEC-0001 REQ-001) keeps engine logic free of CLI
concerns and lets the engine be tested without spawning a process;
`patina-elevate` is a standalone Windows-only helper added later for the
one-time Developer Mode elevation flow.

```mermaid
flowchart TD
    subgraph cli["patina-cli (bin)"]
        args["clap arg parsing"]
        reporter["output::Reporter\n(human / --json)"]
        exit["exit-code funnel\n(REQ-022)"]
    end
    subgraph core["patina-core (lib)"]
        discover["repo discovery\n(REQ-003)"]
        config["patina.toml model\n(REQ-004)"]
        plan["planner\n(REQ-005)"]
        render["template render\n(REQ-009)"]
        journal["journal + cursor\n(REQ-011, REQ-012)"]
        recover["recovery / rollback\n(REQ-013)"]
        statedir["state directory\n(REQ-016)"]
    end
    args --> discover
    discover --> config
    config --> plan
    plan --> render
    plan --> journal
    journal --> recover
    plan --> reporter
    recover --> exit
    statedir --> journal
```

- **`patina-core`** is the library crate. It owns repository discovery
  (REQ-003), the flat `patina.toml` module model (REQ-004), the five
  file modes (REQ-005), template rendering (REQ-009), path
  canonicalization (REQ-010), the journal and progress cursor
  (REQ-011, REQ-012), crash recovery (REQ-013), backups (REQ-014,
  REQ-015), and the per-machine state directory (REQ-016). It never
  prints user-facing output directly.
- **`patina-cli`** is the binary crate. It parses arguments with
  `clap`, drives the engine, and renders results through the
  `output::Reporter` abstraction (REQ-026) — human-readable by default,
  JSON under `--json`. All process exit codes flow through a single
  funnel that maps engine outcomes onto the formalized codes (REQ-022).
- **`patina-elevate`** is a standalone Windows-only helper binary. It
  carries the smallest possible trust surface — no dependency on
  `patina-core` or `patina-cli` — and exists solely to toggle the
  Developer Mode registry flag under a single UAC prompt. It is gated
  behind a `windows` Cargo feature, so a non-Windows build produces no
  such artifact.

User-facing output never uses `println!` / `eprintln!` outside the
`Reporter` layer; everything else logs through `tracing`. See
AGENTS.md "Hard rules" for the enforcement detail.

## Journal format

Before Patina mutates any file, it writes the entire plan to a journal
in the per-machine state directory and `fsync`s it exactly once
(REQ-011). The journal is the source of truth a later recovery run
reads to converge the filesystem.

The journal is encoded with `postcard`. Because `postcard` makes no
wire-format-stability promise across versions, every journal carries a
version envelope so a future Patina can detect and reject a journal it
cannot decode rather than misread it (see the product north star's
Known-Unknowns note in AGENTS.md).

```mermaid
flowchart LR
    env["version envelope"] --> plan["encoded plan\n(all operations)"]
    plan --> cursor["progress cursor\n(per-op completion)"]
    cursor --> sentinel["terminal sentinel\nCOMMIT / ROLLED_BACK"]
```

- The **version envelope** lets recovery refuse an unknown format.
- The **encoded plan** is the full set of operations, written and
  fsynced upfront in a single durable write (REQ-011).
- The **progress cursor** records per-operation completion as the apply
  proceeds. The cursor is written without a per-operation `fsync`
  (REQ-012) — the upfront plan fsync plus the filesystem-probing
  recovery makes per-op durability unnecessary.
- The **terminal sentinel** records whether the cycle committed or
  rolled back.

`patina debug journal <path>` decodes a journal back into
human-readable form for post-mortem inspection.

## Apply phases

`patina apply` runs three phases in order. The first two are read-only;
only the third touches the filesystem, and it does so only after the
journal is durable.

```mermaid
sequenceDiagram
    participant U as User
    participant P as Plan
    participant D as Diff
    participant M as Mutate
    U->>P: patina apply
    P->>P: resolve repo, config, variables, profile
    P->>D: produce ordered operation list
    D->>U: render diff (REQ-026)
    U-->>D: confirm (TTY) / plan-only (non-TTY)
    D->>M: write + fsync journal (REQ-011)
    M->>M: per-op mutate + cursor (REQ-012)
    M->>U: COMMIT sentinel, exit 0 (REQ-022)
```

1. **Plan.** Resolve the repository (REQ-003), parse `patina.toml`
   (REQ-004), resolve the variable precedence chain (REQ-007) and
   profile (REQ-008), render templates (REQ-009), canonicalize paths
   (REQ-010), and produce an ordered list of operations across the five
   file modes (REQ-005).
2. **Diff.** Compare the planned end-state against the live filesystem
   and present the diff. An interactive TTY prompts for confirmation; a
   non-interactive shell falls through to plan-only and writes nothing.
   Re-applying against unchanged source is a no-op with byte-identical
   stdout (REQ-021).
3. **Mutate.** Write and fsync the journal (REQ-011), take backups
   before any overwrite (REQ-014, REQ-015), apply each operation while
   advancing the progress cursor (REQ-012), and write the terminal
   sentinel. The process exits through the formalized exit-code funnel
   (REQ-022). Mutations and read-only commands coordinate through an
   advisory file lock (REQ-023).

## Recovery

Crash safety is the engine's headline guarantee: a `kill -9` mid-apply
leaves the filesystem in either the pre-apply or post-apply state,
never an intermediate one (REQ-013).

On the next run, recovery reads the journal envelope, then probes the
filesystem to determine how far the interrupted apply got and converges
deterministically:

- If the journal has no terminal sentinel, recovery uses the progress
  cursor (REQ-012) and a filesystem probe to decide, per operation,
  whether to complete the remaining mutations (roll forward) or restore
  from backups (roll back).
- Backups taken before overwrite (REQ-014) are retained for the last
  ten apply cycles; older cycles are garbage-collected on the next
  apply (REQ-015). Backups live in the per-machine state directory and
  never inside the repository.

`patina rollback` reverses the last successful apply by reading the
journal and restoring the recorded pre-apply bytes; afterwards the
filesystem matches the pre-apply state byte-for-byte, modulo files the
user touched outside Patina. `patina status` reports drift between the
declared end-state and the live filesystem. The per-machine state
directory that holds journal, backups, lock, and drift cache uses
OS-appropriate locations (REQ-016) and must not live on a cloud-sync
mount — see `docs/USER_GUIDE.md` "State directory".
