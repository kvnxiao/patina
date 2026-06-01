# Patina User Guide

Patina is a cross-platform dotfile manager whose source of truth is
your centralized git repository. You declare configuration in
`patina.toml` files and run `patina apply`; Patina materializes each
declaration at the right target as a symbolic link, rendered template
output, or byte copy.

This guide covers installation, declaring dotfiles, the apply flow,
where Patina keeps per-machine state, how to recover from a bad apply,
and common troubleshooting.

## Installation

Patina is a single binary. Build it from source with a current Rust
toolchain:

```sh
cargo install --path patina-cli --locked
```

This installs the `patina` binary onto your `PATH`. Verify it:

```sh
patina --version
```

On Windows, creating symbolic links requires either Developer Mode
enabled or an elevated (UAC) session. Patina surfaces a clear error
when it lacks the privilege rather than failing cryptically.

## Declaring dotfiles

Configuration lives in `patina.toml` files inside your dotfiles
repository. Each entry declares a source path in the repo and one or
more targets on the machine, plus the file mode Patina uses to
materialize it (per-file symlink, atomic directory symlink, byte copy,
copy-tree, or template render).

A minimal example:

```toml
# A symlink — `mode` defaults to "symlink" when omitted.
[[file]]
source = "git/gitconfig"
target = "~/.gitconfig"

# A template — a `.tmpl` source is rendered with MiniJinja. The mode is
# implicit for `.tmpl` sources and must not be declared.
[[file]]
source = "shell/zshrc.tmpl"
target = "~/.zshrc"
```

Each entry is a `[[file]]` table. `mode` accepts `symlink` (the default),
`symlink-dir`, `copy`, or `copy-tree`; a `.tmpl` source is always rendered as
a template and takes no explicit `mode`. Use `target` for a single
destination or `targets = [...]` to fan one source out to many.

Templates are rendered with MiniJinja under strict-undefined semantics:
referencing a variable that was never defined is an error at render
time, not a silent empty string. Variables resolve through a defined
precedence chain, and profiles select machine-specific variable sets.

## Apply flow

Run `patina apply` to materialize your declarations. Apply is a
diff-and-prompt loop by default:

1. **Plan** — Patina discovers your repository, parses every
   `patina.toml`, resolves variables and the active profile, and
   renders templates into a concrete list of operations.
2. **Diff** — Patina compares the planned end-state against what is
   actually on disk and prints the diff.
3. **Prompt** — in an interactive terminal, Patina asks for
   confirmation before writing anything. In a non-interactive shell
   (CI, a piped invocation), it falls through to plan-only and writes
   nothing.

Re-running `patina apply` against unchanged source is a no-op: the same
plan, no writes, and byte-identical stdout. Patina never overwrites a
file it does not own without taking a backup first.

## Commands

Beyond `apply`, `status`, `rollback`, and `debug journal`, Patina ships
five commands for setting up a repository and migrating existing
dotfiles into management. Each of the mutating commands accepts two
common flags:

- `--json` emits a structured JSON envelope instead of human-readable
  output. For read-only commands this is a pure formatting switch.
- `--yes` proceeds without the interactive confirmation prompt. The
  commands that overwrite or delete data — `remove`, `promote`, and
  `doctor --fix` — follow the same prompt semantics as `apply`: a bare
  invocation in an interactive terminal prompts before mutating; a
  non-interactive shell refuses to mutate unless you pass `--yes`.
  `init` and `add` do not have a confirm-before-mutate gate. `init`
  writes unconditionally (it refuses only if a manifest already
  exists), and accepts `--yes` for parity without acting on it. `add`
  prompts only for an omitted mode or module when run in an interactive
  terminal — and refuses *those specific* missing inputs in a
  non-interactive shell — so once mode and module are supplied it
  writes without prompting.

| Command   | Purpose                                                                                       |
| --------- | --------------------------------------------------------------------------------------------- |
| `init`    | Scaffold a root `patina.toml` and persist the default-repository pointer.                     |
| `add`     | Bring an existing dotfile under management: copy it into a module and write a `[[file]]` entry.|
| `remove`  | Unmanage a target: drop its `[[file]]` entry and replace the target with a regular file holding the last-applied content. |
| `promote` | Copy a drifted copy-mode target's current bytes back into its repository source, then re-apply. |
| `doctor`  | Inspect the environment for known problems (UNC repository paths, missing Windows Developer Mode, OS-too-old, missing default repo). |

