---
id: SPEC-0002
slug: patina-complete-cli
title: Patina complete CLI surface and Windows symlink elevation
status: implemented
created: 2026-05-25
supersedes: []
---

# SPEC-0002: Patina complete CLI surface and Windows symlink elevation

## Summary

SPEC-0001 ships the engine plus the three-command integration CLI
surface (`apply`, `status`, `rollback`, `debug journal`). SPEC-0002
layers the user-facing CLI commands that humans actually reach for
during day-to-day use: `init` to scaffold a new dotfiles repository,
`add` to register an existing dotfile without writing TOML by hand,
`remove` to unmanage a target while leaving the system functional,
`promote` to update a source file from an externally-edited copy-mode
target, and `doctor` to surface environment problems that would
otherwise produce surprising apply failures (repository on a UNC
path, Developer Mode missing on Windows, running on a too-old
Windows build, no persisted `default_repo` pointer). Cloud-sync
mounts are explicitly out of scope for v1.0 detection — the project
docs (`docs/USER_GUIDE.md`) include a user-facing
callout about keeping the state directory off iCloud Drive,
OneDrive, Dropbox, etc.

This SPEC also lands the **Windows symbolic link permission flow**.
Creating symbolic links on Windows requires either Developer Mode
enabled (a machine-level registry flag) or administrator privileges.
Running the main `patina.exe` process elevated would mean a UAC prompt
on every apply, which is unacceptable user experience. Instead, on
the first apply whose plan contains symbolic link operations, Patina
detects whether Developer Mode is enabled. If it is not, the user is
prompted to enable it via a single one-time UAC elevation that
launches a bundled helper binary (`patina-elevate.exe`). The helper
toggles the registry key and exits. The main process never runs
elevated; all subsequent applies proceed without UAC prompts because
Developer Mode is a machine-level setting.

The complete CLI commands are built on the engine and lock primitives
established in SPEC-0001. The Windows elevation flow uses the
`winsafe` crate (`taskschd`, `shell`, `advapi` features). Cross-platform
parity is preserved: macOS and Linux never invoke the elevation flow;
their builds do not link `patina-elevate.exe` and their releases ship
only the `patina` binary.

## Goals

<goals>
- A user with a fresh clone of an empty directory runs
  `patina init` and a root `patina.toml` is created with `root = true`,
  the per-machine state directory is populated with a persisted
  default repository pointer, and the user sees clear next-step
  instructions.
- A user with an existing dotfile (for example `~/.zshrc`) runs
  `patina add ~/.zshrc` and Patina places it under a module
  (creating the module subdirectory if needed), writes a `[[file]]`
  entry, and either prompts for the mode or honors a
  `--symlink` / `--copy` / `--template` override.
- A user runs `patina remove ~/.zshrc` and the symlink at
  `~/.zshrc` is replaced with a regular file containing the
  last-applied content (the system stays functional); the
  `[[file]]` entry is removed from the module's `patina.toml`.
- A user runs `patina promote ~/.gitconfig` after editing the
  deployed file directly and the corresponding repository source is
  updated to match the deployed target.
- A user runs `patina doctor` and sees warnings for any of:
  repository on a Windows UNC path, missing Developer Mode on
  Windows when the repository declares any symbolic link `[[file]]`
  entries, OS too old to support Developer Mode, missing
  `default_repo` pointer.
- On Windows, a user running `patina apply --yes` whose plan
  contains symbolic links and whose machine does not have Developer
  Mode enabled sees a single one-time UAC prompt; accepting it
  toggles the registry key via `patina-elevate.exe` and the apply
  completes; subsequent applies run without any prompt.
</goals>

## Non-goals

<non-goals>
- No `patina watch` subcommands, per-OS service install, drift
  detection, drift notification, or Windows
  `ERROR_SHARING_VIOLATION` retry-with-backoff. Those land in
  SPEC-0003.
- No automatic Developer Mode toggling without user consent. The
  UAC prompt is the consent gate; refusing it produces a clear
  error.
- No `SeCreateSymbolicLinkPrivilege` per-user grant pathway. The
  Developer Mode registry flag is the only supported route on
  Windows.
- No support for Windows versions older than 10 1703 (April 2017).
  Older versions lack the Developer Mode registry surface; doctor
  emits a warning and the symbolic link mode is unusable on such
  hosts.
- No `patina-elevate.exe` code signing in v1.0. The UAC dialog
  shows a yellow shield (unsigned publisher); code signing is a
  later release pipeline concern.
- No automatic migration of dotfiles from other managers (chezmoi,
  yadm, dotter, stow). Users hand-curate their repository.
- No `patina gc` command. Backup retention remains the engine's
  hardcoded last-ten-applies policy from SPEC-0001.
- No promotion of template-rendered targets. `patina promote`
  refuses on targets whose source was a `*.tmpl` file because
  templating is non-invertible.
- No `doctor --auto-fix` that runs without prompts. The `--fix`
  flag still prompts before each destructive action.
- No cloud-sync provider detection in `patina doctor`. SPEC-0001's
  assumption that users do not place the per-machine state
  directory on iCloud Drive / OneDrive / Dropbox / Box / Google
  Drive / Syncthing stands; the project docs include a callout
  (`docs/USER_GUIDE.md`) explaining the risk. Active
  detection — hardcoded provider list or otherwise — is a v1.1
  candidate.
- No `--linger` flag on `patina watch install` in v1.0 (defined in
  SPEC-0003). Linux users who want the watcher to survive logout
  run `sudo loginctl enable-linger $USER` manually; the docs
  include the snippet.
- No `windows_dev_mode.cache` file. The Developer Mode registry
  flag is re-read on each apply that requires it (registry reads
  are microsecond-scale; the cache invalidation surface earns no
  net win).
- No `--symlink-dir` or `--copy-tree` mode flag on `patina add`. In
  v1.0 `patina add` offers exactly `--symlink`, `--copy`, and
  `--template` (REQ-002). The two directory-oriented engine modes
  from SPEC-0001 REQ-005 (`symlink-dir`, `copy-tree`) stay reachable
  by hand-editing the module `patina.toml`, but `add` does not
  generate them; exposing them as `add` flags is a v1.1 candidate.
- No release / packaging pipeline. SPEC-0002 specifies which binaries
  each OS's release artifact contains (`patina` everywhere;
  `patina-elevate.exe` only on Windows — REQ-008), but the mechanics
  of building, code-signing, and distributing those artifacts
  (`cargo install`, Homebrew, MSI, etc.) are owned by the release
  tooling, not a v1.0 feature requirement here.
</non-goals>

## User stories

<user-stories>
- As a user setting up Patina for the first time, I want
  `patina init` to scaffold a sensible `patina.toml` so I can start
  registering dotfiles without learning the TOML schema cold.
- As a user with a pre-existing `~/.zshrc` I want to bring under
  management, I want `patina add ~/.zshrc` to copy my file into a
  sensible module directory, write the right `[[file]]` entry, and
  leave my system in working order so that `patina apply` would be
  a no-op.
- As a user who decided not to use Patina for `~/.gitconfig`
  anymore, I want `patina remove ~/.gitconfig` to remove the
  `[[file]]` entry from the TOML and leave a regular file at
  `~/.gitconfig` so my git commands continue to work.
- As a user who edited my deployed `~/.config/foo.conf` directly
  during debugging, I want `patina promote ~/.config/foo.conf` to
  copy that edit back into the source file in my repository so the
  next apply re-installs the edit rather than reverting it.
