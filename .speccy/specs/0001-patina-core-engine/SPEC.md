---
id: SPEC-0001
slug: patina-core-engine
title: Patina core engine — transactional apply with apply/status/rollback CLI
status: in-progress
created: 2026-05-25
supersedes: []
---

# SPEC-0001: Patina core engine — transactional apply with apply/status/rollback CLI

## Summary

Patina is a cross-platform dotfile manager whose source of truth is a user's
centralized git repository. A user runs `patina apply` and configurations
appear at the right targets on the machine — as symbolic links pointing back
into the repository, as rendered template output, or as byte copies where a
link is not appropriate.

This SPEC defines the **engine** that performs that materialization, plus a
three-command CLI surface (`patina apply`, `patina status`, `patina rollback`)
sufficient to exercise the engine end-to-end in integration tests. Two
additional SPECs follow: SPEC-0002 layers the complete user-facing CLI
(`init`, `add`, `remove`, `promote`, `doctor`) plus the Windows symlink
elevation flow; SPEC-0003 layers the watch subsystem (filesystem event
loop, per-OS service install, drift detection).

The engine guarantees that a mid-apply crash leaves the filesystem in
either the pre-apply state or the post-apply state and never an intermediate
state. It achieves this with a single-fsync postcard journal written upfront
plus a per-operation progress cursor, recovered by filesystem probing rather
than journal replay. Pre-existing user files are backed up before overwrite
to a per-machine state directory; the backup directory retains the last ten
apply cycles and never enters the dotfiles repository.

The engine is built as a `tokio`-based async library (`patina-core`) plus an
async CLI binary (`patina-cli`). Errors in the library use `thiserror`; the
CLI chains them with `anyhow`. Neither crate is permitted to call `unwrap`,
`expect`, or `panic` in production code paths.

## Goals

<goals>
- A user clones a Patina-shaped dotfile repository onto a fresh macOS,
  Linux, or Windows machine, runs `patina apply`, and the configurations
  declared in `patina.toml` files materialize at the right targets without
  any admin or sudo prompt during the apply itself.
- A mid-apply crash (segfault, power loss, `kill -9`) leaves the
  filesystem in either the pre-apply state or the post-apply state.
  Re-running `patina apply` converges deterministically without
  intermediate corruption.
- A user runs `patina apply` interactively, sees a diff of pending
  changes, and answers `y` or `n` at a prompt. The same command in a
  non-interactive shell displays the diff and exits without mutating
  anything; passing `--yes` skips the prompt regardless of TTY.
- A user runs `patina status` and sees every managed file classified as
  CLEAN, DRIFTED, MISSING, or ORPHANED relative to the dotfiles repo.
- A user runs `patina rollback` and the filesystem returns to the state
  immediately before the last successful apply.
- A user reading two consecutive `patina apply` runs on an unchanged
  source tree sees identical stdout output, with no wall-clock timestamps
  in the human-readable view.
</goals>

## Non-goals

<non-goals>
- No `patina init`, `add`, `remove`, `promote`, or `doctor` commands.
  Those land in SPEC-0002.
- No Windows symlink permission flow (Developer Mode detection or UAC
  helper). Symlink mode on Windows that fails without Developer Mode
  surfaces as a typed error from `patina-core` in this SPEC; the
  prompt-and-elevate flow lands in SPEC-0002.
- No `patina watch` subcommands, per-OS service install, filesystem
  event subscription, debounce, drift detection, drift notification, or
  Windows `ERROR_SHARING_VIOLATION` retry policy. Those land in
  SPEC-0003.
- No merge-mode file types (`merge-json`, `merge-toml`, etc.). The five
  modes in this SPEC are the v1.0 set; merge modes are explicitly
  deferred beyond v1.0.
- No nested modules. A repo has exactly two levels: the root
  `patina.toml` and the per-module `patina.toml` files in immediate
  subdirectories.
- No `on_change` or `on_drift` hook events. The only events in v1.0 are
  `pre_apply` and `post_apply`.
- No JSON schema version field on `--json` output. Versioning is
  deferred until output evolution forces it.
- No `patina gc` command. Backup retention is automatic by count.
- No `--repo <path>` global flag. Repository discovery uses environment
  variable, working-directory walk-up, and a persisted default only.
- No GUI, no migrations from other dotfile managers, no embedded
  scripting language, no native encryption, no cross-machine state sync,
  no machine inventory, no dashboards.
</non-goals>

## User Stories

<user-stories>
- As a developer setting up a fresh laptop, I want to clone my dotfiles
  repository, run a single command, and have my shell, editor, and git
  configurations land at the right paths so my environment matches my
  other machines immediately.
- As a cautious user, I want the default `patina apply` to show me a
  diff and prompt before mutating anything so I never accidentally
  overwrite a file I edited outside of Patina.
- As a CI script author, I want `patina apply` in a non-interactive
  shell to display the plan and exit without mutating so my pipeline
  can preview a deployment safely.
- As a user whose `patina apply` was interrupted by a system reboot, I
  want the next invocation to detect the partial work and converge to
  a clean state without my intervention.
- As a user who edited a copy-mode file in place and wants to revert,
  I want `patina rollback` to restore the file to its pre-last-apply
  content using the local backup.
- As a contributor debugging a failed apply, I want
  `patina debug journal <path>` to decode a binary journal file into a
  human-readable form so I can see exactly which operations the engine
  planned and which it executed before the failure.
</user-stories>

## Assumptions

<assumptions>
- The `postcard` serialization crate's wire format is stable across
  the lifetime of v1.0; the journal embeds a version envelope so the
  decoder can refuse incompatible records explicitly rather than
  silently misinterpret them.
- `MiniJinja`'s strict-undefined behavior, including the Jinja2
  inheritance that an undefined value in an `{% else %}` block
  silently renders as empty string, is acceptable for v1.0. If a
  future requirement demands hard-fail in all contexts, custom
  undefined behavior or a post-render lint pass can be added without
  changing the SPEC shape.
- `fs2`'s advisory file lock papers over the semantic differences
  between POSIX `flock(2)` and Windows `LockFileEx` adequately for
  coordinating a single Patina CLI process. The watcher-CLI
  coordination story (SPEC-0003) depends on the same lock behaving
  consistently across platforms.
- Tokio's file I/O layer is implemented with `spawn_blocking` over a
  thread pool on every supported platform in v1.0. The engine accepts
  this rather than waiting for native async file I/O; future migration
  to `io_uring` or equivalent does not require SPEC changes.
- Users instructed not to place the per-machine state directory on
  iCloud Drive, OneDrive, Dropbox, Box, Google Drive, or Syncthing
  will follow the instruction or accept the consequences. SPEC-0002
  adds active doctor warnings; SPEC-0001 documents only.
- A user's home directory and the per-machine state directory share
  the same filesystem in the common case. Cross-filesystem state
  directories work but may produce additional fsync overhead and are
  not optimized for in v1.0.
- The `notify` crate, referenced for the watcher in SPEC-0003,
  handles cross-platform filesystem event quirks sufficiently. This
  SPEC does not depend on `notify` directly but the engine's journal
  format must remain readable by SPEC-0003 watcher code.
- `winsafe`'s coverage of `taskschd`, `shell`, and `advapi` features,
  referenced by SPEC-0002 and SPEC-0003 for Windows surfaces, is
  adequate. SPEC-0001 does not link `winsafe` itself but its decisions
  about path canonicalization and state directory layout must remain
  consistent with the Windows code paths in later SPECs.
</assumptions>

## Requirements

<requirement id="REQ-001">
### REQ-001: Cargo workspace with `patina-core` library and `patina-cli` binary

The repository root contains a Cargo workspace declaring two members:
`patina-core` (library crate, error type via `thiserror`) and
`patina-cli` (binary crate, application error chaining via `anyhow`).
Both crates declare `edition = "2024"`, set `rust-version` to `1.95` or
higher, and carry an MIT license declaration.

<done-when>
- `cargo metadata --format-version 1` lists both `patina-core` and
  `patina-cli` as workspace members.
- `cargo build --workspace` succeeds on a stock Rust toolchain matching
  the declared `rust-version`.
- `patina-core/Cargo.toml` declares `thiserror` as a direct dependency
  and does not declare `anyhow`.
- `patina-cli/Cargo.toml` declares `anyhow` and `patina-core` as direct
  dependencies.
- Both `Cargo.toml` files contain `edition = "2024"`, `rust-version`
  matching the workspace MSRV, and `license = "MIT"`.
</done-when>

<behavior>
- Given a clean checkout, when `cargo build --workspace` is run with the
  declared minimum Rust toolchain, then the build succeeds without
  warnings.
- Given a checkout, when `cargo metadata` is queried, then both members
  appear with the configured edition, MSRV, and license.
</behavior>

<scenario id="CHK-001">
Given the repository at HEAD after this SPEC lands,
when `cargo metadata --format-version 1 --no-deps` runs from the workspace root,
then the JSON output's `packages` array contains entries whose `name`
fields are `patina-core` and `patina-cli`, each with
`"edition": "2024"` and `"license": "MIT"`.
</scenario>

<scenario id="CHK-002">
Given the repository at HEAD,
when `cargo build --workspace --locked` runs with the MSRV toolchain,
then the build exits 0.
</scenario>
</requirement>

<requirement id="REQ-002">
### REQ-002: `patina-core` is an async library using tokio

`patina-core` exposes async functions for the apply, status, and
rollback pipelines and uses `tokio` as its async runtime adapter. The
public entry points return typed `Result` types built from
`thiserror`-generated error enums.

<done-when>
- `patina-core`'s public apply, status, and rollback entry points are
  `async fn` returning typed `Result`.
- `patina-core/Cargo.toml` declares `tokio` as a direct dependency
  with the features `rt-multi-thread`, `fs`, `process`, `signal`,
  `sync`, `time`, `io-util`, and `macros`.
- `patina-cli`'s `main` is annotated with `#[tokio::main]` and calls
  the library's async entry points with `.await`.
- Synchronous code in `patina-core` invoking the library's async APIs
  must construct a tokio runtime explicitly; there is no sync
  facade.
</done-when>

<behavior>
- Given a checkout at HEAD, when `cargo check -p patina-core` runs,
  then it compiles and the library's public entry points are
  `async fn`.
- Given a checkout at HEAD, when `cargo check -p patina-cli` runs,
  then `patina-cli/src/main.rs` contains `#[tokio::main]` on the
  binary's entry point.
</behavior>

<scenario id="CHK-003">
Given the repository at HEAD after this SPEC lands,
when `cargo check --workspace --locked` runs,
then the command exits 0 and `patina-core/src/lib.rs` declares
`pub async fn apply(...)`, `pub async fn status(...)`, and
`pub async fn rollback(...)`.
</scenario>

<scenario id="CHK-004">
Given the repository at HEAD,
when `patina-cli/src/main.rs` is inspected,
then the `main` function carries the `#[tokio::main]` attribute and
the binary uses `.await` to invoke `patina-core`'s public
entry points.
</scenario>
</requirement>

<requirement id="REQ-003">
### REQ-003: Repository discovery uses env var, walk-up, persisted default — no `--repo` flag

The engine resolves the dotfiles repository path in a fixed priority
order: the `PATINA_REPO` environment variable (if set), then a walk
upward from the current working directory looking for a `patina.toml`
declaring `root = true`, then a persisted default path stored in the
per-machine state directory. No `--repo` global flag exists on any
command.

<done-when>
- `patina apply` with `PATINA_REPO` set to a valid repository directory
  uses that directory regardless of the current working directory.
- `patina apply` with `PATINA_REPO` unset, run from a subdirectory of
  a valid repository, walks upward, finds the root `patina.toml`, and
  uses the containing directory as the repository root.
- `patina apply` with `PATINA_REPO` unset, run from a directory whose
  ancestors contain no root `patina.toml`, falls back to the persisted
  default path if present, otherwise emits a typed error naming the
  three sources tried and exits with code 1.
- `clap`'s derived parser does not contain a `--repo` argument on any
  subcommand.
</done-when>

<behavior>
- Given `PATINA_REPO=/tmp/dotfiles` and a CWD outside any Patina
  repository, when `patina apply` runs, then the engine reads
  `/tmp/dotfiles/patina.toml`.
- Given no `PATINA_REPO`, a CWD nested four directories deep inside a
  Patina repository, and no persisted default, when `patina apply` runs,
  then the engine discovers the repository by walking up four levels.
- Given no `PATINA_REPO`, a CWD outside any repository, and no
  persisted default, when `patina apply` runs, then the engine exits
  with code 1 and stderr names the three discovery sources that were
  tried.
</behavior>

<scenario id="CHK-005">
Given a temporary directory `T` containing `patina.toml` with
`root = true`,
when `PATINA_REPO=T patina apply --yes` runs from an unrelated CWD,
then the engine resolves `T` as the repository root and the apply
proceeds.
</scenario>

<scenario id="CHK-006">
Given a temporary directory `T` with `patina.toml` at its root and a
CWD at `T/zsh/`,
when `patina apply --yes` runs with `PATINA_REPO` unset,
then the engine walks up from `T/zsh/`, finds `T/patina.toml`, and
applies.
</scenario>

