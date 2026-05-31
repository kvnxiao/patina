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
[[link]]
source = "git/gitconfig"
target = "~/.gitconfig"

[[template]]
source = "shell/zshrc.j2"
target = "~/.zshrc"
```

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

These commands reuse the exit codes established for `apply` in
SPEC-0001:

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

(Enabling the Linux watcher service to survive logout —
`sudo loginctl enable-linger $USER` — is a SPEC-0003 concern and is not
covered here.)

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
interrupted or completed apply intended to do.

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