- As a user setting up Patina on a fresh machine, I want
  `patina doctor` to tell me about misconfigurations (repository
  on a UNC path on Windows, OS-too-old, missing Developer Mode)
  so I can fix them before they cause apply-time surprises.
- As a Windows user whose dotfiles include a symbolic link entry, I
  want Patina to detect that I need Developer Mode and offer to
  enable it via a single UAC prompt, rather than failing my apply
  or asking me to run Patina as administrator.
</user-stories>

## Assumptions

<assumptions>
- The `winsafe` crate's `taskschd`, `shell`, and `advapi` features
  cover the Windows APIs SPEC-0002 needs: registry read/write
  (Developer Mode flag), `ShellExecuteEx` (UAC elevation helper
  launch), and basic Win32 metadata queries (OS version). The
  brainstorm verified `taskschd` coverage for SPEC-0003's service
  install; the same crate suffices here. If a gap surfaces during
  implementation, the documented fallback is to swap in
  `windows`+`winreg`+`planif` for the affected module.
- The Windows Developer Mode registry key
  (`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock\AllowDevelopmentWithoutDevLicense`)
  is stable across Windows 10 1703 through Windows 11. Microsoft has
  not documented a deprecation; if they do, SPEC-0002 will need an
  amendment.
- `ShellExecuteEx` with the `runas` verb produces the standard
  Windows UAC prompt and returns an HRESULT the caller can check
  for the canonical "user declined" pattern (`ERROR_CANCELLED` or
  similar). This is documented behavior since Windows Vista.
- The user has machine administrator rights on their own machine.
  On corporate / domain-locked-down machines where Developer Mode
  is policy-disabled, the UAC prompt will succeed but the registry
  write will fail with `ERROR_ACCESS_DENIED`; doctor surfaces this
  case as "Developer Mode is policy-locked; contact IT".
- The persisted-default repository pointer in the per-machine state
  directory is written as plain text (one absolute path per line,
  UTF-8). No schema versioning is needed because the file is
  Patina-internal and read by Patina-internal code; if its shape
  changes between Patina versions, the upgrade pathway is to
  re-run `patina init` (which rewrites the file).
- `patina init` may be invoked in a directory that is not a git
  repository. SPEC-0002 does not require the dotfiles repository to
  be a git repository, although in practice users will almost
  always make it one. Doctor surfaces a helpful suggestion to run
  `git init` if the directory has no `.git/`, but does not refuse.
</assumptions>

## Requirements

<requirement id="REQ-001">
### REQ-001: `patina init` scaffolds a root `patina.toml` and persists the default repository pointer

The `patina init` subcommand creates a root `patina.toml` file
declaring `root = true` at the current working directory (or at a
path passed as a positional argument), writes the absolute canonical
path of that directory to the per-machine state directory's
`default_repo` file, and prints a next-step hint pointing at
`patina add`. If a `patina.toml` already exists at the target path,
the command refuses with a typed error and exits 1.

<done-when>
- `patina init` in an empty directory creates a `patina.toml` whose
  parsed content includes `[patina]` and `root = true`.
- The created file's `[patina]` table contains at least a
  `created_at` field carrying an RFC 3339 timestamp string (note:
  this is the only place in user-facing output where a timestamp is
  permitted, because the file is configuration, not stdout).
- The per-machine state directory's `default_repo` file contains
  the absolute canonical path of the initialized directory after
  the command exits 0.
- `patina init` against a directory containing an existing
  `patina.toml` exits 1 with a typed error naming the existing
  file path.
- `patina init <path>` creates the `patina.toml` at `<path>`
  (creating the directory if necessary) and uses that path as the
  persisted default.
- The command's stdout ends with a single-line hint of the form
  `Next: run \`patina add <path>\` to register an existing dotfile.`
</done-when>

<behavior>
- Given an empty directory `/tmp/dot`, when `patina init /tmp/dot`
  runs, then `/tmp/dot/patina.toml` exists with `root = true`, the
  state directory's `default_repo` file contains `/tmp/dot`, and
  exit code is 0.
- Given a directory already containing `patina.toml`, when
  `patina init` runs, then the existing file is untouched, the
  state directory is untouched, and exit code is 1.
</behavior>

<scenario id="CHK-001">
Given an empty tempdir `T` and a clean state directory,
when `patina init T` runs,
then `T/patina.toml` exists, its parsed content's `[patina]` table
contains `root = true`, the state directory's `default_repo` file
contains the canonical absolute path of `T`, and the process
exits 0.
</scenario>

<scenario id="CHK-002">
Given a tempdir `T` containing a `patina.toml` with `root = true`,
when `patina init T` runs,
then `T/patina.toml` is unchanged (byte-identical to before),
stderr contains the substring `already exists` and the path of
`T/patina.toml`, and exit code is 1.
</scenario>
</requirement>

<requirement id="REQ-002">
### REQ-002: `patina add <path>` registers an existing dotfile under a module

The `patina add <path>` subcommand takes an absolute or
HOME-relative path on the local filesystem, determines the
appropriate module subdirectory inside the dotfiles repository
(prompting the user when ambiguous, or accepting `--module <name>`
to skip the prompt), copies the file into that module's directory,
writes a `[[file]]` entry into the module's `patina.toml` (creating
the module's `patina.toml` if it does not yet exist), and leaves
the original target path in a state that subsequent `patina apply`
would converge from.

<done-when>
- `patina add ~/.zshrc --module zsh --symlink` copies `~/.zshrc`
  into `<repo>/zsh/zshrc`, creates `<repo>/zsh/patina.toml` if
  absent, and appends a `[[file]]` entry with
  `source = "zshrc"`, `target = "~/.zshrc"`, `mode = "symlink"`
  to that file.
- After `patina add ~/.zshrc --module zsh --symlink`, running
  `patina apply --yes` materializes `~/.zshrc` as a symbolic link
  pointing back into the repository (the system is functional).
- `patina add <path>` without `--module` prompts the user for a
  module name; in a non-TTY shell it exits 1 with a typed error
  naming the missing `--module` argument.
- `patina add <path>` accepts the mode override flags
  `--symlink`, `--copy`, `--template`, exactly one of which may
  be set per invocation.
- `patina add <path>` without any mode override prompts the user
  in a TTY for the mode and exits 1 in a non-TTY.
- `patina add` against a path that is already managed exits 1
  with a typed error naming the existing `[[file]]` entry and
  the source module.
</done-when>

<behavior>
- Given a pre-existing `~/.zshrc` and a repository whose root
  `patina.toml` exists but has no module subdirectories, when
  `patina add ~/.zshrc --module zsh --symlink` runs, then
  `~/.zshrc` is copied into the repository at `<repo>/zsh/zshrc`,
  a new file `<repo>/zsh/patina.toml` is created containing the
  `[[file]]` entry, and `~/.zshrc` is left as a regular file
  containing the original bytes (apply has not yet run).
- Given the same scenario followed by `patina apply --yes`,
  when both commands complete, then `~/.zshrc` is a symbolic
  link to `<repo>/zsh/zshrc`.
- Given `patina add ~/.zshrc --symlink --copy` (two mode flags),
  when the parser runs, then exit code is 2 (clap usage error)
  and stderr names the conflicting flags.
</behavior>

<scenario id="CHK-003">
Given a tempdir HOME containing `~/.zshrc` with content "foo" and
a tempdir repository with only a root `patina.toml`,
when `patina add ~/.zshrc --module zsh --symlink` runs,
then `<repo>/zsh/zshrc` is a regular file with content "foo",
`<repo>/zsh/patina.toml` exists and contains a `[[file]]` entry
with `source = "zshrc"`, `target = "~/.zshrc"`,
`mode = "symlink"`, and `~/.zshrc` is a regular file with
content "foo" (apply has not yet been invoked).
</scenario>