<scenario id="CHK-007">
Given a CWD outside any Patina repository, no `PATINA_REPO`, and no
persisted default in the state directory,
when `patina apply` runs,
then exit code is 1 and stderr contains the substrings
`PATINA_REPO`, `walk-up`, and `persisted default`.
</scenario>
</requirement>

<requirement id="REQ-004">
### REQ-004: Flat module structure — root `patina.toml` plus immediate-subdirectory modules

The engine recognizes exactly two depths of `patina.toml` files: the
root file that declares `root = true` in its `[patina]` table, and
per-module files in immediate subdirectories of the root that omit the
`root` key. A `patina.toml` file located at a depth greater than one
below the root or a non-root `patina.toml` that declares `root = true`
is a configuration error.

<done-when>
- A repository with root `patina.toml` plus module files at
  `zsh/patina.toml`, `nvim/patina.toml`, `git/patina.toml` discovers
  three modules.
- A `patina.toml` at `zsh/plugins/patina.toml` is rejected at discovery
  with a typed error naming the offending path and the maximum allowed
  depth.
- A `patina.toml` at `zsh/patina.toml` containing `root = true` is
  rejected at discovery with a typed error naming the file and the
  expected absence of `root`.
- A root `patina.toml` missing `root = true` is rejected at discovery
  with a typed error naming the file and the missing key.
</done-when>

<behavior>
- Given a repository with root `patina.toml` and a `nvim/patina.toml`
  module, when discovery runs, then the engine returns a module list
  containing exactly the `nvim` module.
- Given a repository whose tree contains a nested
  `zsh/plugins/patina.toml`, when discovery runs, then the engine
  returns a typed error and does not enumerate any modules.
</behavior>

<scenario id="CHK-008">
Given a temporary repository `T` with files
`T/patina.toml` (containing `[patina]\nroot = true`),
`T/zsh/patina.toml`, and `T/nvim/patina.toml`,
when the engine discovers modules in `T`,
then the result is a module set of exactly `{zsh, nvim}`.
</scenario>

<scenario id="CHK-009">
Given a temporary repository `T` with
`T/patina.toml` (root) and `T/zsh/plugins/patina.toml`,
when the engine discovers modules in `T`,
then discovery fails with a typed error whose Display contains
`zsh/plugins/patina.toml` and the phrase `maximum module depth`.
</scenario>
</requirement>

<requirement id="REQ-005">
### REQ-005: Five file modes — per-file symlink, atomic directory symlink, byte copy, copy-tree, template render; single or multi-target fan-out

Each `[[file]]` entry in a module's `patina.toml` declares a mode that
controls how the source path materializes at one or more target paths.
Each entry must declare **exactly one** of `target` (a single absolute
or HOME-relative path string) or `targets` (a non-empty array of such
strings); declaring both, neither, or `targets = []` is a parse error.
When `targets` is used, the entry materializes the source at every
listed target path according to the declared mode; internally the
engine treats a `target = "x"` entry as equivalent to
`targets = ["x"]` and records one journal operation per
(source, target_i) pair, so the same crash recovery, status, backup,
and rollback machinery applies per-target without special-casing. The
five v1.0 modes are:

- per-file `symlink`: target is a symbolic link pointing to the source
  file in the repository; if the source is a directory, the engine
  walks the directory and creates one symlink per file at the
  mirrored target path.
- `symlink-dir`: the source is a directory and the target is a single
  atomic symbolic link pointing at that directory; the engine does
  not walk into the source.
- `copy`: the target is a byte copy of the source file; subsequent
  changes to the source do not propagate without re-running apply.
- `copy-tree`: the source is a directory; the engine recursively
  copies the tree to the target path.
- implicit template render: a source file with a `.tmpl` suffix is
  rendered through MiniJinja into the target path (without the
  `.tmpl` suffix); the materialized target is a regular file, not a
  symlink.

<done-when>
- A `[[file]]` entry with `mode = "symlink"` and a file source
  materializes the target as a symbolic link whose target equals the
  canonical absolute source path.
- A `[[file]]` entry with `mode = "symlink"` and a directory source
  produces one symbolic link per file under the source tree at the
  mirrored target paths.
- A `[[file]]` entry with `mode = "symlink-dir"` materializes the
  target as a single symbolic link pointing at the source directory.
- A `[[file]]` entry with `mode = "copy"` materializes the target as
  a regular file whose byte content equals the source.
- A `[[file]]` entry with `mode = "copy-tree"` materializes the
  target as a directory tree of regular files mirroring the source.
- A source path with a `.tmpl` suffix declared in any file-mode entry
  is rejected at validation; templating is implicit, not explicit.
- A source file with `.tmpl` suffix, regardless of declared mode,
  renders through MiniJinja and materializes at the target path with
  the `.tmpl` suffix stripped, as a regular file.
- A `[[file]]` entry whose `mode` is none of the five recognized
  values is rejected at TOML parse with a typed error naming the
  offending value and the allowed set.
- A `[[file]]` entry that omits the `mode` field defaults to
  `mode = "symlink"`. Per the symlink mode's behavior, a directory
  source under the default falls back to the per-file walk
  (one symlink per file at mirrored targets), not an atomic
  directory symlink. Users wanting atomic directory symlinks must
  declare `mode = "symlink-dir"` explicitly.
- A `[[file]]` entry declaring both `target` and `targets` is
  rejected at parse with a typed error naming both keys and the
  XOR rule.
- A `[[file]]` entry declaring neither `target` nor `targets` is
  rejected at parse with a typed error naming the missing field
  pair.
- A `[[file]]` entry declaring `targets = []` (empty array) is
  rejected at parse with a typed error naming `targets` and the
  non-empty constraint.
- A `[[file]]` entry with `targets = [t1, t2, ..., tN]` and any of
  the five modes materializes the source at every `t_i` according
  to the mode: symlink fans out to N symbolic links pointing at the
  same canonical source path, symlink-dir to N directory symlinks,
  copy to N byte copies, copy-tree to N trees, and a `.tmpl` source
  to N rendered files (rendered once with the resolved variable
  context and written to each target with the `.tmpl` suffix
  stripped).
- A multi-target entry's targets are restored atomically per
  REQ-013 and REQ-019: if any target's write fails, the entire
  entry's set of targets is reverted as a unit.
</done-when>

<behavior>
- Given a module declaring `source = "zshrc"`, `target = "~/.zshrc"`,
  `mode = "symlink"`, when apply runs, then `~/.zshrc` is a symlink
  whose target is the canonical path of the repository's `zsh/zshrc`.
- Given a module declaring `source = "config"`,
  `target = "~/.config/nvim"`, `mode = "symlink-dir"`, when apply
  runs, then `~/.config/nvim` is a single symlink, not a tree of
  per-file symlinks.
- Given a module declaring `source = "gitconfig.tmpl"`,
  `target = "~/.gitconfig"`, with no explicit `mode` (or any mode),
  when apply runs, then `~/.gitconfig` is a regular file containing
  the MiniJinja-rendered output and the `.tmpl` suffix is stripped.
- Given a module declaring `mode = "merge-json"`, when the TOML
  parser runs, then parsing fails with a typed error naming
  `merge-json` and listing the five accepted modes.
- Given a module declaring `source = "agent.toml"`,
  `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`,
  `mode = "symlink"`, when apply runs, then both
  `~/.claude/agent.toml` and `~/.codex/agent.toml` exist as
  symbolic links whose readlink targets equal the canonical path
  of the repository's `<module>/agent.toml`.
- Given the same module with `mode = "copy"`, when apply runs,
  then both target paths are regular files whose byte content
  equals the source.
- Given a module declaring `source = "agent.toml.tmpl"` and
  `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`,
  when apply runs, then both target paths (with the `.tmpl`
  suffix stripped) are regular files containing the same
  MiniJinja-rendered output.
- Given a `[[file]]` entry declaring both `target = "x"` and
  `targets = ["y"]`, when the TOML parser runs, then parse fails
  with a typed error naming both keys and the substring
  `exactly one`.
- Given a `[[file]]` entry declaring `targets = []`, when the
  TOML parser runs, then parse fails with a typed error naming
  `targets` and the substring `non-empty`.
</behavior>

<scenario id="CHK-010">
Given a tempdir repository with `T/patina.toml`, `T/zsh/patina.toml`
declaring `[[file]] source = "zshrc" target = "~/.zshrc" mode = "symlink"`,
and `T/zsh/zshrc` with arbitrary content,
when `patina apply --yes` runs against a HOME pointing at a tempdir,
then `$HOME/.zshrc` is a symbolic link whose readlink target equals
the canonical path of `T/zsh/zshrc`.
</scenario>

<scenario id="CHK-011">
Given a tempdir repository declaring a `[[file]]` with
`source = "gitconfig.tmpl"` and content
`[user]\n    email = {{ patina.profile_email }}`,
with `patina.profile_email = "kevin@example.com"` resolved through the
variable chain,
when `patina apply --yes` runs,
then `$HOME/.gitconfig` is a regular file whose content is
`[user]\n    email = kevin@example.com` and `$HOME/.gitconfig.tmpl`
does not exist.
</scenario>

<scenario id="CHK-012">
Given a tempdir repository declaring a `[[file]]` with
`mode = "merge-json"`,
when the engine attempts to parse the module's `patina.toml`,
then parse fails with a typed error whose Display contains
`merge-json` and the substrings `symlink`, `symlink-dir`, `copy`, and
`copy-tree`.
</scenario>

<scenario id="CHK-041">
Given a tempdir repository declaring a `[[file]]` with
`source = "zshrc"` and `target = "~/.zshrc"` (no `mode` field),
when `patina apply --yes` runs,
then `$HOME/.zshrc` is a symbolic link whose readlink target equals
the canonical path of the repository's `zsh/zshrc` (default mode is
`symlink`).
</scenario>

<scenario id="CHK-042">
Given a tempdir repository with a module declaring
`[[file]] source = "agent.toml" targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"] mode = "symlink"`,
and `<module>/agent.toml` with arbitrary content,
when `patina apply --yes` runs against a HOME pointing at a tempdir,
then both `$HOME/.claude/agent.toml` and `$HOME/.codex/agent.toml`
are symbolic links whose readlink targets equal the canonical path
of `<module>/agent.toml`.
</scenario>

<scenario id="CHK-043">
Given the same fixture as CHK-042 but with `mode = "copy"`,
when `patina apply --yes` runs,
then both `$HOME/.claude/agent.toml` and `$HOME/.codex/agent.toml`
are regular files whose byte content equals the source file.
</scenario>

<scenario id="CHK-044">
Given a tempdir repository with a module declaring
`[[file]] source = "agent.toml.tmpl" targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`
and `<module>/agent.toml.tmpl` containing
`name = {{ patina.user }}`,
when `patina apply --yes` runs,
then both `$HOME/.claude/agent.toml` and `$HOME/.codex/agent.toml`
are regular files containing the same MiniJinja-rendered output
(`name = <resolved patina.user>`), and neither
`$HOME/.claude/agent.toml.tmpl` nor `$HOME/.codex/agent.toml.tmpl`
exists.
</scenario>

<scenario id="CHK-045">
Given a `[[file]]` entry declaring both
`target = "~/.claude/agent.toml"` and
`targets = ["~/.codex/agent.toml"]`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display names both
`target` and `targets` and contains the substring `exactly one`.
</scenario>

<scenario id="CHK-046">
Given a `[[file]]` entry declaring neither `target` nor `targets`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains the
substrings `target`, `targets`, and `missing`.
</scenario>

<scenario id="CHK-047">
Given a `[[file]]` entry declaring `targets = []`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains
`targets` and the substring `non-empty`.
</scenario>
</requirement>

<requirement id="REQ-006">
### REQ-006: `[[hook]]` schema with `pre_apply` and `post_apply` events; `must_succeed` defaults true

Each `[[hook]]` entry in a `patina.toml` declares a command keyed by an
event from the set `{"pre_apply", "post_apply"}`. The schema fields are
`event`, `command`, `shell` (optional, default platform-appropriate),
`when` (optional MiniJinja predicate), and `must_succeed` (optional
boolean, default `true`). No other event values are accepted in v1.0;
`on_change` and `on_drift` are explicit non-goals.

<done-when>
- A `[[hook]]` entry with `event = "pre_apply"` parses successfully.
- A `[[hook]]` entry with `event = "post_apply"` parses successfully.
- A `[[hook]]` entry with `event = "on_change"` or any value other
  than the two accepted ones is rejected at parse with a typed error
  naming the offending value and the accepted set.
- A `[[hook]]` entry omitting `must_succeed` is treated as
  `must_succeed = true`.
- A `[[hook]]` entry's `when` field, if present, is evaluated against
  the same MiniJinja context as `*.tmpl` rendering and must produce a
  boolean value or the hook is rejected.
- A `[[hook]]` entry's `shell` field, if present, must name an
  executable resolvable on the current `PATH`; absence falls back to
  `bash` on macOS and Linux and `pwsh` on Windows.
</done-when>