`patina remove` has a `--purge` flag: instead of leaving a regular file
behind with the last-applied content, `--purge` deletes the target
outright.

`patina doctor` is read-only by default and reports its findings as
warnings. With `--fix`, it walks the findings it knows how to remediate,
prompts for confirmation on each, and applies the fix on accept. In a
non-interactive shell, `--fix` requires `--yes`.

These commands reuse the exit codes established for `apply`:

- `0` — success.
- `1` — a generic error (config parse, IO, an undefined template
  variable, and so on).
- `4` — exclusive-lock acquisition timed out (another `patina` process
  held the lock).
- `5` — the interactive prompt was declined, or — on Windows — the
  one-time elevation UAC prompt was refused.

### Windows symbolic-link elevation

Creating symbolic links on Windows requires either Developer Mode or an
elevated session. When Patina needs the privilege and Developer Mode is
off, it offers a one-time elevation: a single UAC prompt appears, and
accepting it toggles Developer Mode on via the bundled
`patina-elevate.exe` helper so future runs need no elevation. If you
decline the UAC prompt, Patina exits with code `5` and points you at
`patina doctor --fix`, which offers the same Developer Mode remediation.

## Watch service

`patina watch` runs a per-user background watcher that re-applies your
configuration when the source repository changes and surfaces drift when
a managed target is edited outside Patina. It never needs admin or sudo
on its default path.

The watcher has two shapes. The lifecycle subcommands manage a background
service registered with your OS supervisor:

| Command                  | Purpose                                                            |
| ------------------------ | ------------------------------------------------------------------ |
| `patina watch install`   | Register the watcher to launch at login. Exits 1 if already installed; run `uninstall` first to re-register. |
| `patina watch uninstall` | Stop the running watcher and remove the service registration.      |
| `patina watch start`     | Ask the supervisor to start the installed service.                 |
| `patina watch stop`      | Ask the supervisor to stop the service without removing it.        |
| `patina watch restart`   | Stop then start the installed service.                             |
| `patina watch status`    | Report the service's installed / running state, last-exit code, and the watcher's subscription and re-apply counters. Read-only. |

`patina watch --foreground` instead runs the watcher loop inline,
attached to the current terminal, and shuts down cleanly on Ctrl-C
(SIGINT) or SIGTERM. The installed background service runs the same
foreground loop under your supervisor.

`install` writes a per-user service descriptor whose location depends on
the OS:

| OS      | Service descriptor                                      | Supervisor       |
| ------- | ------------------------------------------------------- | ---------------- |
| macOS   | `~/Library/LaunchAgents/com.patina.watcher.plist`       | `launchd`        |
| Linux   | `~/.config/systemd/user/patina-watcher.service`         | `systemd --user` |
| Windows | Scheduled Task named `Patina Watcher` (HKCU, logon trigger) | Task Scheduler |

### Surviving logout on Linux

A `systemd --user` service stops when you log out and starts again when
you next log in. If you want the watcher to keep running across logout —
for example on a server you SSH in and out of — enable lingering for your
user once:

```sh
sudo loginctl enable-linger $USER
```

Patina does not run this for you and ships no `--linger` flag: the
command needs sudo, and Patina's invariant is that it never prompts for
elevated privilege on your behalf. Run it yourself when you need
survive-logout behavior; skip it on a desktop where the watcher only
needs to run while you are logged in.

### Non-systemd init systems

`patina watch install` targets `systemd --user` on Linux. On a
distribution without systemd (Void, Devuan with a non-systemd init,
Alpine), run `patina watch --foreground` under your own supervisor
(runit, s6, OpenRC) instead — Patina does not ship service templates for
these init systems in v1.0.

### Drift notifications

For every non-symlink managed target (copy-mode files, copy-tree files,
rendered templates), the watcher hashes the target when it changes and
compares against the hash recorded at the last apply. On divergence it
emits a desktop notification titled "Patina: drift detected" naming the
target, and records the event in a drift cache at
`<state>/patina/drift.cache`. Notifications are rate-limited to at most
one per target per 60-second window. Symlink targets are not watched for
drift — editing a symlinked file is editing the source, which the source
watcher already catches.

Drift surfaces two ways, and you do not need the watcher running to see
it the second way:

- As the desktop notification above, **only while the watcher is
  running**.
- As `DRIFTED` in `patina status`, **always**. `patina status` decides
  drift by re-hashing the target live, independent of the watcher, so a
  file you edit and then revert to its recorded bytes reports `CLEAN`
  even though the watcher logged the intervening edit. The drift cache is
  the watcher's own notification ledger; `patina status` does not read
  it.

Resolve a drifted target either way:

- `patina apply` reverts the target to the source content.
- `patina promote` updates the source from the target's current bytes,
  then re-applies.

## State directory

Patina writes its journal, backups, advisory lock, and drift cache to a
**per-machine state directory**, never into your dotfiles repository.
The location is OS-appropriate:

| OS      | State directory                          | Override                  |
| ------- | ---------------------------------------- | ------------------------- |
| Linux   | `~/.local/state/patina/`                 | `$XDG_STATE_HOME/patina/` |
| macOS   | `~/Library/Application Support/patina/`  | (none in v1.0)            |
| Windows | `%LOCALAPPDATA%\patina\`                 | (none in v1.0)            |

The state directory must live on a local-disk filesystem. Patina's
crash-safety guarantee depends on the journal being written atomically
and surviving a `kill -9`; cloud-sync providers intermediate writes
through their own queueing and versioning layers, which breaks atomic
`fsync`, reorders recovery reads, and leaves the advisory lock
undefined. **Patina does not detect cloud-sync directories in v1.0** —
keep both the state directory and your dotfiles repository off the
following kinds of mounts:

- iCloud Drive
- OneDrive
- Dropbox
- Box
- Google Drive
- Syncthing

If you must move the state directory, point `XDG_STATE_HOME` (Linux) at
another local-disk path; do not point it at any of the providers above.

## Recovery

Patina is built so an interrupted apply converges deterministically on
the next run. If `patina apply` is killed mid-write, the filesystem
ends up in either the pre-apply or the post-apply state — never a torn
intermediate. The next invocation reads the journal and rolls forward
or back to reach a consistent state.

Two commands help you recover deliberately:

- `patina status` reports drift between what your configuration
  declares and what is currently on disk.
- `patina rollback` reverses the last successful apply by restoring the
  pre-apply bytes recorded in the journal. Afterwards the filesystem
  matches the pre-apply state byte-for-byte, modulo files you edited
  outside Patina.

For a post-mortem, `patina debug journal <path>` decodes the binary
journal into human-readable form so you can see exactly what the
interrupted or completed apply intended to do. The parallel
`patina debug drift-cache <path>` decodes the watcher's binary drift
cache (`<state>/patina/drift.cache`), printing its version envelope, the
journal timestamp it is bound to, and one block per recorded divergence
naming the target path, the expected and actual hashes, and the
detection time. Both refuse a file written by a newer Patina with a typed
error naming the version mismatch, and exit 1 on an invalid path.

## Troubleshooting

- **`patina apply` writes nothing and only prints a plan.** You are in
  a non-interactive shell. Apply falls through to plan-only when stdin
  is not a TTY. Run it in an interactive terminal to get the
  confirmation prompt.
- **Symlink creation fails on Windows.** Enable Developer Mode or run
  the command from an elevated (UAC) session so Patina has the
  privilege to create symbolic links.
- **A template render fails with an undefined-variable error.** Patina
  uses strict-undefined semantics on purpose. Define the variable in
  the appropriate scope or profile rather than relying on an empty
  default.
- **Apply seems to hang.** Another `patina` process may hold the
  advisory lock. Patina waits up to a bounded timeout and then exits
  with the lock-timeout exit code; check for a concurrent apply or a
  running watcher.
- **Recovery behaves unexpectedly after a crash.** Confirm your state
  directory is on local disk and not a cloud-sync mount (see "State
  directory"). Use `patina debug journal` to inspect the journal that
  recovery read.
- **The watcher stops when you log out of a Linux box.** A `systemd
  --user` service ends with your session by default. Run `sudo loginctl
  enable-linger $USER` once to keep it running across logout (see "Watch
  service").
- **`patina status` reports `DRIFTED` but no desktop notification
  appeared.** Notifications only fire while the watcher is running, and
  are rate-limited to one per target per 60 seconds; `patina status`
  reports drift from a live re-hash regardless. Resolve with `patina
  apply` (revert to source) or `patina promote` (update source from
  target).