<scenario id="CHK-004">
Given the post-state of CHK-003,
when `patina apply --yes` runs,
then `~/.zshrc` is a symbolic link whose readlink target equals
the canonical path of `<repo>/zsh/zshrc`.
</scenario>
</requirement>

<requirement id="REQ-003">
### REQ-003: `patina remove <path>` unmanages a target; `--purge` deletes it

The `patina remove <path>` subcommand removes the `[[file]]` entry
for the named target from its module's `patina.toml` and replaces
the target on disk with a regular file containing the last-applied
content (so the user's system remains functional). With `--purge`,
the target file is also deleted from the system entirely. After
mutating the repository and the target, `remove` re-journals the new
managed set: it writes a fresh `<ts>.COMMIT` apply record (SPEC-0001
REQ-029) that no longer lists the removed target, so `patina status`
treats the path as deliberately unmanaged (absent from the report)
rather than as an ORPHANED leftover. ORPHANED, per SPEC-0001
REQ-018, is reserved for a target a user dropped *implicitly* — by
hand-editing `patina.toml` or deleting a source file — without
running `remove`; the fresh COMMIT is what keeps the explicit-remove
path out of that bucket. This mirrors `patina promote`, which also
re-journals after mutating (REQ-004). If removing the entry leaves
the module's `patina.toml` with no `[[file]]` or `[[hook]]` entries,
the empty `patina.toml` is left in place; the empty module directory
is the user's call to clean up (Patina does not auto-delete user
files).

<done-when>
- `patina remove ~/.zshrc` (without `--purge`) removes the
  `[[file]]` entry from the relevant module's `patina.toml` and
  replaces the symbolic link or rendered file at `~/.zshrc` with
  a regular file whose content equals the last-applied content.
- `patina remove ~/.zshrc --purge` removes the entry AND deletes
  the file at `~/.zshrc` entirely.
- `patina remove <path>` against a path that is not currently
  managed exits 1 with a typed error naming the path and the
  three discovery sources (env, walk-up, persisted default).
- `patina remove ~/.zshrc` writes a fresh `<ts>.COMMIT` apply record
  that omits `~/.zshrc`; consequently `patina status` no longer lists
  `~/.zshrc` (it is unmanaged — absent from the report — not
  ORPHANED), and a subsequent `patina apply --yes` is a no-op for
  `~/.zshrc`.
- For copy-mode targets, the "last-applied content" is the bytes of
  the journaled source — the canonical source path recorded per
  SPEC-0001 REQ-029 — which the engine reads from the repository at
  remove time.
- For template-rendered targets, the "last-applied content" is
  produced by re-rendering that journaled source through MiniJinja
  against the variable context resolved at remove time. The engine
  does NOT recover raw rendered bytes from the journal, which records
  only a blake3 hash of them (SPEC-0001 REQ-029). If the resolved
  variable context changed since the last apply the re-rendered bytes
  may differ from the last-applied bytes; remove leaves the freshly
  re-rendered output — the deliberate "reset to current source
  intent" semantics (DEC-005).
- The original source file in the repository is NOT deleted by
  `patina remove` (purge or not); the user reclaims the
  repository file manually if desired.
</done-when>

<behavior>
- Given a managed `~/.zshrc` (symlink to `<repo>/zsh/zshrc`),
  when `patina remove ~/.zshrc` runs, then `~/.zshrc` is a
  regular file containing the content of `<repo>/zsh/zshrc` at
  remove time, and the `[[file]]` entry is gone from
  `<repo>/zsh/patina.toml`.
- Given the same managed `~/.zshrc`, when
  `patina remove ~/.zshrc --purge` runs, then `~/.zshrc` does
  not exist on disk, and the `[[file]]` entry is gone.
- Given an unmanaged path `~/.bashrc`, when
  `patina remove ~/.bashrc` runs, then the file is unchanged,
  no TOML is mutated, and exit code is 1.
</behavior>

<scenario id="CHK-005">
Given a tempdir HOME and repository, an applied symbolic link at
`~/.zshrc` pointing to `<repo>/zsh/zshrc` (content "shell-config"),
when `patina remove ~/.zshrc --yes` runs,
then `~/.zshrc` is a regular file with content "shell-config",
`<repo>/zsh/patina.toml` no longer contains a `[[file]]` entry
for `~/.zshrc`, `<repo>/zsh/zshrc` is unchanged, and a subsequent
`patina status --json` does not list `~/.zshrc` in its `files`
array (the fresh COMMIT omitted it, so it is unmanaged rather than
ORPHANED).
</scenario>

<scenario id="CHK-006">
Given the same applied symbolic link,
when `patina remove ~/.zshrc --purge --yes` runs,
then `~/.zshrc` does not exist on disk, the entry is removed from
the TOML, and the repository source `<repo>/zsh/zshrc` is
unchanged.
</scenario>
</requirement>

<requirement id="REQ-004">
### REQ-004: `patina promote <target>` updates the source from a drifted copy-mode target

The `patina promote <target>` subcommand reads the content of the
deployed target on disk and writes it back to the corresponding
source file in the repository, then re-applies (so the journal
records the new content as the expected hash). The command refuses
to operate on template-rendered targets (their sources are
`.tmpl` files and templating is not invertible) and on
symbolic-link-mode targets (the target IS the source for those, so
the operation is meaningless). Promote is the explicit answer to
"a copy-mode target drifted; I want to keep my edit, not lose it
on the next apply".

<done-when>
- `patina promote <target>` on a copy-mode managed target reads
  the current bytes of the target file and writes them to the
  source file in the repository.
- After a successful promote, `patina status` reports the target
  as CLEAN against the new source content.
- `patina promote` on a target that maps to a `.tmpl` source
  refuses with a typed error naming the template path and the
  reason ("templating is non-invertible").