<behavior>
- Given a hook declaring `event = "pre_apply"` and
  `command = "brew bundle"`, when apply runs on macOS with
  `must_succeed = false`, then the hook executes; if it returns
  non-zero, the apply continues with a warning.
- Given a hook declaring `event = "on_change"`, when the parser
  loads the file, then parsing fails before any hook runs.
- Given a hook declaring `when = "patina.os == 'macos'"` and the
  resolved variable context contains `patina.os = "linux"`, when the
  apply pipeline encounters the hook, then the hook is skipped.
</behavior>

<scenario id="CHK-013">
Given a tempdir repository whose module declares
`[[hook]] event = "on_change" command = "echo hi"`,
when the engine parses the module's `patina.toml`,
then parsing fails with a typed error whose Display contains
`on_change` and the substrings `pre_apply` and `post_apply`.
</scenario>

<scenario id="CHK-014">
Given a tempdir repository whose module declares
`[[hook]] event = "pre_apply" command = "exit 0"` (no `must_succeed`),
when the engine resolves the hook,
then the resolved hook has `must_succeed = true`.
</scenario>
</requirement>

<requirement id="REQ-007">
### REQ-007: Variable precedence chain with reserved `patina.*` namespace

The engine resolves variables by composing six layers in priority
order from highest to lowest: CLI overrides (`-v key=value`),
per-machine variables (persisted in state directory), per-profile
variables (from the active profile's TOML), per-module variables
(from the module's `patina.toml`), repo-shared variables (from the
root `patina.toml`), and built-in variables under the `patina.*`
namespace. Any user attempt to set a variable in the `patina.*`
namespace at any layer except built-ins is rejected at resolution
with a typed error naming the offending key.

<done-when>
- A variable set as a CLI override shadows the same key set at any
  lower layer.
- A built-in variable `patina.os` resolves to `macos`, `linux`, or
  `windows` based on the host platform without any user
  configuration.
- A user attempt to set `patina.os` via `-v patina.os=foo` is
  rejected with a typed error naming `patina.os`.
- A user attempt to declare `patina.custom = "x"` in any `patina.toml`
  is rejected at parse with a typed error naming `patina.custom`.
- The full set of built-in `patina.*` variables documented in this
  SPEC is `patina.os`, `patina.arch`, `patina.hostname`, `patina.user`,
  `patina.home`, `patina.profile`, and the dynamic map
  `patina.env.*` exposing the current process's environment
  variables.
- A reference to `patina.env.FOO` resolves to the value of the `FOO`
  environment variable at apply time; a reference to a non-existent
  env var produces the strict-undefined error path documented in
  REQ-009 unless guarded by an `{% else %}` fallback.
</done-when>

<behavior>
- Given a module declaring `email = "module@example.com"` and a CLI
  override `-v email=cli@example.com`, when a template references
  `{{ email }}`, then the rendered output contains
  `cli@example.com`.
- Given any `patina.toml` declaring `patina.foo = "x"`, when the
  parser runs, then parsing fails with a typed error naming
  `patina.foo` and the substring `reserved`.
</behavior>

<scenario id="CHK-015">
Given a repository whose root `patina.toml` declares
`[variables] email = "root@example.com"` and a module declares
`[variables] email = "module@example.com"`,
when `patina apply --yes -v email=cli@example.com` runs against a
template referencing `{{ email }}`,
then the rendered output contains `cli@example.com`.
</scenario>

<scenario id="CHK-016">
Given a repository whose root `patina.toml` declares
`[variables] "patina.foo" = "bar"`,
when the engine parses the file,
then parsing fails with a typed error whose Display contains
`patina.foo` and the substring `reserved`.
</scenario>

<scenario id="CHK-040">
Given a process environment with `CI=true` and a `[[hook]]` declaring
`when = "patina.env.CI == 'true'"`,
when the engine resolves the hook,
then the hook's `when` clause evaluates true and the hook runs.
</scenario>
</requirement>

<requirement id="REQ-008">
### REQ-008: Profile resolution chain — env, persisted, auto-match, fallback

The engine resolves the active profile by composing four sources in
priority order: the `PATINA_PROFILE` environment variable, a
persisted profile name in the per-machine state directory, an
`auto_match` rule in the root `patina.toml` evaluated against the
built-in variable context, and a no-profile fallback. No CLI flag
selects the profile.

<done-when>
- With `PATINA_PROFILE=work` set, the engine resolves the profile to
  `work` regardless of other sources.
- With `PATINA_PROFILE` unset and a persisted choice of `home` in the
  state directory, the engine resolves to `home`.
- With `PATINA_PROFILE` unset, no persisted choice, and a root
  `patina.toml` declaring
  `[[auto_match]] when = "patina.hostname == 'tower'" profile = "desktop"`,
  the engine resolves to `desktop` when the host's name is `tower`.
- With all three above absent or non-matching, the engine resolves to
  the no-profile fallback (an empty profile name) and no profile-scoped
  variables or modules apply.
- No subcommand exposes a `--profile` flag (verified by the absence of
  the flag in `clap`'s derived parser).
</done-when>

<behavior>
- Given `PATINA_PROFILE=work`, when apply runs, then the engine logs
  the resolved profile as `work` at info level.
- Given no env var, no persisted choice, no auto-match, when apply
  runs, then the engine logs the resolved profile as `(none)` and
  proceeds with only the no-profile scope.
</behavior>

<scenario id="CHK-017">
Given a tempdir repository with no `[[auto_match]]` rules and a state
directory containing no persisted profile,
when `PATINA_PROFILE=work patina apply --yes --json` runs,
then the JSON output's top-level `profile` field equals `"work"`.
</scenario>

<scenario id="CHK-018">
Given a tempdir repository whose root `patina.toml` declares
`[[auto_match]] when = "patina.hostname == 'CHK-host'" profile = "desktop"`
and a host configured to report hostname `CHK-host`,
when `patina apply --yes --json` runs with `PATINA_PROFILE` unset and no
persisted choice,
then the JSON output's `profile` field equals `"desktop"`.
</scenario>
</requirement>

<requirement id="REQ-009">
### REQ-009: MiniJinja with strict undefined behavior renders templates and evaluates `when` expressions

The engine uses a single MiniJinja environment configured with
`UndefinedBehavior::Strict` to render `*.tmpl` files and to evaluate
`when` expressions on `[[file]]` and `[[hook]]` entries. A reference
to an undefined variable in either context produces a typed engine
error rather than silent empty-string substitution, with the
Jinja2-inherited exception that an undefined value in an `{% else %}`
fallback block renders as empty string (documented in assumptions).

<done-when>
- A template referencing `{{ undefined_var }}` produces a typed error
  during plan computation with the Display naming the variable.
- A `when = "undefined_var == 'x'"` expression produces a typed
  error during plan computation with the Display naming the
  variable.
- The same MiniJinja environment instance is used for both `.tmpl`
  rendering and `when` evaluation.
- A `*.tmpl` file with `{% if defined %}{{ undefined_var }}{% else %}fallback{% endif %}`
  renders `fallback` without error when `defined` is unset, per Jinja2's
  inherited else-block undefined behavior.
</done-when>

<behavior>
- Given a `gitconfig.tmpl` referencing `{{ patina.email }}` where
  `patina.email` is not a built-in and no `email` variable is set,
  when the plan is computed, then the engine returns a typed error
  naming the missing reference.
- Given a `when` expression referencing an undefined variable, when
  the plan is computed, then the engine returns a typed error naming
  the missing reference.
</behavior>

<scenario id="CHK-019">
Given a template `gitconfig.tmpl` containing
`[user]\nemail = {{ user_email }}` and no variable named `user_email`
in any layer,
when `patina apply --yes` runs,
then the command exits 1 and stderr contains the substring
`user_email`.
</scenario>

<scenario id="CHK-020">
Given a `[[file]]` entry with
`when = "patina.os == 'macos' and missing_var"` and no `missing_var`
in the resolved variable context,
when plan computation runs,
then the engine returns a typed error whose Display contains
`missing_var`.
</scenario>
</requirement>

<requirement id="REQ-010">
### REQ-010: Path canonicalization — absolute on read with lexical fallback

The engine canonicalizes every repository path, source path, target
path, and state-directory path it reads to absolute form. A path that
exists is canonicalized through the filesystem (resolving symlinks
and `.` / `..`); a path that does not yet exist is converted to
absolute form lexically by joining with the canonical parent or
current working directory.

<done-when>
- A relative repository path passed via `PATINA_REPO=./dotfiles`
  resolves to an absolute path in any error messages and journal
  entries.
- A target path `~/.zshrc` resolves to the absolute home-relative
  path of the invoking user.
- A target path whose parent directory does not yet exist
  canonicalizes lexically (joining with absolute CWD or absolute
  parent) rather than failing.
- The journal stores only canonical absolute paths; relative paths
  never appear in journal records.
</done-when>

<behavior>
- Given a CWD of `/home/user/work` and `PATINA_REPO=./dot`, when
  apply runs, then the journal records the repository root as
  `/home/user/work/dot`.
- Given a target path `~/.config/foo/bar.conf` and a
  not-yet-existing `~/.config/foo/` directory, when plan computation
  resolves the target, then the resolved path is
  `/home/user/.config/foo/bar.conf` (lexical fallback) rather than
  an error.
</behavior>

<scenario id="CHK-021">
Given a CWD `/tmp/work` and a tempdir repository at `/tmp/work/dot`,
when `PATINA_REPO=./dot patina apply --yes --json` runs,
then the JSON output's `repo_root` field equals `/tmp/work/dot`.
</scenario>
</requirement>

<requirement id="REQ-011">
### REQ-011: Single-fsync upfront postcard journal records the plan before any mutation

Before performing any filesystem mutation, the engine computes the
full plan (list of file operations) and writes
it to a single binary file at `<state>/patina/journal/<ts>.plan`
using `postcard` encoding. The file is fsync'd once, along with its
parent directory's fsync, before any operation in the plan is
attempted. The journal includes a version envelope so future format
changes can be detected and refused. The matching `<ts>.COMMIT`
sentinel embeds the committed apply record (see REQ-029) behind that
same version envelope, so the plan and commit-record formats version
together.

<done-when>
- A `patina apply` invocation writes one `<ts>.plan` file under
  `<state>/patina/journal/` before any source-to-target file
  operation executes.
- The plan file's first bytes contain a version envelope
  (a `u16` major version field at offset 0) decodable without
  invoking the full postcard decoder.
- The plan file is fsync'd; the parent directory is fsync'd; both
  fsyncs complete before the first mutation begins.
- The plan file persists across the entire apply duration and is
  deleted only after a successful `COMMIT` sentinel is written and
  fsync'd.
- The plan file's content is the same when the same source repository
  and the same variable context are observed twice in a row, modulo
  the timestamp in its filename.
</done-when>

<behavior>
- Given an apply that fsync'd the plan and then crashed before any
  mutation, when the engine starts the next time, then it discovers
  the plan and treats it as a recoverable in-progress apply.
- Given an apply that successfully wrote `COMMIT`, when the engine
  starts the next time, then no recovery is performed for that
  timestamp.
</behavior>

<scenario id="CHK-022">
Given a tempdir repository with one `[[file]]` entry,
when `patina apply --yes` runs and the process is killed
(`SIGKILL` on POSIX, `TerminateProcess` on Windows) immediately
after `flush_plan_and_fsync` returns but before the first mutation,
then `<state>/patina/journal/` contains exactly one `<ts>.plan` file
and no `<ts>.COMMIT` sentinel.
</scenario>
</requirement>

<requirement id="REQ-012">
### REQ-012: Per-operation progress cursor records completion without fsync

As the engine executes each operation in the plan, it appends a
progress record to `<state>/patina/journal/<ts>.progress` indicating
that op `i` has completed. Progress writes are unbuffered to the
kernel page cache but are not fsync'd per operation; the engine
relies on filesystem probing during recovery to reconcile the actual
state regardless of progress-cursor synchronization.

<done-when>
- A successful apply produces a `<ts>.progress` file containing one
  record per completed operation, in operation order.
- The progress file is written without explicit `fsync` calls; the
  engine documents that probing during recovery is the
  source-of-truth.
- An apply that crashes mid-execution leaves a progress file whose
  contents may lag the actual filesystem state by up to one
  operation; recovery does not assume the progress file is exact.
</done-when>

<behavior>
- Given an apply with a plan of 100 operations, when apply runs,
  then the engine performs zero `fsync` calls on the progress file
  during the execution loop (observable via syscall trace in a test
  environment that exposes it).
- Given an apply that completed N operations and was then killed,
  when the engine recovers, then it does not rely solely on the
  progress file but probes the filesystem to determine actual
  completion.
</behavior>

<scenario id="CHK-023">
Given a tempdir repository declaring three file operations and a test
harness that records `fsync` calls,
when `patina apply --yes` runs,
then the recorded fsyncs include one on the plan file, one on its
parent directory, and one on the `COMMIT` sentinel, but no
per-operation fsyncs on the progress file.
</scenario>
</requirement>

<requirement id="REQ-013">
### REQ-013: Crash recovery probes filesystem and converges to pre-apply or post-apply state

When the engine starts and finds a plan file without a corresponding
`COMMIT` sentinel, it enters recovery mode. Recovery probes the
filesystem for each operation in the plan to determine whether the
operation completed, partially completed, or did not start. The
engine then reverses any completed operations using the journaled
inverse operations and the backup directory, restoring the
pre-apply state. After recovery, the user can re-run `patina apply`
to retry.

<done-when>
- An apply interrupted before any operation executed leaves the
  filesystem in the pre-apply state after recovery; the plan file
  and progress file are removed.
- An apply interrupted after N of M operations completed, when
  recovery runs, results in the filesystem being restored to the
  pre-apply state (the N completed ops are reversed using backups
  and inverse ops); the plan file and progress file are removed.
- Recovery is idempotent: running recovery twice in a row produces
  the same final state as running it once.
- Recovery never proceeds forward (it does not finish a partial
  apply); it always rolls back to pre-apply state.
</done-when>

<behavior>
- Given a crashed apply at operation 5 of 10 with a backup directory
  preserving the original targets of ops 1-5, when `patina apply` is
  next invoked, then the engine first runs recovery to restore the
  five overwritten targets from backups, then presents the new
  invocation's plan as if no prior apply existed.
- Given a crashed apply where no operations had executed, when
  recovery runs, then it deletes the orphaned plan and progress
  files without touching any targets.
</behavior>

<scenario id="CHK-024">
Given a tempdir repository, an apply that completed 3 of 5 file
operations before SIGKILL, and the corresponding backup directory
intact,
when `patina apply --yes` is next invoked,
then before any new mutation occurs, the engine restores the 3
previously-overwritten targets from backups, removes the orphaned
plan and progress files, then proceeds with the new plan from
scratch.
</scenario>
</requirement>

<requirement id="REQ-014">
### REQ-014: Backups taken before overwrite to per-machine state; never enter the repo

Before the engine overwrites any pre-existing user file (including
replacing a regular file with a symlink), it copies the original to
`<state>/patina/backups/<ts>/<mirrored-target-path>`. The backup
directory is per-apply, identified by the same timestamp as the
plan file. The dotfiles repository is never written to during apply.

<done-when>
- Overwriting a pre-existing `~/.zshrc` produces a backup at
  `<state>/patina/backups/<ts>/home/<user>/.zshrc` (or the
  platform-equivalent path) before the symlink or copy is created.
- A file that does not pre-exist (the target is created fresh) does
  not produce a backup entry.
- After apply, the dotfiles repository directory has no new files
  written by Patina (verified by a git status check showing no
  changes attributable to the engine).
- The backup directory mirrors the absolute target paths under
  the `<ts>` root.
</done-when>

<behavior>
- Given a pre-existing `~/.zshrc` with content "old", when apply
  runs and replaces it with a symlink, then
  `<state>/patina/backups/<ts>/.../home/<user>/.zshrc` contains the
  bytes "old".
- Given no pre-existing `~/.gitconfig`, when apply renders a
  template to `~/.gitconfig`, then the backup directory contains no
  entry for `~/.gitconfig`.
</behavior>

<scenario id="CHK-025">
Given a tempdir HOME containing a pre-existing `~/.zshrc` with content
"original" and a Patina repository declaring a symlink target on
`~/.zshrc`,
when `patina apply --yes` runs,
then `<state>/patina/backups/<ts>/.../zshrc` is a regular file
containing the bytes "original" and `~/.zshrc` is a symlink.
</scenario>
</requirement>

<requirement id="REQ-015">
### REQ-015: Backup retention keeps the last ten apply cycles; older cycles GC'd on next apply

On each successful apply (after the `COMMIT` sentinel is written),
the engine garbage-collects backup directories older than the tenth
most recent apply, retaining the ten newest. No CLI command exposes
this GC; there is no `patina gc` in v1.0.

<done-when>
- After eleven successful applies, `<state>/patina/backups/` contains
  exactly ten subdirectories.
- The retained ten correspond to the ten most recent applies by
  timestamp.
- A failed apply (no `COMMIT` written) does not trigger GC.
- No `patina gc` subcommand exists in the CLI surface.
</done-when>

<behavior>
- Given a state directory with 15 historical backup subdirectories,
  when a 16th apply succeeds, then 5 of the oldest subdirectories
  are removed and 10 remain (the new apply plus the 9 most recent
  prior).
- Given a state directory with 3 backup subdirectories, when an
  apply fails midway, then no GC runs and the 3 prior subdirectories
  remain untouched.
</behavior>

<scenario id="CHK-026">
Given a tempdir state directory with `<state>/patina/backups/`
containing 15 timestamped subdirectories,
when `patina apply --yes` runs a successful apply,
then after `COMMIT` is written, `<state>/patina/backups/` contains
exactly 10 subdirectories: the one from the just-completed apply
plus the 9 most recent prior ones.
</scenario>
</requirement>

<requirement id="REQ-016">
### REQ-016: Per-machine state directory uses OS-appropriate locations

The engine resolves the per-machine state directory in an OS-specific
manner: on Linux, `$XDG_STATE_HOME/patina/` falling back to
`$HOME/.local/state/patina/`; on macOS,
`$HOME/Library/Application Support/patina/`; on Windows,
`%LOCALAPPDATA%\patina\`. The state directory holds the journal,
backups, persisted profile choice, persisted default repository
path, and the lock file. The dotfiles repository is never written to.

<done-when>
- On Linux with `XDG_STATE_HOME=/x/y`, the state directory is
  `/x/y/patina/`.
- On Linux with `XDG_STATE_HOME` unset, the state directory is
  `$HOME/.local/state/patina/`.
- On macOS, the state directory is
  `$HOME/Library/Application Support/patina/`.
- On Windows, the state directory is `%LOCALAPPDATA%\patina\`.
- The state directory contains subdirectories `journal/` and
  `backups/` plus files `profile` (text), `default_repo` (text),
  and `lock` (advisory-lock target).
</done-when>

<behavior>
- Given a Linux host with `XDG_STATE_HOME=/var/lib/patina-state`,
  when apply runs, then journal writes target
  `/var/lib/patina-state/patina/journal/<ts>.plan`.
- Given a Windows host with `LOCALAPPDATA=C:\Users\Kevin\AppData\Local`,
  when apply runs, then journal writes target
  `C:\Users\Kevin\AppData\Local\patina\journal\<ts>.plan`.
</behavior>

<scenario id="CHK-027">
Given a Linux test host with `XDG_STATE_HOME` set to a tempdir `T`,
when `patina apply --yes` runs successfully,
then `T/patina/journal/` contains a `.plan` and a `.COMMIT` file
and `T/patina/backups/<ts>/` exists.
</scenario>
</requirement>

<requirement id="REQ-017">
### REQ-017: `patina apply` prompts in TTY, exits without mutation in non-TTY, accepts `--yes` and `--force-deploy`

The `patina apply` subcommand computes the plan, renders a diff using
the `similar` crate, and behaves as follows:

- In a TTY: print the diff to stdout, prompt `Apply? [y/N]` on stderr,
  read a single line, apply only on `y`/`Y` confirmation.
- In a non-TTY: print the diff to stdout and exit with code 0 without
  performing any mutation.
- With `--yes`: apply unconditionally, regardless of TTY state, with
  no prompt.
- With `--force-deploy`: override every hook in the resolved plan to
  `must_succeed = false` for this invocation only.
- With `--json`: emit the plan and result as a single JSON document
  on stdout with no prompt. `--json` alone does NOT mutate the
  filesystem (the prompt is suppressed and the apply is treated as a
  preview). To apply via JSON output, pass both `--json --yes`.
- With `--pager=delta` or `--pager=difft`: pipe the rendered diff
  through the named external tool if found on PATH, otherwise fall
  back to the embedded `similar`-rendered diff.

<done-when>
- `patina apply` in a TTY shows the diff, prompts, and aborts on
  any input other than `y`/`Y`.
- `patina apply` in a non-TTY (verified via `is-terminal`) exits 0
  after printing the plan without mutation.
- `patina apply --yes` mutates regardless of TTY state without
  prompting.
- `patina apply --force-deploy --yes` runs the apply with every
  hook treated as `must_succeed = false` for this invocation; a
  pre_apply or post_apply hook failure warns rather than aborts or
  rolls back.
- `patina apply --pager=delta` invoked on a system without `delta`
  on PATH falls back to the embedded `similar` renderer with a
  one-line warning on stderr.
- `patina apply --json` produces a single JSON object on stdout with
  fields `repo_root`, `profile`, `plan` (array of ops), and `result`
  (one of `applied` / `previewed` / `aborted` / `rolled_back`).
- `patina apply --json` without `--yes` produces a JSON document
  whose `result` field is `previewed`; the filesystem is not
  mutated.
- `patina apply --json --yes` produces a JSON document whose
  `result` field is `applied` (on success) or `rolled_back` (when a
  `must_succeed` `post_apply` hook failed and the file ops were
  reversed).
</done-when>

<behavior>
- Given a TTY and a plan with one file operation, when
  `patina apply` runs and the user enters `n`, then no mutation
  occurs and the process exits with code 5 (user declined).
- Given a non-TTY and the same plan, when `patina apply` runs,
  then the diff prints and the process exits 0 without mutation.
- Given a hook that always fails with exit 1 and
  `must_succeed = true`, when `patina apply --yes` runs without
  `--force-deploy`, then the apply aborts (pre_apply) or rolls back
  (post_apply).
- Given the same hook, when `patina apply --yes --force-deploy`
  runs, then the apply completes and stderr contains a warning that
  the hook failed.
</behavior>

<scenario id="CHK-028">
Given a tempdir Patina repository declaring one symlink `[[file]]` and
a test harness simulating a non-TTY stdin,
when `patina apply` runs (no `--yes`),
then the process exits 0, the target symlink is not created, and
stdout contains the rendered diff.
</scenario>

<scenario id="CHK-029">
Given a tempdir repository declaring a `[[hook]]` with
`event = "post_apply" command = "exit 1"` (default `must_succeed = true`),
when `patina apply --yes` runs,
then the file operations execute first, the hook returns non-zero,
the engine rolls back the file operations using backups, and the
process exits with code 3.
</scenario>

<scenario id="CHK-030">
Given the same hook,
when `patina apply --yes --force-deploy` runs,
then the file operations execute, the hook returns non-zero, the
file operations are NOT rolled back, stderr contains a warning
naming the hook, and the process exits with code 0.
</scenario>
</requirement>

<requirement id="REQ-018">
### REQ-018: `patina status` classifies every managed file as CLEAN / DRIFTED / MISSING / ORPHANED

The `patina status` subcommand reads the most recent journal (the
last `COMMIT`-sentineled apply) and classifies every managed target
into one of four states by comparing the recorded expectation — the
link target for symlink-mode targets, or the `blake3` content hash
recorded per REQ-029 for content-mode targets — to the current
filesystem state:

- CLEAN: target exists and matches expected.
- DRIFTED: target exists but content differs from expected.
- MISSING: target was applied but no longer exists on disk.
- ORPHANED: target exists on disk but the current plan no longer
  manages it (formerly managed, since removed from the repo).

With `--json`, the command emits a structured object including a
`last_apply` field with `at`, `user`, and `host` keys derived from
the journal record.

<done-when>
- A target unmodified since apply is reported as `CLEAN`.
- A target whose disk content differs from the expected hash is
  reported as `DRIFTED`.
- A target that was applied but has been deleted on disk is
  reported as `MISSING`.
- A target present on disk that the current plan no longer manages
  (formerly in journal, removed from repo) is reported as
  `ORPHANED`.
- `patina status --json` emits a top-level object with `last_apply`
  (object with `at`, `user`, `host`), `files` (array of
  `{path, state}` objects), and aggregate counters
  (`clean`, `drifted`, `missing`, `orphaned`).
- A multi-target `[[file]]` entry (per REQ-005's `targets` array)
  produces one row per target in the human-readable output and one
  element per target in the `files` JSON array. The aggregate
  counters count each target independently: an entry with three
  targets all CLEAN contributes 3 to the `clean` counter.
</done-when>

<behavior>
- Given a successful apply and no subsequent filesystem changes,
  when `patina status --json` runs, then every entry's `state` is
  `clean` and `last_apply.at` is an RFC 3339 timestamp.
- Given a copy-mode target whose content was edited externally,
  when `patina status` runs, then the affected file is reported
  with state `drifted`.
- Given a `[[file]]` entry with
  `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`
  applied cleanly and `~/.codex/agent.toml` then edited
  externally, when `patina status` runs, then the output contains
  one CLEAN entry for `~/.claude/agent.toml` and one DRIFTED entry
  for `~/.codex/agent.toml`.
</behavior>

<scenario id="CHK-031">
Given a successful apply of three file operations and no subsequent
filesystem changes,
when `patina status --json` runs,
then the JSON output's `clean` counter is 3 and `drifted`,
`missing`, `orphaned` are each 0.
</scenario>

<scenario id="CHK-032">
Given a successful apply that materialized `~/.gitconfig` (copy
mode), followed by a test step that appends bytes to `~/.gitconfig`,
when `patina status --json` runs,
then the JSON output's `drifted` counter is 1 and the `files` array
contains an entry with `path` resolving to `.gitconfig` and `state`
equal to `drifted`.
</scenario>

<scenario id="CHK-048">
Given a tempdir repository with a `[[file]]` entry declaring
`source = "agent.toml"`,
`targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`,
`mode = "copy"`, an applied state, and a test step that overwrites
`~/.codex/agent.toml` with different bytes,
when `patina status --json` runs,
then the JSON output's `files` array contains two entries — one
with `path` resolving to `.claude/agent.toml` and `state = "clean"`,
one with `path` resolving to `.codex/agent.toml` and
`state = "drifted"` — the `clean` counter is at least 1, and the
`drifted` counter is at least 1.
</scenario>
</requirement>

<requirement id="REQ-019">
### REQ-019: `patina rollback` reverses the last successful apply via the journal and backups

The `patina rollback` subcommand reverses the most recent `COMMIT`ed
apply by replaying its inverse operations using the journal and the
corresponding backup directory. After rollback, the filesystem state
matches the pre-apply state of the most recent apply. The rolled-back
apply's journal is marked rolled-back; the user can apply again to
re-establish the dotfile state.

<done-when>
- A successful `patina apply` followed by `patina rollback --yes`
  leaves the filesystem indistinguishable from its pre-apply state.
- Files that were created fresh by apply (no backup) are deleted.
- Files that existed before apply are restored from backups.
- The journal for the rolled-back apply is marked rolled-back (a
  `<ts>.ROLLED_BACK` sentinel is written and fsync'd), and is
  excluded from `patina status`'s "last apply" computation.
- A `patina rollback` invoked with no prior successful apply emits a
  typed error and exits 1.
- A multi-target `[[file]]` entry's targets (per REQ-005) are
  restored as an atomic unit: either every target in the entry
  reverts to pre-apply state, or rollback fails (no partial
  restore). This mirrors the all-or-nothing semantic the engine
  applies per-`[[file]]` entry during apply and crash recovery
  (REQ-013).
</done-when>

<behavior>
- Given a pre-existing `~/.zshrc` with content "old", an apply that
  replaced it with a symlink, when `patina rollback --yes` runs,
  then `~/.zshrc` is again a regular file with content "old".
- Given a target `~/.gitconfig` that did not exist before apply,
  when `patina rollback --yes` runs, then `~/.gitconfig` no longer
  exists.
- Given no prior apply on a fresh state directory, when
  `patina rollback` runs, then the process exits 1 and stderr
  names "no prior apply found".
- Given a pre-existing `~/.claude/agent.toml` (content "old") and
  no pre-existing `~/.codex/agent.toml`, an apply that materialized
  a `[[file]]` entry with
  `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]` and
  `mode = "copy"` from a source containing "new", when
  `patina rollback --yes` runs, then `~/.claude/agent.toml` is a
  regular file with content "old" and `~/.codex/agent.toml` does
  not exist.
</behavior>

<scenario id="CHK-033">
Given a pre-existing `~/.zshrc` with content "original" and a
Patina apply that materialized it as a symlink to a repo file,
when `patina rollback --yes` runs,
then `~/.zshrc` is a regular file (not a symlink) with content
"original" and the journal contains a `<ts>.ROLLED_BACK` sentinel.
</scenario>

<scenario id="CHK-049">
Given a tempdir HOME with a pre-existing `~/.claude/agent.toml`
(content "old") and no pre-existing `~/.codex/agent.toml`, a
Patina apply that materialized both targets via a `[[file]]` entry
with `targets = ["~/.claude/agent.toml", "~/.codex/agent.toml"]`
and `mode = "copy"` from a source containing "new",
when `patina rollback --yes` runs,
then `~/.claude/agent.toml` is a regular file with content "old",
`~/.codex/agent.toml` does not exist, and the journal contains a
`<ts>.ROLLED_BACK` sentinel.
</scenario>
</requirement>

<requirement id="REQ-020">
### REQ-020: `patina debug journal <path>` decodes a binary plan into human-readable form

`patina debug` is a clap subcommand group. v1.0 ships exactly one
subcommand under it (`debug journal <path>`); the group exists as
the extension point for future debug subcommands (SPEC-0003 layers
`debug drift-cache` onto the same group without restructuring the
CLI surface).

The `patina debug journal <path>` subcommand reads a postcard-encoded
journal file at the given path, validates the version envelope,
decodes the plan, and prints a human-readable rendering to stdout. If
the file is missing or its version envelope is incompatible with the
running binary, the command emits a typed error and exits 1.

<done-when>
- `patina debug` exists as a clap subcommand group; running
  `patina debug --help` lists `journal` as a subcommand and exits 0.
- `patina debug journal /path/to/plan` on a valid plan prints the
  recorded operations and timestamps to stdout.
- The output is structured (one operation per line or per indented
  block) and identifies each op's mode, source, and target.
- An invalid path produces a typed error naming the path and
  exits 1.
- A plan written by a newer version of Patina (version envelope
  major version exceeds the running binary's) is refused with a
  typed error naming both versions; exit 1.
</done-when>

<behavior>
- Given a plan file produced by an apply earlier in the test, when
  `patina debug journal <that-path>` runs, then stdout contains the
  decoded ops in a format that identifies each op's mode and
  paths.
- Given a path that does not exist, when the subcommand runs, then
  exit code is 1 and stderr names the path.
</behavior>

<scenario id="CHK-034">
Given a tempdir repository, a successful `patina apply`, and the
resulting `<state>/patina/journal/<ts>.plan` file,
when `patina debug journal <ts>.plan` runs,
then stdout contains the substring `symlink` or `copy`
corresponding to the modes declared in the test fixture and the
process exits 0.
</scenario>

<scenario id="CHK-050">
Given the compiled `patina` binary,
when `patina debug --help` runs,
then stdout names `journal` as a subcommand of the `debug` group and
the process exits 0.
</scenario>
</requirement>

<requirement id="REQ-021">
### REQ-021: Stdout output is deterministic — no wall-clock timestamps in human-readable output

Two consecutive `patina apply` invocations against an unchanged
source repository and unchanged environment produce byte-identical
stdout output. The human-readable output contains no wall-clock
timestamps, no PIDs, no random IDs. The journal record on disk is
permitted to contain timestamps (it is not user-facing); the diff
on stdout is not.

<done-when>
- `patina apply --yes` run twice on the same fixture produces
  byte-identical stdout output across the two invocations (the
  second is effectively a no-op since the first applied, so the
  "no changes" output is also stable).
- `patina apply --json` likewise produces byte-identical stdout
  output across two consecutive invocations on unchanged input.
- No `chrono::Utc::now()`-style or `jiff::Timestamp::now()`-style
  call appears in user-facing output paths (verified by grep + code
  review; tests assert the byte-identity above as the primary
  signal).
</done-when>

<behavior>
- Given an apply that emits a diff and a "Applied 3 changes."
  summary, when the same command runs a second time after the
  first succeeded, then the second invocation's diff is empty and
  the summary is "No changes." — both reproducible across repeats.
- Given a `--json` apply, when the same command runs twice in a
  row, then `diff -u out1 out2` produces empty output.
</behavior>

<scenario id="CHK-035">
Given a tempdir repository and a clean state directory,
when `patina apply --yes --json > out1.json` runs and then
`patina apply --yes --json > out2.json` runs against the
unchanged repository,
then `diff -u out1.json out2.json` produces no output.
</scenario>
</requirement>

<requirement id="REQ-022">
### REQ-022: Exit codes formalized — 0 success, 1 generic, 2 pre_apply abort, 3 post_apply rollback, 4 lock timeout, 5 user declined

The CLI's exit codes are:

- `0`: success.
- `1`: generic error (config parse failure, IO error, undefined
  variable, etc.).
- `2`: a `must_succeed = true` `pre_apply` hook failed, aborting
  before any file operations.
- `3`: a `must_succeed = true` `post_apply` hook failed, triggering
  rollback of the file operations.
- `4`: timed out waiting to acquire the advisory lock at
  `<state>/patina/lock`.
- `5`: user declined the interactive prompt or refused an elevation
  request (the latter applies once SPEC-0002 adds the Windows
  elevation flow).

<done-when>
- A successful `patina apply --yes` exits 0.
- A `patina apply` whose plan computation fails (undefined
  variable, invalid TOML) exits 1.
- A `patina apply --yes` whose pre_apply hook with
  `must_succeed = true` returns non-zero exits 2 without performing
  any file operation.
- A `patina apply --yes` whose post_apply hook with
  `must_succeed = true` returns non-zero exits 3 after rolling back
  the file operations.
- A `patina apply` that cannot acquire the lock within the
  configured timeout exits 4.
- A `patina apply` in TTY where the user enters `n` at the prompt
  exits 5.
</done-when>

<behavior>
- Given a `pre_apply` hook with `command = "exit 7"` and
  `must_succeed = true`, when `patina apply --yes` runs, then the
  process exits with code 2 (not 7) and no file operations have
  executed.
- Given a TTY apply where the user enters `n`, when the process
  returns, then the exit code is 5.
</behavior>

<scenario id="CHK-036">
Given a tempdir repository declaring a `[[hook]]` with
`event = "pre_apply" command = "false"`,
when `patina apply --yes` runs,
then the process exits with code 2.
</scenario>
</requirement>

<requirement id="REQ-023">
### REQ-023: Advisory file lock coordinates mutations and read-only commands

The engine acquires an advisory file lock on `<state>/patina/lock`
before any mutating operation. Mutating subcommands (`apply`,
`rollback`) acquire an exclusive lock; read-only subcommands
(`status`) acquire a shared lock. If a read-only command cannot
acquire a shared lock within five seconds (because another process
holds an exclusive lock), it emits a warning to stderr and proceeds
without the lock. If a mutating command cannot acquire the
exclusive lock within sixty seconds, it exits with code 4.

The acquisition *mechanism* the engine apply path uses is selectable
per REQ-030, so a caller that already holds the exclusive lock, or one
that needs a non-blocking attempt, drives the same apply without the
engine self-acquiring a conflicting lock. The behaviour described here
is the default (blocking) policy and is what `patina apply` /
`patina rollback` use.

<done-when>
- Two concurrent `patina apply` invocations on the same machine do
  not interleave file mutations; the second blocks until the first
  releases.
- A concurrent `patina status` and `patina apply` allow `status` to
  read but `apply` blocks `status` momentarily during the mutation
  window.
- `patina apply` blocked on the lock for >60s exits with code 4.
- `patina status` blocked on a shared lock for >5s emits the warning
  and proceeds; exit code is 0 if status itself succeeds.
- A process that crashes while holding the lock causes the OS to
  release the lock automatically; the next process acquires it
  cleanly.
</done-when>

<behavior>
- Given two CLI processes running `patina apply --yes`
  simultaneously, when both start, then process A acquires the
  exclusive lock, process B blocks until A finishes, and both
  applies complete sequentially.
- Given a CLI process holding an exclusive lock and a concurrent
  `patina status` invocation, when the status invocation has waited
  five seconds, then it prints a warning to stderr and proceeds to
  read state without the lock; exit code 0.
</behavior>

<scenario id="CHK-037">
Given a test harness that spawns two `patina apply --yes` processes
against the same tempdir state directory and synchronizes their
start within a 100ms window,
when both processes complete,
then the union of their journal `<ts>.plan` files numbers exactly
two and neither apply was interleaved with the other (verified by
the journal timestamps being non-overlapping).
</scenario>
</requirement>

<requirement id="REQ-024">
### REQ-024: No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, or `unimplemented!` in production code

Production code in both `patina-core` and `patina-cli` contains no
`unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, or
`unimplemented!` calls outside `#[cfg(test)]` modules. Clippy is
configured to deny these patterns, and CI fails any PR introducing
them.

<done-when>
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
  exits 0, with the lint set enabling `clippy::unwrap_used`,
  `clippy::expect_used`, `clippy::panic`, `clippy::unreachable`,
  `clippy::todo`, and `clippy::unimplemented` for non-test code.
- `clippy.toml` declares `allow-expect-in-tests = true` so test code
  may use `.expect("descriptive message")`.
- A grep of `patina-core/src/**/*.rs` and `patina-cli/src/**/*.rs`
  excluding `#[cfg(test)]` modules yields zero hits for `unwrap()`,
  `expect(`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`.
- Genuinely impossible preconditions in production are refactored to
  be type-system enforced (e.g., via the `NonZero*` or enum-state
  patterns) or surfaced via `?` plus a typed error variant, never
  via panic.
</done-when>

<behavior>
- Given the workspace at HEAD, when Clippy runs with the project's
  configured lint set, then no warnings or errors fire for the
  panic-family lints on production code.
- Given a contributor adds `.unwrap()` to a non-test path in
  `patina-core`, when CI runs, then the Clippy step fails and blocks
  merge with a `clippy::unwrap_used` error naming the offending
  line.
</behavior>

<scenario id="CHK-038">
Given the repository at HEAD after this SPEC lands,
when `cargo clippy --workspace --all-targets --locked -- -D warnings` runs,
then the command exits 0.
</scenario>

<scenario id="CHK-039">
Given a working tree where a contributor has inserted `foo.unwrap()`
in `patina-core/src/apply.rs`,
when `cargo clippy --workspace --all-targets -- -D warnings` runs,
then the command exits non-zero with a `clippy::unwrap_used` error
naming `patina-core/src/apply.rs` and the offending line.
</scenario>
</requirement>

<requirement id="REQ-025">
### REQ-025: CI runs the full test suite on macOS, Linux, and Windows

The continuous integration pipeline runs the workspace test suite
(`cargo test --workspace --locked` plus `cargo clippy --workspace
--all-targets --locked -- -D warnings`) on `macos-latest`,
`ubuntu-latest`, and `windows-latest` runners on every push and
every pull request. All three matrix jobs are in the
required-status-checks set; failure on any single OS blocks merge
into `main`. The north-star parity rule — "macOS, Linux, Windows are
first-class; two-of-three is not done" — is operationalised here.

<done-when>
- A workflow file under `.github/workflows/` declares a test job
  (or matrix of jobs) with `strategy.matrix.os` containing all
  three of `macos-latest`, `ubuntu-latest`, `windows-latest`,
  running both `cargo test --workspace --locked` and
  `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- The workflow triggers on both `push` (to `main`) and
  `pull_request`.
- All three OS jobs appear in the repository's required-status-checks
  configuration on `main`; merge is blocked when any one of them
  reports failure.
- A regression that affects only one OS (e.g. a Windows long-path
  case, or a Linux-only `#[cfg(target_os = "linux")]` panic) causes
  only that OS's matrix job to fail; merge is still blocked.
- Third-party actions referenced by the workflow are pinned to
  their latest published major version per
  `.claude/rules/github-actions/github-actions-versioning.md`.
</done-when>

<behavior>
- Given a PR introducing a Linux-only unconditional `panic!`, when
  CI runs, then `ubuntu-latest` fails while macOS and Windows pass,
  and merge into `main` is blocked.
- Given a PR breaking Windows path canonicalisation but green on
  macOS and Linux, when CI runs, then only `windows-latest` fails
  and merge is blocked.
</behavior>

<scenario id="CHK-051">
Given the repository's CI workflow file(s) under
`.github/workflows/`,
when parsed as YAML,
then a job exists whose `strategy.matrix.os` list simultaneously
contains the strings `macos-latest`, `ubuntu-latest`, and
`windows-latest`, and the workflow `on:` block includes both `push`
and `pull_request`.
</scenario>

<scenario id="CHK-052">
Given a working tree where a contributor has inserted
`#[cfg(target_os = "windows")] compile_error!("forced");` into
`patina-core/src/lib.rs`,
when the matrix CI runs,
then `windows-latest` exits non-zero while `macos-latest` and
`ubuntu-latest` exit 0, and the workflow's overall status is
failure.
</scenario>
</requirement>

<requirement id="REQ-026">
### REQ-026: User-facing output flows through `output::Reporter`; raw print macros are clippy-denied elsewhere

The workspace defines an `output::Reporter` trait that is the single
user-facing output sink for `patina-cli` (and any user-facing prints
that originate inside `patina-core`). It has at least two
implementations: `HumanReporter` (formatted text, colour where
appropriate, the default) and `JsonReporter` (line-delimited JSON
when `--json` is set). Every user-facing message routes through a
`Reporter` method. The `println!`, `eprintln!`, `print!`, and
`eprint!` macros are denied by Clippy outside the `output` module.
The `tracing` macros (`info!`, `warn!`, `error!`, `debug!`,
`trace!`) remain permitted everywhere because they emit structured
events, not user-facing output.

<done-when>
- A `pub trait Reporter` exists in `patina-cli/src/output/mod.rs`
  (or equivalent path) with methods covering at minimum: progress
  events, diff rendering, prompt + response capture, and a final
  result line.
- `HumanReporter` and `JsonReporter` implementations exist under
  the `output` module and both satisfy the deterministic-stdout
  property required by REQ-021 on identical inputs.
- `clippy.toml` lists `std::println`, `std::eprintln`,
  `std::print`, and `std::eprint` under `disallowed-macros`. The
  `output` module is the only path permitted to call these macros;
  every other file in the workspace must route output through a
  `Reporter`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
  exits 0 on the workspace at HEAD.
- Adding `println!("hi")` to a non-`output` file in `patina-core`
  or `patina-cli` causes the same clippy command to exit non-zero
  with a `clippy::disallowed_macros` error naming the offending
  line.
</done-when>

<behavior>
- Given `patina apply` with no `--json`, when it runs, then all
  user-facing output is rendered by `HumanReporter`.
- Given `patina apply --json`, when it runs, then all user-facing
  output is rendered by `JsonReporter` and stdout is a sequence
  of one JSON document per line.
- Given a contributor adds `eprintln!("debug")` to
  `patina-core/src/apply.rs`, when CI clippy runs, then it fails
  with a `clippy::disallowed_macros` error and the PR is blocked.
- Given a contributor adds `tracing::info!("foo")` anywhere in the
  workspace, when CI clippy runs, then no `disallowed_macros`
  error fires.
</behavior>

<scenario id="CHK-053">
Given the workspace at HEAD,
when `cargo clippy --workspace --all-targets --locked -- -D warnings`
runs,
then it exits 0 and `clippy.toml`'s `disallowed-macros` list
contains entries for `std::println`, `std::eprintln`, `std::print`,
and `std::eprint`.
</scenario>

<scenario id="CHK-054">
Given a working tree where a contributor has inserted
`println!("hi")` in `patina-core/src/plan.rs`,
when `cargo clippy --workspace --all-targets --locked -- -D warnings`
runs,
then it exits non-zero with a `clippy::disallowed_macros` error
naming `patina-core/src/plan.rs` and the offending line.
</scenario>

<scenario id="CHK-055">
Given two consecutive invocations of `patina apply --json` against
an unchanged source tree,
when both invocations write to stdout via `JsonReporter`,
then their stdout output is byte-identical (the deterministic
property required by REQ-021 holds across both Reporter
implementations).
</scenario>
</requirement>

<requirement id="REQ-027">
### REQ-027: `docs/ARCHITECTURE.md` and `docs/USER_GUIDE.md` ship with named structural anchors

The repository carries two top-level docs files inside a `docs/`
directory: `docs/ARCHITECTURE.md` (contributor-facing engine
architecture) and `docs/USER_GUIDE.md` (user-facing usage and
operational guidance). Each file has a fixed set of `##`-level
headings that downstream tests and cross-SPEC references can rely
on. The product north star's Known-Unknowns note "SPEC-0001
documents only" the cloud-sync constraint on the per-machine state
directory; the `## State directory` section of
`docs/USER_GUIDE.md` carries that constraint as a structured list
of providers to avoid.

<done-when>
- `docs/ARCHITECTURE.md` exists and contains at least these
  `##`-level headings, by exact text: `## Engine layers`,
  `## Journal format`, `## Apply phases`, `## Recovery`.
- `docs/USER_GUIDE.md` exists and contains at least these
  `##`-level headings, by exact text: `## Installation`,
  `## Declaring dotfiles`, `## Apply flow`, `## State directory`,
  `## Recovery`, `## Troubleshooting`.
- The `## State directory` section of `docs/USER_GUIDE.md` contains
  a markdown bullet list naming cloud-sync paths the state
  directory must not live on; the bullets include (at minimum) the
  literal entries `iCloud Drive`, `OneDrive`, `Dropbox`, `Box`,
  `Google Drive`, `Syncthing`.
- An integration test parses both files as markdown and asserts
  the required heading set is present in each, and that the
  cloud-sync providers appear as bullets under the named section.
  Tests gate structural presence (heading existence by exact text,
  bullet membership in a named section) — never substring-match
  prose, per the test-hygiene rule in AGENTS.md.
</done-when>

<behavior>
- Given the repository at HEAD, when the docs-structure
  integration test runs, then both files exist and every required
  `##`-level heading is present in each.
- Given a contributor deletes `## State directory` from
  `docs/USER_GUIDE.md`, when the docs-structure test runs, then it
  fails naming the missing heading.
- Given a contributor renames a bullet entry under
  `## State directory` from `Google Drive` to `Google Drive (gdrive)`,
  when the test runs, then it fails naming the missing literal
  entry (the test asserts list membership by exact text, not
  prefix match).
</behavior>

<scenario id="CHK-056">
Given the repository at HEAD,
when an integration test parses `docs/ARCHITECTURE.md` and extracts
the set of `##`-level headings,
then the set contains, by exact text, `Engine layers`,
`Journal format`, `Apply phases`, `Recovery`.
</scenario>

<scenario id="CHK-057">
Given the repository at HEAD,
when an integration test parses `docs/USER_GUIDE.md` and extracts
the set of `##`-level headings,
then the set contains, by exact text, `Installation`,
`Declaring dotfiles`, `Apply flow`, `State directory`, `Recovery`,
`Troubleshooting`.
</scenario>

<scenario id="CHK-058">
Given the repository at HEAD,
when an integration test extracts the markdown bullet-list items
from the body of the `## State directory` section in
`docs/USER_GUIDE.md`,
then the extracted set contains each of `iCloud Drive`, `OneDrive`,
`Dropbox`, `Box`, `Google Drive`, `Syncthing` as a literal
bullet-text entry.
</scenario>
</requirement>

<requirement id="REQ-028">
### REQ-028: `deny.toml` configured and `cargo deny check` gates CI

The repository carries a `deny.toml` at root with `[licenses]`,
`[advisories]`, `[bans]`, and `[sources]` sections populated. Every
push and pull request runs `cargo deny check` as a required CI job;
the job is in the required-status-checks set so licence, advisory,
bans, and sources violations block merge into `main`.

<done-when>
- `deny.toml` exists at the repository root.
- Parsed as TOML, the document contains top-level tables named
  `licenses`, `advisories`, `bans`, and `sources`.
- The `[licenses]` allowlist captures the policy under which the
  project ships (e.g. `MIT`, `Apache-2.0`); GPL-family licences
  (and any other licences incompatible with the project's
  distribution model) are not in the allowlist.
- A CI workflow under `.github/workflows/` runs
  `cargo deny check` (or an equivalent action wrapper) on every
  `push` to `main` and every `pull_request`.
- Any third-party action used to run cargo-deny is pinned to its
  latest published major version per
  `.claude/rules/github-actions/github-actions-versioning.md`.
- The cargo-deny job is in the required-status-checks set so merge
  to `main` is blocked when it fails.
- Adding a dependency licensed `GPL-3.0` (or any other licence
  outside the allowlist) to `Cargo.toml` causes `cargo deny check`
  to fail with a `licenses` error naming the offending crate.
</done-when>

<behavior>
- Given the workspace at HEAD, when `cargo deny check` runs
  against the configured `deny.toml`, then it exits 0 because all
  current dependencies satisfy the licence, advisory, bans, and
  sources policy.
- Given a PR adding a GPL-3.0-licensed dependency to `Cargo.toml`,
  when CI runs, then the cargo-deny job fails with a `licenses`
  error naming the crate and merge is blocked.
</behavior>

<scenario id="CHK-059">
Given the repository at HEAD,
when `cargo deny check` runs against the configured `deny.toml`,
then it exits 0.
</scenario>

<scenario id="CHK-060">
Given `deny.toml` exists at the repository root,
when parsed as TOML,
then the resulting document contains top-level tables named
`licenses`, `advisories`, `bans`, and `sources`.
</scenario>

<scenario id="CHK-061">
Given a working tree where `Cargo.toml` declares a dependency on a
crate published under the `GPL-3.0` licence and that licence is not
in the `deny.toml` allowlist,
when `cargo deny check` runs,
then it exits non-zero with a `licenses` error naming the
offending crate.
</scenario>
</requirement>

<requirement id="REQ-029">
### REQ-029: The committed apply record retains per-target source provenance and a blake3 content hash

The `<ts>.COMMIT` sentinel embeds the committed apply record
(`ApplyRecord`) behind the shared version envelope (REQ-011). For
every materialized target the record retains enough provenance for
later commands and SPECs to map the target back to its source and to
detect content drift, without re-reading the repository or the
already-deleted plan file:

- the canonical absolute target path;
- the canonical source path the target was materialized from — for
  symlink-mode targets this is the recorded link target; for
  content-mode targets (copy, copy-tree files, template render) it is
  the canonical source the bytes were copied or rendered from;
- the `[[file]]`-entry index that groups multi-target entries into
  the atomic rollback unit (REQ-019);
- for content-mode targets, a 32-byte `blake3` hash of the
  materialized bytes.

The hash is `blake3` rather than a `std::hash` fingerprint so the
same hash serves the journal here and the SPEC-0003 drift cache,
which compares a freshly computed `blake3` of a target against this
recorded value. Because the record layout widened relative to the
first implementation, the shared version-envelope major is bumped to
`2`; a binary reading a major it does not support refuses the record
per the REQ-011 envelope rule rather than mis-decoding it.

This record is the sole post-commit source of truth: the `<ts>.plan`
file is deleted at commit (REQ-011), so SPEC-0002 (`remove`,
`promote`) and SPEC-0003 (watcher subscriptions, drift detection)
read source paths and content hashes from this record, not from the
plan.

<done-when>
- After a successful apply, decoding the `<ts>.COMMIT` record yields,
  for each content-mode target, a non-empty canonical source path and
  a 32-byte `blake3` hash of the bytes written to that target.
- After a successful apply, decoding the record yields, for each
  symlink-mode target, the canonical link target as its source.
- Two consecutive applies of an unchanged source repository record a
  byte-identical `blake3` hash for each content target (the hash is a
  stable function of the bytes, consistent with REQ-021 determinism
  for any output derived from it).
- Any `<ts>.plan` or `<ts>.COMMIT` file this binary writes carries
  `2` in the `u16` major version field at offset 0.
- A `<ts>.COMMIT` record whose envelope major exceeds the running
  binary's supported major is refused with the typed
  version-mismatch error from REQ-011, naming both versions.
- `patina status` (REQ-018) classifies content targets by comparing a
  freshly computed `blake3` of the live file against the recorded
  `blake3` hash; CLEAN/DRIFTED behaviour is unchanged.
</done-when>

<behavior>
- Given a copy-mode apply of `<repo>/git/gitconfig` to
  `~/.gitconfig`, when the `<ts>.COMMIT` record is decoded, then the
  entry for `~/.gitconfig` names the canonical source
  `<repo>/git/gitconfig` and a 32-byte `blake3` hash equal to the
  `blake3` of the materialized bytes.
- Given a symlink-mode apply, when the record is decoded, then the
  entry's source equals the canonical path the link points at.
- Given a record written by a future Patina whose envelope major is
  `3`, when a binary supporting major `2` decodes it, then decoding
  fails with the version-mismatch error naming `3` and `2`.
</behavior>

<scenario id="CHK-062">
Given a tempdir repository whose `git` module declares
`[[file]] source = "gitconfig" target = "~/.gitconfig" mode = "copy"`
and `<repo>/git/gitconfig` with arbitrary content,
when `patina apply --yes` runs and the resulting
`<state>/patina/journal/<ts>.COMMIT` record is decoded,
then the decoded record contains an entry whose target resolves to
`~/.gitconfig`, whose source equals the canonical absolute path of
`<repo>/git/gitconfig`, and whose content hash equals the 32-byte
`blake3` of the bytes of `<repo>/git/gitconfig`.
</scenario>

<scenario id="CHK-063">
Given a tempdir repository with one content-mode `[[file]]` entry,
when `patina apply --yes` runs twice against the unchanged source
and both `<ts>.COMMIT` records are decoded,
then the recorded `blake3` content hash for that target is
byte-identical across the two records.
</scenario>

<scenario id="CHK-064">
Given the `<state>/patina/journal/<ts>.COMMIT` file produced by a
successful apply,
when its first two bytes are read as a little-endian `u16`,
then the value equals `2`.
</scenario>
</requirement>

<requirement id="REQ-030">
### REQ-030: Engine apply entry points accept a lock-acquisition policy

The engine apply entry points (`apply` / `execute_plan`) take an
explicit lock-acquisition policy that selects how the exclusive
advisory lock (REQ-023) is obtained for the run, rather than
unconditionally self-acquiring a sixty-second blocking exclusive lock.
The policy has three variants:

- **Blocking** — acquire the exclusive lock with the REQ-023 sixty-second
  cap and map a timeout to exit code 4. This is the default and the
  policy `patina apply` and `patina rollback` use; their observable
  behaviour is unchanged.
- **NonBlocking** — make a single acquisition attempt; on contention
  return a typed contention error and perform zero filesystem mutation
  (no plan flush, no journal write, no backups). This is the policy
  the SPEC-0003 watcher uses to skip a re-apply cycle when the CLI
  holds the lock.
- **Held** — the caller supplies an already-acquired exclusive
  [`LockGuard`]; the engine does not acquire the lock a second time.
  This is the policy SPEC-0002 `remove` / `promote` use to mutate the
  repository and then re-journal through a re-apply while holding a
  single lock for the whole command, without self-contending.

The default-policy path is byte-for-byte equivalent to the
pre-amendment apply: the same lock, the same timeout, the same exit
code. The policy parameter only adds the two non-default acquisition
strategies the downstream SPECs require; it introduces no new
user-facing flag and no change to `patina apply` / `patina rollback`.

<done-when>
- The apply entry points accept a lock policy; invoking with the
  default (Blocking) policy acquires the exclusive lock with the
  REQ-023 sixty-second cap and is observably identical to the
  pre-amendment apply.
- Under the NonBlocking policy, an apply that finds the lock held by
  another holder returns a typed contention error and writes nothing
  to the filesystem (no `<ts>.plan`, no `<ts>.COMMIT`, no backup).
- Under the Held policy, an apply driven by a caller that already
  holds the exclusive guard performs no second lock acquisition and
  does not self-contend, completing as if it held the lock for the
  whole call.
- `patina apply` and `patina rollback` use the Blocking policy and
  still exit 4 when the exclusive lock cannot be acquired within the
  REQ-023 cap (REQ-022 / REQ-023 unchanged).
</done-when>

<behavior>
- Given a caller that drives the apply under the default Blocking
  policy, when no other holder contends, then the apply acquires and
  releases the exclusive lock exactly as the pre-amendment engine did.
- Given a second process holding the exclusive lock and a caller that
  drives the apply under the NonBlocking policy, when the apply runs,
  then it returns a typed contention error and the state directory's
  journal and backups are unchanged.
- Given a caller that has already acquired the exclusive guard and
  then drives the apply under the Held policy, when the apply runs,
  then it completes without attempting a second acquisition (no
  self-deadlock against its own held lock).
</behavior>

<scenario id="CHK-065">
Given a tempdir state directory whose `<state>/patina/lock` is held
exclusively by a test-controlled guard,
when an apply is driven under the NonBlocking policy,
then it returns the typed contention error and the
`<state>/patina/journal/` directory contains no new `<ts>.plan` or
`<ts>.COMMIT` file written by the contended attempt.
</scenario>

<scenario id="CHK-066">
Given a test that acquires the exclusive lock at `<state>/patina/lock`
and retains the guard,
when it drives an apply under the Held policy passing that guard,
then the apply completes successfully (it does not time out against
its own held lock) and the resulting `<ts>.COMMIT` record is present.
</scenario>

<scenario id="CHK-067">
Given a tempdir repository and an uncontended state directory,
when `patina apply --yes` runs (Blocking policy) and then a second
`patina apply --yes` runs against the unchanged source,
then both complete with exit code 0 and the second produces
byte-identical stdout to the first (REQ-021 determinism preserved
under the default policy).
</scenario>
</requirement>

## Decisions

<decision id="DEC-001">
The v1.0 work is split into three SPECs (engine + integration CLI;
complete CLI + Windows symlink elevation; watch mode + per-OS service
install + drift) rather than two or four. Three SPECs allows each
piece to ship and be reviewed independently while not over-slicing
the small Windows-permission surface (which lives within SPEC-0002).
The 2-SPEC alternative (bundle CLI + watch + Windows) was rejected
because SPEC-0002 would have bundled three semi-independent surfaces;
the 4-SPEC alternative (split Windows permission into its own SPEC)
was rejected because Windows-symlink work is under 10% of CLI surface
and creates artificial cross-SPEC coupling.
</decision>

<decision id="DEC-002">
`patina-core` is an async library using `tokio`. A sync library with
`rayon` for parallelism was considered (more portable, simpler
control flow) but rejected because future network I/O is anticipated
(remote repositories, telemetry, sync-to-server) and migrating a sync
library to async retroactively is a per-signature change across the
codebase. Paying the tokio cost upfront is cheaper than the future
migration; the spawn_blocking-based filesystem layer is acceptable
because OS-level async file I/O is not yet universally available
across the supported targets.
</decision>

<decision id="DEC-003">
Transactional apply uses a single-fsync upfront postcard journal
plus a per-operation progress cursor, with crash recovery by
filesystem probing. Alternatives considered and rejected:

- Filesystem snapshots (APFS/ZFS/Btrfs/VSS): platform-specific,
  ext4 has no native snapshot, Windows VSS requires admin. Violates
  the cross-platform safety prior.
- Per-operation fsync: 1000 ops × 5ms fsync = 5s overhead on top of
  actual filesystem work, three orders of magnitude slower than
  single-fsync.
- SQLite WAL as journal: heavy runtime dependency, no recovery
  advantage over a fsync'd append-only postcard log, harder to
  debug.

The single-fsync model trades simple journal-driven recovery for
probe-driven recovery (more code complexity in the recovery path)
in exchange for one fsync per apply rather than N. Recovery code
runs only on the crash path and tolerates the added complexity.
</decision>

<decision id="DEC-004">
The journal uses `postcard` binary encoding rather than JSON. Users
do not read journals; debug requirements are served by
`patina debug journal <path>`, which decodes the binary form for
support cases. JSON would add bytes and parse overhead with no
user-facing benefit; postcard's Rust-native, schema-evolution-aware
format pairs cleanly with the workspace's `serde` use.
</decision>

<decision id="DEC-005">
Watcher-CLI coordination uses an advisory file lock at
`<state>/patina/lock`, implemented via `fs2`. Alternative designs
considered and rejected:

- Mandatory OS-level lock: POSIX has no portable mandatory locking;
  Windows has mandatory locking but only at the byte-range level.
  Cross-platform parity requires advisory.
- Single-daemon serialization (CLI talks to long-running watcher
  via IPC): the watcher is optional in v1.0, so the CLI must work
  standalone, yielding two code paths plus an IPC protocol on top
  of the lock alternative. Net complexity exceeds the advisory-lock
  model.

`fs2` papers over `flock(2)` vs `LockFileEx` semantic differences;
if it proves insufficient during SPEC-0001 implementation, `fd-lock`
is the documented fallback (localized change).
</decision>

<decision id="DEC-006">
MiniJinja was chosen over Tera for templating. Both support Jinja2
syntax, strict-undefined behavior, and the expression language the
`when` clauses need. MiniJinja's advantages: smaller transitive
dependency footprint, more active upstream maintenance (April 2026
release cadence), and the same engine instance can serve both
`*.tmpl` rendering and `when` evaluation against a unified variable
context. Liquid and handlebars-rs were rejected because their
expression languages do not cleanly express `when` predicates like
`patina.os == "macos" and patina.hostname != "work"`.
</decision>

<decision id="DEC-007">
`patina apply` is TTY-driven: bare apply prompts in a terminal,
exits without mutation in a non-terminal pipe; `--yes` skips the
prompt. There is no `--dry-run` flag because the bare invocation in
a non-TTY shell is the dry-run path (it shows the diff and exits).
The three-state model (bare interactive / `--dry-run` / `--yes`) was
considered but rejected: `--dry-run` is fully subsumed by piping a
bare invocation through `cat` or any non-TTY consumer, and the
smaller flag set is easier to document and remember.
</decision>

<decision id="DEC-008">
Backup retention is by count (last ten applies). Alternatives
considered: time-bound (last 30 days), size-bound (under 1 GB),
hybrid (whichever applies first), manual via `patina gc`. The
count-based default is predictable, requires no user knowledge of
time-window or size semantics, and bounds disk usage proportional
to the number of recent applies. A user who runs ten quick applies
in one day loses yesterday's pre-Patina backup; this is documented
as a known limitation. A `patina gc` command with tuneable retention
is a v1.1 candidate.
</decision>

<decision id="DEC-009">
Hook failure handling uses a per-hook `must_succeed` field defaulting
to `true`. A `pre_apply` hook with `must_succeed = true` failure
aborts before any file operation; a `post_apply` hook with the same
attribute failure rolls back the file operations using the journal.
A `must_succeed = false` hook only warns on failure. The invocation
flag `--force-deploy` overrides every hook in the current apply to
`must_succeed = false`. Alternative models considered:

- Always rollback on post_apply failure (no opt-out): transient
  failures like a flaky `brew install` revert good deployments.
- Never auto-rollback (warn only): violates the "all-or-nothing"
  mental model.
- Per-event default (pre = false, post = true): asymmetric, easier
  to forget.

Per-hook explicit opt-out plus an invocation-level override
balances the safety story with the offline / transient-failure
escape hatch.
</decision>

<decision id="DEC-010">
The `--repo` global flag was dropped in favor of three discovery
sources: `PATINA_REPO` environment variable, walk-up from CWD, and
a persisted default in the state directory (set by `patina init`
in SPEC-0002). Test isolation uses the environment variable; user
ergonomics use the walk-up; explicit configuration uses the
persisted default. An explicit per-invocation `--repo` flag is
unnecessary clutter on the CLI surface and was deferred indefinitely.
</decision>

<decision id="DEC-011">
Crash recovery always restores the pre-apply state. The engine never
"finishes" a partial apply forward to the post-apply state. The
brainstorm's safety prior allowed convergence to either pre-apply or
post-apply; the engine chooses backward-only for these reasons:

- Backward recovery is simpler to implement and audit. The journal
  records inverse operations and backups that always work; forward
  recovery would require re-decoding the plan, re-resolving
  variables, and re-executing remaining ops, which multiplies the
  code paths on the crash path.
- Code complexity on the crash path is more dangerous than on the
  happy path because it is exercised less often. Choosing the
  smaller code path is a safety choice.
- A user who experiences a crash and re-runs `patina apply` after
  recovery completes still reaches the post-apply state in one more
  invocation. The cost is one extra apply cycle on the next run;
  the benefit is a much smaller recovery code path.
- The journal contents and backup retention scheme already exist
  for rollback (REQ-019); recovery reuses the same primitives.
</decision>

## Open Questions

All sixteen open questions raised during the brainstorm were resolved
before the SPEC was written. The self-review pass surfaced six
additional questions, all now resolved by user direction. Resolutions
recorded below; the corresponding SPEC content was updated in the
same revision.

- [x] a. **Built-in `patina.*` variables.** Drop `patina.repo_root`
  (no concrete v1.0 use case; can be added in v1.1 if templates
  demand it). Keep `patina.os`, `patina.arch`, `patina.hostname`,
  `patina.user`, `patina.home`, `patina.profile`. Add the dynamic
  map `patina.env.*` so `when` clauses and templates can reference
  process environment variables under the same reserved namespace.
- [x] b. **REQ-002 split.** Split into REQ-002 (async library +
  tokio wiring) and a new REQ-024 (no `unwrap` / `expect` / `panic!`
  / `unreachable!` / `todo!` / `unimplemented!` in production).
  Each is independently observable.
- [x] c. **Hook `cwd` / `env` fields.** Do not ship as schema
  fields. Instead, the `when` clause (MiniJinja context) exposes
  process env vars via `patina.env.VAR_NAME`, so OS-conditional
  hooks can match on the environment without growing the schema.
  Per-hook `cwd` is not in v1.0; commands needing a specific
  working directory can use `cd ... && cmd` shell syntax inside
  their `command` string.
- [x] d. **Default `[[file]]` mode.** Omitted `mode` defaults to
  `mode = "symlink"`. Per the symlink mode's behavior, a directory
  source under the default does the per-file walk (one symlink per
  file at mirrored target paths). Users wanting atomic directory
  symlinks must declare `mode = "symlink-dir"` explicitly.
- [x] e. **Recovery direction.** Backward-only recovery (always
  restore pre-apply state) is the engine's commitment. Captured as
  DEC-011 in this revision.
- [x] f. **`--json` mutation semantics.** `--json` alone never
  mutates the filesystem (treated as a preview). To apply via JSON
  output, pass `--json --yes`. The `result` field of the JSON
  document distinguishes `previewed` (no mutation), `applied`,
  `rolled_back` (post_apply must_succeed failure), or `aborted`
  (pre_apply must_succeed failure or other early exit).

## Changelog

<changelog>
| Date       | Author       | Summary |
|------------|--------------|---------|
| 2026-05-25 | human/kevin  | Initial draft after brainstorm. Locks the three-SPEC slicing (engine, complete CLI, watch), async patina-core with tokio, single-fsync postcard journal + progress cursor, advisory fs2 file lock, MiniJinja templating, TTY-driven apply semantics with `--yes` / `--force-deploy` overrides, count-based backup retention (last 10), `[[hook]]` schema with `must_succeed` default true, formalised exit codes 0-5. |
| 2026-05-25 | human/kevin  | Resolve six self-review questions. Drop `patina.repo_root` from built-ins; add `patina.env.*` map for env access. Split REQ-002 (async lib) from new REQ-024 (no-panic enforcement). Keep hook schema minimal; route OS/env conditionals through `when` clauses against `patina.env.*`. Default `[[file]]` mode is `symlink`. Add DEC-011 capturing backward-only recovery. `--json` alone never mutates; `--json --yes` mutates and reports result. |
| 2026-05-26 | human/kevin  | Add multi-target fan-out to `[[file]]` schema in REQ-005. Each entry declares exactly one of `target` (string) or `targets` (non-empty array of strings); both, neither, or `targets = []` are parse errors. All five modes support `targets`: the engine materializes the source at every listed target path according to the declared mode, recording one journal operation per (source, target_i) pair so per-target progress, status, backup, and rollback work without special-casing. Update REQ-018 to report each target as its own row with independent CLEAN/DRIFTED/MISSING/ORPHANED counters. Update REQ-019 to require atomic per-`[[file]]`-entry rollback: any target failure in the entry reverts all of the entry's targets as a unit. Add scenarios CHK-042..049 covering symlink, copy, template, parse-error, multi-target status, and multi-target rollback. |
| 2026-05-27 | human/kevin via assistant | Close five gaps surfaced by north-star audit against AGENTS.md (no prior `state="completed"` task is invalidated; all 21 existing tasks remain `pending`). Add REQ-025 (CI matrix gates merge on macOS / Linux / Windows; quality-bar parity rule operationalised). Add REQ-026 (`output::Reporter` trait with `HumanReporter` + `JsonReporter` impls; `clippy.toml` `disallowed-macros` denies `std::println` / `std::eprintln` / `std::print` / `std::eprint` outside the `output` module; `tracing` macros stay permitted). Add REQ-027 (`docs/ARCHITECTURE.md` + `docs/USER_GUIDE.md` with named structural anchors; cloud-sync paths-to-avoid list lives under `## State directory` as a markdown bullet list). Add REQ-028 (`deny.toml` at repo root with `[licenses]` / `[advisories]` / `[bans]` / `[sources]` tables; `cargo deny check` runs as a required-status-check on `push` and `pull_request`). Amend REQ-020 in place to name `patina debug` as a clap subcommand group — extension point for SPEC-0003's `debug drift-cache` — and add CHK-050 covering `patina debug --help`. New scenarios CHK-050..061 follow the existing numbering. No prior REQ semantics changed. |
| 2026-05-28 | human/kevin via assistant | Reconcile REQ-020 and REQ-011 with the file-operations-only on-disk plan format (surfaced by a blocking T-019 review). The serialized `Plan`/`PlannedOperation` model (T-011) records only file operations (symlink / render / copy with source + target); it carries no hooks, no resolved variable context, and no pre-state hash. REQ-020's `<done-when>` is narrowed to drop the "hooks, variable context" and "pre-state hash where applicable" rendering promises (`debug journal` now prints recorded operations + timestamps, identifying each op's mode / source / target); REQ-011's prose drops the stray "and hook invocations" (its own `<done-when>` only ever committed to file operations). No `<scenario>`, `<behavior>`, `<done-when>` test surface changed beyond the two narrowed REQ-020 bullets; CHK-034 and CHK-050 are untouched. Cross-spec verified: SPEC-0002/0003 read no hooks/variable-context/pre-state-hash from the plan — pre-state/expected hashes live in the COMMIT/backup record (REQ-017, rollback) and the SPEC-0003 drift-cache, and SPEC-0003 references `debug journal` only as a structural template for `debug drift-cache`. T-019 stays `pending`; the SPEC-drift blocker is resolved by this amendment, leaving only the style fix for the next implementer pass. No prior `state="completed"` task is invalidated. |
| 2026-05-29 | human/kevin via assistant | Add REQ-029 to close cross-SPEC gaps the 2026-05-28 file-ops-only amendment left in the COMMIT record (caught while prepping SPEC-0002/0003 decomposition; the SPEC-0001 PR is not yet merged). The committed `ApplyRecord` now retains, per target, the canonical source path and — for content targets — a 32-byte `blake3` content hash (was a `u64` `std::hash` fingerprint with no source); the shared version-envelope major bumps `1`→`2`. Rationale: SPEC-0003 drift detection (REQ-007) compares `blake3` hashes and the SPEC-0003 watcher (REQ-005) needs per-target source paths from the committed journal because the `.plan` is deleted at commit; SPEC-0002 `remove`/`promote` (REQ-003/REQ-004) need the target→source map. Tiny clarifying cross-references added to REQ-011 (the COMMIT sentinel shares the envelope) and REQ-018 (the expected hash is the `blake3` content hash). New task T-026 implements the record widening end-to-end (type + write side + read side + `blake3` dependency + version bump) as one atomic, CI-green change; T-010 and T-017 stay `completed` (their REQ-011/012 and REQ-018 semantics are unchanged — T-026's review covers the files it touches). REPORT.md deleted and status flipped `implemented`→`in-progress`; speccy-ship re-runs once T-026 lands. |
| 2026-05-30 | human/kevin via assistant | Add REQ-030 (engine apply-path lock-acquisition policy) to unblock SPEC-0002 and SPEC-0003, which a post-ship cross-SPEC review found cannot be implemented against the shipped engine. `execute_plan` (`apply/engine.rs`) and `run_rollback` (`rollback/mod.rs`) self-acquire the exclusive lock with `exclusive_timeout()` (60s blocking poll) and expose no non-blocking path and no apply-while-holding-lock path. Consequences: the SPEC-0003 watcher (REQ-006/REQ-008/CHK-013) cannot attempt the lock non-blocking and skip on contention — it would block ≤60s behind the CLI, and pre-acquiring then calling `apply()` self-deadlocks on a second `flock`/`LockFileEx` from the same process; and SPEC-0002 `remove`/`promote` (REQ-009) cannot mutate under the exclusive lock and then re-journal via re-apply without self-contending. REQ-030 adds a `LockPolicy` to the apply entry points with three variants — Blocking (default; byte-for-byte the pre-amendment behaviour, what `patina apply`/`rollback` use), NonBlocking (single attempt, typed contention error, zero filesystem mutation), and Held (caller supplies the acquired `LockGuard`; engine does not re-acquire). No new user-facing flag; `patina apply`/`rollback` behaviour and REQ-022/REQ-023 are unchanged. One clarifying cross-reference added to REQ-023 prose pointing at REQ-030; new scenarios CHK-065..067 follow the existing numbering. Since SPEC-0001 already shipped, this amendment reopens it on a fresh branch/PR: REPORT.md and `journal/VET.md` deleted, status flipped `implemented`→`in-progress`; new task T-027 implements the policy end-to-end (engine signature + the three variants + watcher/CLI call-site wiring stays in the downstream SPECs). No prior `state="completed"` task is invalidated — the default-policy path preserves existing apply/rollback behaviour. |
</changelog>

## Notes

### Rejected whole-shape framings

The following alternative whole-shape framings were considered during
brainstorming and rejected with the reasons recorded.

- **2-SPEC bundle (CLI + watch together).** SPEC-0002 would contain
  the complete CLI plus watch plus Windows permission flow.
  Rejected: SPEC-0002 too large; three semi-independent surfaces
  violate the "individually shippable + reviewable" constraint.
  Captured in DEC-001.

- **4-SPEC split (Windows permission its own SPEC).** Rejected:
  Windows symlink permission surface is under 10% of complete-CLI
  surface; standalone SPEC creates artificial cross-SPEC coupling
  for a small slice that lives inside `doctor` and `apply`.
  Captured in DEC-001.

- **Patina-as-stow (zero setup ownership).** Rejected: the
  `[[hook]]` schema's footprint (~100-200 LOC) earns its keep by
  satisfying the "clone repo, one command, environment matches"
  user story without growing a setup DSL. Captured in REQ-006 and
  the absence of a `[[step]]` schema.

- **Sync `patina-core` + rayon for parallelism.** Rejected: future
  network I/O is anticipated; sync-to-async migration is per-signature
  work across the codebase. Tokio's spawn_blocking-based FS layer
  achieves the same parallel throughput sync+rayon would. Captured
  in DEC-002.

- **Watcher-as-daemon with IPC-based serialization.** Rejected:
  watcher is optional in v1.0, so the CLI must work standalone.
  Captured in DEC-005.

### Cross-SPEC handoffs

SPEC-0001 establishes infrastructure that SPEC-0002 and SPEC-0003
build on:

- SPEC-0002 adds `patina init`, `add`, `remove`, `promote`, `doctor`
  and the Windows symlink Developer Mode prompt-and-elevate flow.
  The `patina-elevate.exe` helper binary is introduced there.
- SPEC-0003 adds `patina watch` with subscription policy (per-journal-entry
  only), debounce, per-OS service install, drift detection (hash-compare
  + desktop notification), and Windows `ERROR_SHARING_VIOLATION`
  retry-with-backoff.

The advisory file lock at `<state>/patina/lock` defined in this SPEC
is the coordination point for SPEC-0003 watcher-CLI serialization.
The journal format defined here is the same format SPEC-0003's
watcher reads to determine subscription paths.

### Tooling assumptions

The repository's `clippy.toml` already configures
`allow-expect-in-tests = true`. CI matrix runs `cargo clippy
--workspace --all-targets --locked -- -D warnings` on macOS, Linux,
and Windows. `cargo-deny` is configured (per the existing `deny.toml`)
and any new dependency added by this SPEC must pass license,
advisory, and bloat checks before merge.