- `patina promote` on a symbolic-link-mode target refuses with a
  typed error naming the target and the reason ("symlink targets
  share content with their source; promotion is meaningless").
- `patina promote` on a `copy-tree` target operates on the
  individual file within the tree, not the whole tree; the entry
  must name a leaf file.
- `patina promote` is a mutating command and acquires the engine's
  exclusive file lock at `<state>/patina/lock`.
</done-when>

<behavior>
- Given a managed `~/.gitconfig` in `copy` mode (last-applied
  content "X") that the user edited externally to content "Y",
  when `patina promote ~/.gitconfig` runs, then the source file
  in the repository contains "Y" and the engine writes a new
  journal record marking "Y" as the expected hash.
- Given a managed `~/.gitconfig` whose source was `gitconfig.tmpl`,
  when `patina promote ~/.gitconfig` runs, then no file is mutated
  and exit code is 1 with stderr naming `gitconfig.tmpl` and the
  word `template`.
</behavior>

<scenario id="CHK-007">
Given a tempdir repository with `<repo>/git/gitconfig` (content
"[user]\nemail = old@example.com"), an applied copy-mode target at
`~/.gitconfig` (same content), and a test that overwrites
`~/.gitconfig` with "[user]\nemail = new@example.com",
when `patina promote ~/.gitconfig --yes` runs,
then `<repo>/git/gitconfig` contains
"[user]\nemail = new@example.com", and the most recent journal
record's expected hash for `~/.gitconfig` matches the blake3 hash of
the new content (SPEC-0001 REQ-029).
</scenario>

<scenario id="CHK-008">
Given a tempdir repository declaring
`source = "gitconfig.tmpl" target = "~/.gitconfig"` and an applied
target containing rendered output,
when `patina promote ~/.gitconfig` runs,
then no file is mutated, stderr contains `gitconfig.tmpl` and the
substring `template`, and exit code is 1.
</scenario>
</requirement>

<requirement id="REQ-005">
### REQ-005: `patina doctor` warns on UNC paths, missing Developer Mode, OS-too-old, and stale state

The `patina doctor` subcommand inspects the per-machine state
directory, the resolved repository path, the running OS, and the
declared file modes in the repository. It emits warnings to stderr
for any of the following conditions, then exits 0 if only warnings
were found or 1 if any error-level finding was raised. The
findings list is exhaustively specified here; future findings
require a SPEC amendment.

Cloud-sync directory detection is explicitly out of scope for v1.0
(see non-goals); the project docs (`docs/USER_GUIDE.md`)
include a user-facing callout.

<done-when>
- Doctor emits a warning on Windows when the resolved repository
  path is a UNC path (starts with `\\` after canonicalization).
- Doctor emits a warning on Windows when the repository's modules
  declare any `[[file]]` entry with `mode = "symlink"` or
  `mode = "symlink-dir"` AND Developer Mode is not currently
  enabled (the registry flag is absent or set to 0).
- Doctor emits a warning on Windows when the running OS build is
  older than Windows 10 1703 (the version that introduced
  Developer Mode).
- Doctor emits an info-level note (not a warning) when no
  `default_repo` file exists in the state directory; the
  suggested fix is `patina init`.
- Doctor exits 0 when only warning-level findings were emitted.
- Doctor exits 1 only when an error-level finding was emitted
  (the v1.0 set has no error-level findings; the exit-1 path is
  reserved for future additions).
- `patina doctor --json` emits a structured JSON document with a
  `findings` array of objects `{code, level, message, path?}`.
</done-when>

<behavior>
- Given a Windows host whose repository path resolves to
  `\\fileserver\share\dotfiles`, when `patina doctor` runs, then
  stderr contains a warning naming the path and the substring
  `UNC`, and exit code is 0.
- Given a Windows host where the repository declares any symbolic
  link `[[file]]` and Developer Mode is off, when
  `patina doctor` runs, then stderr contains a warning naming
  `Developer Mode` and the registry key path.
</behavior>

<scenario id="CHK-010">
Given a Windows test host where Developer Mode is OFF (registry
value `AllowDevelopmentWithoutDevLicense` is `0` or absent) and a
test repository declaring at least one
`[[file]] mode = "symlink"` entry,
when `patina doctor --json` runs,
then the JSON output contains a finding with
`code = DOC-WIN-DEVMODE`, `level = warning`, and a `message`
naming `Developer Mode` and the registry path.
</scenario>
</requirement>

<requirement id="REQ-006">
### REQ-006: `patina doctor --fix` interactively offers to remediate fixable findings

With the `--fix` flag, `patina doctor` enumerates each finding for
which Patina knows a remediation, prompts the user for confirmation
on each, and performs the remediation on accept. The fixable
findings in v1.0 are limited to Developer Mode missing on Windows
(remedied by launching the UAC elevation flow defined in REQ-007)
and the absence of a `default_repo` pointer (remedied by writing
the current working directory, or by re-running `patina init`).
Non-fixable findings (UNC paths, OS-too-old) are listed with a
brief explanation of why Patina cannot remedy them.

<done-when>
- `patina doctor --fix` on Windows with Developer Mode off
  prompts the user; on `y`/`Y` it launches the UAC elevation
  flow (REQ-007) and re-checks the registry afterward.
- `patina doctor --fix` with no `default_repo` prompts to write
  the current working directory's canonical absolute path; on
  accept, the file is created.
- `patina doctor --fix` in a non-TTY shell exits 1 with a typed
  error naming the missing `--yes` flag (no per-finding prompt
  is possible without TTY).
- `patina doctor --fix --yes` accepts every prompt automatically
  for fixable findings; non-fixable findings still emit
  warnings.
- Each fix that runs writes a structured trace event via
  `tracing` recording the finding code, the remediation chosen,
  and the outcome.
</done-when>

<behavior>
- Given a Windows host with Developer Mode off and a repository
  with symbolic link entries, when `patina doctor --fix` runs
  interactively and the user accepts the prompt, then Patina
  spawns `patina-elevate.exe` via UAC, the registry key is set
  to 1, and a subsequent `patina doctor` reports no warning
  for Developer Mode.
- Given a clean state directory with no `default_repo` and the
  user's CWD pointing at a valid Patina repository, when
  `patina doctor --fix --yes` runs, then `default_repo` is
  written and contains the CWD's canonical absolute path.
</behavior>

<scenario id="CHK-011">
Given a Windows test host with Developer Mode OFF, a repository
with a symlink `[[file]]`, and a TTY test harness that auto-types
`y` on the first prompt,
when `patina doctor --fix` runs,
then the registry value
`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock\AllowDevelopmentWithoutDevLicense`
is set to 1 after the command completes and the command exits 0.
</scenario>
</requirement>

<requirement id="REQ-007">
### REQ-007: Windows Developer Mode detection and one-time UAC helper flow

On Windows hosts, when the engine begins an apply whose plan
contains any operation with `mode = "symlink"` or
`mode = "symlink-dir"`, the engine reads the Developer Mode
registry flag. If the flag is missing or set to 0, the engine
emits an interactive prompt asking the user to enable Developer
Mode via a one-time UAC elevation. On accept, the engine spawns
the bundled `patina-elevate.exe` helper via `ShellExecuteEx` with
the `runas` verb. On user-accepted UAC, the helper sets the
registry flag to 1, exits 0, and the engine re-reads the flag and
proceeds with the apply. On declined UAC or any other failure, the
engine emits a clear typed error and exits with code 5 (user
declined / cancelled). The main `patina.exe` process never runs
elevated.

<done-when>
- On macOS and Linux, no Developer Mode check runs and no helper
  binary is invoked.
- On Windows with Developer Mode already enabled, the apply
  proceeds without any prompt or helper invocation.
- On Windows with Developer Mode disabled and a plan containing
  zero symbolic link operations, the apply proceeds without any
  prompt (no symbolic links to create).
- On Windows with Developer Mode disabled and a plan containing
  any symbolic link operation, the engine prompts before any
  filesystem mutation occurs.
- On user-accepted UAC, the registry value is read again after
  helper exit; if the value is 1, the apply proceeds; otherwise
  the engine emits a typed error naming the registry path and
  exits 1.
- On user-declined UAC, the engine emits a typed error whose
  message names "Developer Mode" and "patina doctor
  --fix", and exits 5.
- The Developer Mode registry flag is read on every apply that
  contains symbolic link operations. There is no cache file; the
  registry read is microsecond-scale and the cache invalidation
  surface (TTL math, external toggles, file format) earns no
  offsetting win.
- The Developer Mode prompt is suppressed entirely when the
  invoking process is already running elevated (the engine
  detects this via the process token); a separate warning fires
  recommending that the user avoid running Patina elevated.
</done-when>

<behavior>
- Given a Windows host with Developer Mode off and a repository
  whose only symbolic link entry was added today, when
  `patina apply --yes` runs in a TTY and the user accepts the
  UAC dialog, then the helper toggles the registry, the engine
  re-reads, and the apply completes; subsequent
  `patina apply --yes` invocations run without any prompt.
- Given the same host, when `patina apply --yes` runs and the
  user clicks "No" on the UAC dialog, then no file operation
  occurs, stderr names `Developer Mode` and `patina doctor
  --fix`, and exit code is 5.
- Given a macOS or Linux host, when `patina apply --yes` runs,
  then `patina-elevate.exe` is not present in the process tree
  and no registry read is attempted.
</behavior>

<scenario id="CHK-012">
Given a Windows test host with Developer Mode OFF and a
repository declaring a `[[file]] mode = "symlink"` entry, plus a
TTY harness configured to decline the UAC prompt,
when `patina apply --yes` runs,
then no symbolic link is created at the target path, stderr
contains the substrings `Developer Mode` and
`patina doctor --fix`, and the process exits with
code 5.
</scenario>

<scenario id="CHK-013">
Given a Windows test host with Developer Mode ON (registry value
is 1) and a repository declaring a `[[file]] mode = "symlink"`
entry,
when `patina apply --yes` runs,
then no UAC prompt is presented, no `patina-elevate.exe` process
is spawned, and the symbolic link is created at the target path.
</scenario>
</requirement>

<requirement id="REQ-008">
### REQ-008: `patina-elevate.exe` is a standalone Windows-only helper that toggles Developer Mode

`patina-elevate.exe` is a small standalone Windows binary built
from a third workspace crate `patina-elevate`. It is invoked only
by the main `patina.exe` process via `ShellExecuteEx` with the
`runas` verb. Its sole responsibility is to read its command-line
arguments, perform exactly the requested elevated action, and exit
with a documented exit code. In v1.0 it supports exactly one
subcommand: `enable-developer-mode`, which sets the registry value
`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock\AllowDevelopmentWithoutDevLicense`
to `1` and exits 0 on success.

<done-when>
- The workspace contains a third crate `patina-elevate` whose
  `Cargo.toml` declares `[[bin]]` named `patina-elevate` that
  builds only on `target_os = "windows"`.
- macOS and Linux release artifacts do not include the
  `patina-elevate` binary.
- Windows release artifacts include both `patina.exe` and
  `patina-elevate.exe` in the same directory.
- `patina-elevate.exe enable-developer-mode` invoked elevated
  sets the registry value to 1 and exits 0.
- `patina-elevate.exe enable-developer-mode` invoked
  non-elevated exits 1 with a typed error written to stderr
  naming `ERROR_ACCESS_DENIED` (or the specific HRESULT
  observed).
- `patina-elevate.exe` invoked with any other subcommand exits 2
  (clap usage error) and prints a usage message listing the
  supported subcommands.
- The crate has no dependency on `patina-core` or `patina-cli`;
  it is intentionally small and audit-friendly.
- `patina-elevate.exe` enforces the same panic-free invariant as
  `patina-core` and `patina-cli` (see SPEC-0001 REQ-024): no
  `unwrap`, `expect`, `panic!`, etc. in production code.
</done-when>

<behavior>
- Given a Windows test host running `patina-elevate.exe
  enable-developer-mode` from an elevated cmd prompt, when the
  process completes, then the registry value is 1 and exit code
  is 0.
- Given the same host running `patina-elevate.exe
  enable-developer-mode` from a non-elevated cmd prompt, when
  the process completes, then the registry value is unchanged
  and exit code is 1.
- Given a macOS build of the workspace, when `cargo build
  --workspace --release` runs, then no `patina-elevate` binary
  is produced.
</behavior>

<scenario id="CHK-014">
Given a Windows test host with Developer Mode OFF,
when an elevated `patina-elevate.exe enable-developer-mode` is
invoked (the test harness spawns it with the `runas` ShellExecute
verb and auto-accepts the UAC dialog),
then on process exit, the registry value
`AllowDevelopmentWithoutDevLicense` reads as `1`, and the helper's
exit code is `0`.
</scenario>

<scenario id="CHK-015">
Given a Linux build of the workspace,
when `cargo build --workspace --release` completes,
then `target/release/` contains `patina` but does not contain a
file named `patina-elevate` or `patina-elevate.exe`.
</scenario>
</requirement>

<requirement id="REQ-009">
### REQ-009: SPEC-0002 commands acquire the SPEC-0001 advisory lock for mutating operations

Each new mutating command introduced by SPEC-0002 (`init`, `add`,
`remove`, `promote`, `doctor --fix`) acquires the engine's
exclusive advisory file lock at `<state>/patina/lock` before any
filesystem mutation, as established in SPEC-0001 REQ-023. Read-only
invocations of `doctor` (without `--fix`) acquire a shared lock.

<done-when>
- `patina init`, `patina add`, `patina remove`, `patina promote`,
  and `patina doctor --fix` each acquire the exclusive lock
  before mutating any file.
- `patina doctor` (without `--fix`) acquires only the shared
  lock and yields it on exit.
- Two concurrent invocations of `patina add` against the same
  state directory serialize: the second blocks until the first
  finishes.
- Lock-contention exit code (4) and timeout behavior follow the
  rules established in SPEC-0001 REQ-023 (mutating commands
  time out after 60 seconds; read-only commands warn after 5).
- Commands that re-journal by re-applying (`remove`, `promote`)
  acquire the exclusive lock ONCE for the whole command and drive
  the engine re-apply under SPEC-0001 REQ-030's `Held` lock policy,
  so the re-apply reuses the already-held guard instead of acquiring
  the lock a second time (which would self-contend against the
  command's own lock). The bare-engine apply path defaults to the
  `Blocking` policy; these commands override it to `Held`.
</done-when>

<behavior>
- Given two test processes invoking `patina add` simultaneously
  against the same state directory, when both run, then the
  second blocks until the first releases the lock; both
  complete without journal interleaving.
- Given a running `patina apply` holding the exclusive lock and
  a concurrent `patina doctor` (read-only) invocation, when
  doctor has waited five seconds without acquiring the shared
  lock, then doctor emits the documented warning and proceeds
  to read state.
</behavior>

<scenario id="CHK-016">
Given a test harness that holds the engine's exclusive lock for
ten seconds in process A,
when process B runs `patina add ~/.zshrc --module zsh --symlink`
in parallel,
then process B blocks for approximately ten seconds, then
proceeds and completes successfully; both processes' journal
operations are non-overlapping.
</scenario>
</requirement>

<requirement id="REQ-010">
### REQ-010: SPEC-0002 commands stream structured output to stderr; stdout reserved for user-facing prose and `--json`

Each SPEC-0002 command's user-facing prose output goes to stdout
(or is suppressed when `--json` is set). Structured `tracing`
output, status events, and warnings go to stderr regardless of
flag combinations. The output channels follow the conventions
established by SPEC-0001: human-readable stdout when no `--json`,
JSON object on stdout when `--json`, all telemetry / progress /
warnings on stderr. `patina init`, `patina add`, `patina remove`,
`patina promote`, and `patina doctor` all support `--json` and all
produce deterministic output (same input → same output) per
SPEC-0001 REQ-021.

<done-when>
- `patina init`, `patina add`, `patina remove`, `patina promote`,
  `patina doctor` all accept `--json` and emit a single JSON
  document on stdout when set.
- The `--json` output schema for each command is documented in
  the SPEC (see scenarios below); the schemas do not include
  wall-clock timestamps or other non-deterministic fields.
- All telemetry, warnings, prompts, and progress information go
  to stderr regardless of `--json`.
- Two consecutive runs of any SPEC-0002 command against the same
  state produce byte-identical stdout output.
</done-when>

<behavior>
- Given `patina init <path>` invoked twice in succession
  (second time on an already-initialized directory), when
  diffed, then the stdout outputs of the two invocations are
  byte-identical (the second's stdout is the typed-error JSON
  document; both invocations produce the same document because
  the second fails identically).
- Given `patina doctor --json` invoked twice against the same
  unchanged state directory and repository, when diffed, then
  the stdout outputs are byte-identical.
</behavior>

<scenario id="CHK-017">
Given a tempdir HOME and clean state directory,
when `patina init T --json > out1.json` runs and then
`patina init T --json > out2.json` runs (the second fails because
`T/patina.toml` already exists),
then `diff -u out1.json out2.json` produces non-empty output
(different result fields), but each individual invocation produces
deterministic stdout (a third repetition `patina init T --json
> out3.json` matches `out2.json` byte-for-byte).
</scenario>

<scenario id="CHK-018">
Given a tempdir state directory and repository,
when `patina doctor --json > out1.json` runs and then
`patina doctor --json > out2.json` runs against unchanged state,
then `diff -u out1.json out2.json` produces no output.
</scenario>
</requirement>

## Decisions

<decision id="DEC-001">
The Windows symbolic link permission flow uses the Developer Mode
machine-level registry flag, not the per-user
`SeCreateSymbolicLinkPrivilege` (granted via `secpol.msc`).
Developer Mode is one HKLM registry write; the privilege grant
requires editing local security policy, which is awkward to
automate and harder to detect. Developer Mode is the documented
modern path on Windows 10 1703+ and Windows 11.
</decision>

<decision id="DEC-002">
The elevation helper is a separate binary (`patina-elevate.exe`)
rather than a `patina elevate` subcommand of the main binary.
Reasons:

- A subcommand that re-invokes the main binary elevated would
  cause UAC to inspect the main binary's full attack surface,
  which is dramatically larger than the elevate helper's tiny
  one. The smaller binary is faster to audit and easier to
  reason about for security.
- The helper has no dependency on `patina-core` or `patina-cli`,
  keeping its compiled size small and its trust surface tight.
- Sharing the same binary would tempt future contributors to add
  "while we're elevated, do X too" features. A standalone
  helper makes scope creep visible.
</decision>

<decision id="DEC-003">
On non-Windows platforms, the workspace does not build the
`patina-elevate` crate. The crate's `Cargo.toml` declares the bin
target with `required-features` gating that fail to build on
`target_os = "windows"` builds. Doing so means the macOS and Linux
release artifacts do not contain dead code or unused symbols. The
trade-off is a small additional build-config wart; SPEC-0002
accepts that for the security-surface payoff.
</decision>

<decision id="DEC-004">
Cloud-sync directory detection is deferred to v1.1. The v1.0
choice is "no detection in code; document the risk in
`docs/USER_GUIDE.md`." Alternatives considered and
deferred:

- Hardcoded provider name list (`Dropbox`, `OneDrive`, `iCloud
  Drive`, `Box`, `Google Drive`, `Syncthing`): heuristic, list
  rots as providers rename directories, false-negatives mean
  users still hit the failure mode silently.
- Inspect filesystem xattrs/ADS for sync-provider markers:
  too platform-specific.
- Probe for running processes: intrusive, requires elevated
  privilege on some platforms.
- Configuration-driven list in root `patina.toml`: adds a config
  surface for a footgun the user is already responsible for.

The v1.0 stance is "users are documented as responsible for
keeping the state directory off cloud-sync mounts; SPEC-0001's
assumption is the load-bearing one." Detection becomes a v1.1
candidate if real users surface the failure mode.
</decision>

<decision id="DEC-005">
`patina remove` (without `--purge`) replaces the managed target
with a regular file containing the last-applied content rather than
whatever currently sits on disk (which may have drifted). For
symbolic-link and copy-mode targets the last-applied content is the
bytes of the journaled source — the canonical source path recorded
per SPEC-0001 REQ-029 — read from the repository at remove time. For
template-rendered targets the engine re-renders that journaled source
through MiniJinja against the variable context resolved at remove
time, because the committed journal records only a blake3 hash of the
rendered bytes (REQ-029), not the bytes themselves. Storing the full
rendered bytes in the journal was rejected: it would bloat every
commit record with whole-file contents for one command's
convenience. The trade-off is that a template whose variable context
changed since the last apply re-renders to current-intent bytes
rather than byte-exact last-applied bytes. This is acceptable and
arguably more correct: remove's purpose is to leave a working file,
and the current source plus context is the freshest expression of
intent.
</decision>

<decision id="DEC-006">
`patina promote` refuses on template-rendered targets rather than
attempting a reverse-template inference. Reasons:

- Templating is not a one-to-one function. Two different
  variable contexts can produce the same rendered output;
  reversing requires knowing which variables produced the
  observed bytes.
- A heuristic reverse (e.g., "find variables whose values appear
  literally in the output and replace those substrings with
  variable references") is brittle and surprising.
- The user's recourse for "I want to change a template
  variable" is to edit the template source or the variable
  scope, not to promote a rendered target back.

The error message names the template path and includes the
recommended action ("edit the template source or variable scope
directly").
</decision>

<decision id="DEC-007">
SPEC-0002 commands write and edit `patina.toml` files (`init`
scaffolds a root manifest; `add` creates / appends a module manifest;
`remove` deletes one `[[file]]` entry from an existing module
manifest). SPEC-0001's `patina-core::config` is parse-only —
`parse_module_config` deserializes via `toml::from_str` and `FileEntry`
is `Deserialize`-only; there is no serializer. SPEC-0002 therefore
introduces a TOML *writer*, and the choice is `toml_edit`
(format-preserving) rather than `toml` (reserialize). Reasons:

- `remove` must delete a single `[[file]]` entry while leaving the
  module manifest's other `[[file]]` / `[[hook]]` entries, its
  `[variables]` table, comments, key ordering, and whitespace intact.
  A reserialize-everything writer would rewrite the whole file,
  discarding the user's comments and formatting.
- REQ-010 requires byte-identical stdout/output across reruns; a
  format-preserving editor keeps the on-disk manifest stable across
  edits, which keeps downstream apply output deterministic.

`init` and `add` write fresh tables, where either crate would do, but
using `toml_edit` for all three keeps one writer path. The TOML *read*
side stays on `patina-core::config` (the `toml` crate); only the write
side adds `toml_edit`.
</decision>

<decision id="DEC-008">
The Windows Developer Mode elevation flow (REQ-007) splits across the two
crates along the engine/CLI IO boundary. `patina-core` performs no
user-facing IO — the `output::Reporter` layer and every interactive prompt
live in `patina-cli`, and the no-`println!` / no-`eprintln!` hard rule
applies to the library — so REQ-007's "the engine emits an interactive
prompt … spawns the helper" is realized as a capability/orchestration
split rather than as prompting inside `execute_plan`:

- **Capability in `patina-core::windows`** (IO-free functions returning
  typed values): read the Developer Mode registry flag, query process
  elevation and the OS build, launch `patina-elevate.exe` via
  `ShellExecuteEx` with the `runas` verb, and re-read the flag after the
  helper exits. None of these prompt or print.
- **Orchestration in `patina-cli`**: inspect the resolved plan for
  symbolic-link operations, render the one-time UAC prompt through the
  `Reporter` (reusing the `Tty` / `PromptReader` seam), decide on the
  user's answer, and map a declined prompt to exit code 5 (a command-layer
  control-flow decision, not an `EngineError`). On accept, the CLI invokes
  the helper-launch capability, then re-drives the engine apply under
  `LockPolicy::Held` so the prompt-then-apply spans one held exclusive
  lock.

This satisfies REQ-007's done-when verbatim: "the engine reads the flag /
spawns the helper" is the `patina-core::windows` capability, and "prompt
before any filesystem mutation occurs" holds because the CLI prompts before
it calls `execute_plan`, the first mutation point. The engine retains
`ExecutorError::WindowsSymlinkPermission` (SPEC-0001) as the backstop if a
symbolic-link materialization is ever attempted without Developer Mode.

The rejected alternative — letting `execute_plan` prompt and spawn directly
— would force `patina-core` to take a `Reporter` / stdin dependency,
violate the library IO boundary, and make the apply path untestable without
a fake terminal. Keeping the capability in the engine crate and the
prompting in the CLI preserves both the IO boundary and the existing
prompt-injection seam.
</decision>

## Open Questions

All five self-review questions resolved 2026-05-26 by user
direction; SPEC content updated in the same revision.

- [x] a. **`patina add` move vs copy semantics.** Copy-on-add
  confirmed (REQ-002 copies the file into the repo; the original
  target survives as a regular file until a follow-up `patina apply`
  replaces it with a symbolic link). Matches the chezmoi/dotter
  convention; the user's file is not lost — it lives in the repo
  at `<repo>/<module>/<source>` and `patina apply` materializes the
  target back. (Corrected 2026-05-31: the 2026-05-26 resolution note
  read "move-on-add", but REQ-002's done-when/behavior and CHK-003
  always required the original target to survive — copy, not rename.
  See the Changelog.)
- [x] b. **Cloud-sync detection.** Removed entirely from v1.0.
  REQ-005 drops the cloud-sync findings; the hardcoded provider
  list and DEC-004 are repurposed to record the "no detection,
  docs-only" stance. SPEC-0001's assumption that users keep state
  off cloud-sync mounts remains load-bearing; the project docs
  (`docs/USER_GUIDE.md`) include a user-facing
  callout. v1.1 may revisit if real users surface the failure.
- [x] c. **Developer Mode cache.** Removed. REQ-007 drops the
  `windows_dev_mode.cache` file and 7-day TTL; the registry flag
  is re-read on every apply that contains symbolic link
  operations. Registry reads are microsecond-scale; the cache
  invalidation surface (TTL math, external toggles, file format)
  earns no net win.
- [x] d. **`patina-elevate.exe` non-elevated behavior.** Refuse
  confirmed (REQ-008 stays as drafted). Auto-re-spawn would
  enlarge the helper's attack surface and defeat the "main process
  never elevated" invariant; users invoking the helper directly
  receive a clear error pointing at `patina doctor`.
- [x] e. **`--json` schemas in REQ-010.** No separate schema block.
  The scenarios in each command's requirement already pin field
  names and types (e.g. CHK-010 pins `code/level/message` on the
  doctor `findings` array; CHK-017/CHK-018 pin byte-identical
  determinism across reruns); restating the same shape in a
  `<json-schema>` block would duplicate the contract without
  adding coverage.

## Changelog

<changelog>
| Date       | Author       | Summary |
|------------|--------------|---------|
| 2026-05-25 | human/kevin  | Initial draft. Locks the complete user-facing CLI surface (`init`, `add`, `remove`, `promote`, `doctor` with `--fix`) plus the Windows symbolic-link Developer Mode flow with the `patina-elevate.exe` standalone helper binary. Cloud-sync path detection is heuristic and uses a hardcoded provider list. `patina remove` (without `--purge`) replaces the managed target with a regular file containing the last-applied content. `patina promote` refuses on template-rendered targets because templating is non-invertible. |
| 2026-05-26 | human/kevin  | Resolve all five self-review questions. (a) Confirm move-on-add in REQ-002. (b) Drop cloud-sync detection entirely from REQ-005, REQ-006, the Assumptions block, and the Summary; DEC-004 is reframed as "no detection, docs only" with v1.1 deferral; `docs/operating-environment.md` carries the user-facing callout. (c) Drop the `windows_dev_mode.cache` file and 7-day TTL from REQ-007; the registry flag is re-read on every apply that needs it. (d) Confirm `patina-elevate.exe` refuses non-elevated invocation. (e) No separate `<json-schema>` block in REQ-010; the per-command scenarios pin field shape implicitly. Cross-reference SPEC-0003 in non-goals: no `--linger` flag in v1.0; docs include the manual `sudo loginctl enable-linger` snippet. |
| 2026-05-27 | human/kevin via assistant | Rename the docs target from `docs/operating-environment.md` to `docs/USER_GUIDE.md` everywhere SPEC-0002 references it (5 sites across Summary, Non-goals, REQ-005 prose, DEC-004, and the prior Changelog row's residual context). SPEC-0001's REQ-027 now formalises `docs/USER_GUIDE.md` with named structural anchors and the cloud-sync paths-to-avoid bullet list lives under its `## State directory` section. No requirement-level change in this SPEC; this is a cross-SPEC reference rename driven by the SPEC-0001 amend. |
| 2026-05-29 | human/kevin via assistant | Align `patina remove` / `patina promote` with the SPEC-0001 REQ-029 amendment (committed `ApplyRecord` now retains per-target source + a 32-byte blake3 content hash). REQ-003: redefine the template-target "last-applied content" as re-rendering the journaled source at remove time (the journal records only a blake3 hash of the rendered bytes, not the bytes), and source the copy-mode content from the journaled source path; DEC-005 rewritten to match and to record why full rendered bytes are not journaled. CHK-007: "SHA of the new content" → "blake3 hash of the new content (SPEC-0001 REQ-029)". Add a cross-SPEC handoff bullet noting `remove`/`promote` read the target→source map and content hash from the committed record. Not yet decomposed, so no TASKS reconciliation. |
| 2026-05-29 | human/kevin via assistant | Fix REQ-003's "status reports it as unmanaged" — there is no such state in SPEC-0001's classifier, which has exactly CLEAN/DRIFTED/MISSING/ORPHANED (`status/classify.rs`). Left as written, a removed-but-on-disk target would classify ORPHANED (removed from the plan, still present) until the next apply, surprising the user who just ran `remove`. Require `patina remove` to re-journal the new managed set after mutating (write a fresh `<ts>.COMMIT` omitting the removed target), so `patina status` simply omits the path (unmanaged/absent) and ORPHANED stays reserved for the *implicit* drop (hand-edited TOML / deleted source). Mirrors `promote`, which already re-journals (REQ-004). Reword the REQ-003 prose + done-when bullet, extend CHK-005 to assert status no longer lists the target, and update the cross-SPEC handoff bullet. No dependency change; not yet decomposed, so no TASKS reconciliation. |
| 2026-05-30 | human/kevin via assistant | Close two gaps surfaced by reviewing the shipped SPEC-0001 code against this SPEC. (1) Lock re-entrancy: the shipped engine apply path self-acquires the exclusive lock, so `remove`/`promote` mutating-under-lock and then re-journaling via a re-apply would self-contend. SPEC-0001 gained REQ-030 (an apply-path lock policy: Blocking / NonBlocking / Held); REQ-009 and the cross-SPEC handoffs now require `remove`/`promote` to hold one exclusive lock for the whole command and drive the re-apply under the `Held` policy. Also pin that the fresh COMMIT is produced via the engine re-apply path (no bespoke COMMIT-writer) and that `remove`'s regular-file replacement is pre-re-apply fs work. (2) TOML writer: `patina-core::config` is parse-only (no serializer), but `init`/`add`/`remove` write and edit manifests. Add DEC-007 selecting `toml_edit` (format/comment-preserving — required so `remove` deletes one `[[file]]` entry without rewriting sibling entries/comments and so REQ-010 determinism holds) and add `toml_edit` to the tooling-notes dependency list. Not yet decomposed, so no TASKS reconciliation. |
| 2026-05-30 | human/kevin via assistant | Harden against two discrepancies found by the SPEC-0001-vs-code + cross-SPEC verification pass. (1) Internal inconsistency: REQ-007's declined-UAC error message and CHK-012 named `patina doctor --fix-symlinks`, a flag defined nowhere — REQ-006 defines the remediation flag as `patina doctor --fix`. Changed all three `--fix-symlinks` references (REQ-007 done-when + behaviour, CHK-012) to `--fix` so the error points at the command that exists. (2) Added two non-goals: `patina add` exposes only `--symlink`/`--copy`/`--template` (not the `symlink-dir`/`copy-tree` engine modes — hand-edit the manifest for those), and the release/packaging pipeline (building/signing/distributing the per-OS artifacts) is out of scope for the feature SPECs. Also reviewed the SPEC-0001 2026-05-30 recovery-ordering amendment (orphan recovery now runs under the exclusive lock) for impact on `remove`/`promote`'s `Held`-policy re-apply: consistent — the re-apply recovers under the caller's already-held guard, no self-contention. Not yet decomposed, so no TASKS reconciliation. |
| 2026-05-30 | human/kevin via assistant | Pin the engine/CLI layering split for the Windows Developer Mode flow as DEC-008 (surfaced during SPEC-0002 decomposition). REQ-007's prose ("the engine emits an interactive prompt … spawns the helper") sits in tension with the `patina-core` IO boundary (no `println!`/`eprintln!`; the `Reporter` and all prompts live in `patina-cli`). DEC-008 records the resolution that satisfies REQ-007 without altering it: the *capability* (registry read, elevation/OS queries, `ShellExecuteEx`/`runas` helper launch, flag re-read) lives in `patina-core::windows` as IO-free functions; the *orchestration* (UAC prompt via `Reporter`, decline → exit 5, re-drive `execute_plan` under `LockPolicy::Held` on accept) lives in `patina-cli`. Decisions-block-only edit: no `<requirement>` / `<done-when>` / `<scenario>` / `<goals>` / `<non-goals>` / `<assumptions>` content changed. TASKS.md T-007 / T-009 already describe this split, so reconcile records no task-content change; re-lock the spec hash. |
| 2026-05-31 | human/kevin via assistant | Correct REQ-002's move-vs-copy prose to match its own authoritative scenario and the shipped/reviewed implementation. The requirement description, the `patina add ~/.zshrc` done-when bullet, the `<behavior>` bullet, and user-story-2 said `patina add` "moves" the file into the repo, but the same requirement's `<behavior>` ("`~/.zshrc` is left as a regular file containing the original bytes") and `<scenario>` CHK-003 ("`~/.zshrc` is a regular file with content \"foo\"") always required the original target to survive — i.e. copy, not rename. T-004 implemented copy (`stage_into_repo` uses `fs_err::copy`) to satisfy CHK-003, and the holistic vet drift review flagged the prose as the stale side. Reworded "moves/moved"→"copies/copied" in those four prose sites and updated the resolved Open Question (a) note (which had read "move-on-add confirmed", contradicting the criteria it shipped with). No `<done-when>` assertion semantics, no `<scenario>`, and no code changed — CHK-003 and the implementation are untouched; this is a prose-consistency correction only. TASKS.md task content is unchanged; re-lock the spec hash. |
</changelog>

## Notes

### Cross-SPEC handoffs

SPEC-0002 depends on SPEC-0001 for:

- The engine's apply pipeline, journal format, and backup
  directory layout. `patina add` and `patina remove` mutate
  through the engine; `patina promote` mutates source files
  directly but then triggers a re-apply through the engine.
- The committed apply record (SPEC-0001 REQ-029). `patina remove`
  reads each target's canonical source path from it (to re-render
  or re-read last-applied content) and then writes a fresh record
  omitting the removed target so status treats it as unmanaged
  rather than ORPHANED; `patina promote` writes a fresh record whose
  per-target blake3 content hash status then classifies against.
- The advisory file lock at `<state>/patina/lock`. All mutating
  SPEC-0002 commands acquire the exclusive lock as established in
  SPEC-0001 REQ-023. `remove` and `promote` re-journal by re-applying
  through the engine; because the shipped engine apply path
  self-acquires the exclusive lock, they pass SPEC-0001 REQ-030's
  `Held` policy (supplying their already-acquired guard) so the
  re-apply does not self-contend.
- The engine re-apply primitive itself: `remove` (after replacing the
  target with a regular file) and `promote` (after writing the source)
  invoke the engine apply path to produce the fresh `<ts>.COMMIT`
  record — there is no bespoke "write a COMMIT" primitive. `remove`'s
  regular-file replacement is `remove`-specific filesystem work done
  before the re-apply; the re-apply, planning against the now-removed
  `[[file]]` entry, simply omits the target from the new record.
- The per-machine state directory layout
  (`<state>/patina/journal/`, `<state>/patina/backups/`,
  `<state>/patina/default_repo`, etc.).
- The TTY-driven prompt semantics established for `patina apply`
  in SPEC-0001 REQ-017. `patina add`, `patina remove`,
  `patina promote`, and `patina doctor --fix` all follow the same
  pattern: bare invocation in TTY prompts; non-TTY refuses
  without `--yes`; `--yes` skips prompts.
- The exit-code mapping in SPEC-0001 REQ-022. SPEC-0002 reuses
  codes 0 (success), 1 (generic), 4 (lock contention), 5 (user
  declined / refused UAC). The pre_apply / post_apply codes (2,
  3) are not reused by SPEC-0002 commands (they have no
  hook-execution surface).

SPEC-0003 will pick up:

- `patina watch` subcommands and per-OS service install.
- Watcher-CLI lock coordination (the lock from SPEC-0001 is the
  coordination point; the watcher acquires the same lock).
- Drift detection (hash-compare non-symlink targets, emit OS
  notification via `notify-rust`).
- Windows transient `ERROR_SHARING_VIOLATION` retry-with-backoff
  (50ms → 1.6s, 5 retries) for file write operations.

### Rejected design alternatives

- **Bake the elevation flow into the main `patina.exe`**.
  Rejected (DEC-002): bigger attack surface, harder to audit,
  invites scope creep.
- **Use `SeCreateSymbolicLinkPrivilege`** for per-user grant.
  Rejected (DEC-001): requires `secpol.msc` editing, awkward to
  automate.
- **Auto-toggle Developer Mode without user consent**. Rejected:
  violates the user-consent principle; users must accept the
  UAC dialog as the explicit consent gate.
- **Detect cloud-sync directories (any technique)**. Rejected
  (DEC-004): the v1.0 stance is "no detection, docs only";
  users are responsible for keeping state off cloud-sync mounts.
  Detection is a v1.1 candidate.
- **Auto-reverse template rendering on `patina promote`**.
  Rejected (DEC-006): heuristic, surprising, error-prone.

### Tooling notes

SPEC-0002 introduces two new direct dependencies: `winsafe`
(`taskschd`, `shell`, `advapi` features) for the Windows-specific
modules, and `toml_edit` for the format-preserving `patina.toml`
writer the `init` / `add` / `remove` commands need (DEC-007). The TOML
read side stays on `patina-core`'s existing `toml` dependency.

The `patina-elevate` crate is a Windows-only workspace member.
Its `Cargo.toml` uses `[target.'cfg(windows)']` gating so the
crate is not compiled on macOS or Linux builds.
