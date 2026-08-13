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
more targets on the machine. You declare entries under one of two
kind-typed table-arrays: `[[file]]` for a file source, `[[directory]]`
for a directory source. Each entry carries an optional `mode` that
chooses how Patina materializes it.

A minimal example:

```toml
# A file symlink. `mode` defaults to "symlink" when omitted.
[[file]]
source = "git/gitconfig"
target = "~/.gitconfig"

# A template. A `.tmpl` source is rendered with MiniJinja. The mode is
# implicit for `.tmpl` sources and must not be declared.
[[file]]
source = "shell/zshrc.tmpl"
target = "~/.zshrc"

# A directory materialized as one symbolic link per leaf file.
[[directory]]
source = "config/mpv"
target = "~/.config/mpv"
mode = "symlink-tree"
```

The table you declare an entry under fixes its source kind, and the
`mode` names are shared across both tables: they mean "symlink this
thing" or "copy this thing", with the table supplying the file/dir
context:

- A `[[file]]` accepts `mode = "symlink"` (the default, a symbolic link
  to the source file) or `mode = "copy"` (a byte copy). A `.tmpl` source
  is always rendered as a template and takes no explicit `mode`.
- A `[[directory]]` accepts three modes. `mode = "symlink"` is the
  default: a single atomic symbolic link to the whole directory.
  `mode = "symlink-tree"` makes one symbolic link per leaf file, so the
  target mirrors the source tree. `mode = "copy"` makes a recursive byte
  copy of the tree.

Use `target` for a single destination or `targets = [...]` to fan one
source out to many. (The earlier single `[[file]]` table with the
`symlink-dir` / `copy-tree` mode names is no longer accepted; declare a
`[[directory]]` entry instead.)

Neither a `source` nor a `target` may contain an ASCII control
character (`U+0000`–`U+001F` or `U+007F`), which covers tab, newline,
and carriage return. Patina refuses the whole manifest at parse time and
names the offending character by code point, since a control character
is invisible in an editor. Spaces and non-ASCII characters are fine:
`~/Application Support/café` is a legal target. The rule exists because
every listing Patina prints (`status`, the apply diff, the Defender
listing, the `debug journal` dump) puts one path per line in
tab-separated columns. A tab would open a column the row never closes,
and a newline would split one row into two, which would let a filename
forge a row that reads as Patina's own output.

### Conditional entries with `when`

Any entry may carry a `when` expression, a MiniJinja predicate gating
whether the entry applies on this host. When `when` evaluates false, the
entry contributes no operations and Patina leaves its target untouched.
A target a prior apply had materialized is classified orphaned:

```toml
# Only symlinked on Windows.
[[file]]
source = "windows/profile.ps1"
target = "~/Documents/PowerShell/profile.ps1"
when = "patina.os == 'windows'"
```

`when` expressions are evaluated by the same MiniJinja engine that
renders templates and resolves `[[auto_match]]` profile rules. The same
predicate engine runs at every `when` site. It uses
strict-undefined semantics. A reference to a variable that was never
defined fails the run with an error, rather than yielding a
silently-false predicate. A typo like `patina.oss` instead of
`patina.os` is such a reference. Built-in
facts such as `patina.os` and `patina.hostname` are always available;
`patina.profile` is not defined during profile resolution, so an
`[[auto_match]]` rule must not reference it.

### Variables

Templates and `when` expressions resolve variables through a layered
precedence chain. From lowest to highest priority: built-in `patina.*`
facts, the repo-shared `[variables]` table, each module's own
`[variables]` table, the active profile's `[profiles.<name>.variables]`
table, per-machine variables, and finally CLI overrides. A higher layer
overrides a lower one for the same key.

```toml
# Root patina.toml: repo-shared defaults plus a per-profile override.
[variables]
editor = "nvim"

[profiles.work.variables]
editor = "code"
```

Templates are rendered with MiniJinja under the same strict-undefined
semantics as `when`: referencing a variable that was never defined is an
error at render time, not a silent empty string. Profiles select the
machine-specific variable set layered on top of the repo-shared one.

## Apply flow

Run `patina apply` to materialize your declarations. Apply is a
diff-and-prompt loop by default:

1. **Plan.** Patina discovers your repository, parses every
   `patina.toml`, resolves variables and the active profile, and
   renders templates into a concrete list of operations.
2. **Diff.** Patina compares the planned end-state against what is
   actually on disk and prints the diff. A target a prior apply
   materialized, but the current plan no longer manages, shows as a
   `remove <target>` block. That covers an entry you dropped from a
   `patina.toml`, and one whose `when` is now false. Patina backs the
   target up and deletes it on apply, so the reap is never hidden from
   the consent diff. A dropped target that
   now sits inside another entry's target is left alone instead: that
   entry owns the path, and where it is a whole-directory `symlink` the
   dropped path leads through the link into the entry's source.
3. **Prompt.** In an interactive terminal, Patina asks for
   confirmation before writing anything. In a non-interactive shell
   (CI, a piped invocation), it falls through to plan-only and writes
   nothing.

Re-running `patina apply` against unchanged source is a no-op: the same
plan, no writes, and byte-identical stdout. Patina never overwrites a
file it does not own without taking a backup first.

On a terminal the diff is colorized (green additions, red removals,
bold entry headers), and warnings and errors are styled too. The
confirmation prompt is shown in a distinct prompt color, and its
`[y/N]` keys are highlighted apart from the prose and each other: a
green affirmative `y`, a red default `N`. Color is a display concern
only: piped or redirected
output is always plain, so the byte-identical-stdout guarantee is
unchanged. The `--color` flag (global, accepted before or after any
subcommand) forces the choice: `auto` (the default) colors a terminal
and strips otherwise, `always` colors even when piped, `never` disables
color. `NO_COLOR` in the environment is honoured under `auto`.

Every multi-row listing lines its columns up the same way, sized to the
widest cell: `patina status`, `patina remote list`, `patina remote
update`, `patina watch status`, `patina doctor`, and the Defender
listing. Painted cells pad by printable width, so a piped run and a
terminal run stay aligned identically.

`patina status` prints one row per managed target and a summary of the
four counters:

```text
clean     /home/u/.zshrc
drifted   /home/u/.gitconfig
missing   /home/u/.config/nvim/init.lua
orphaned  /home/u/.oldrc
clean: 1  drifted: 1  missing: 1  orphaned: 1
```

On a terminal the state word is green (clean), yellow (drifted), red
(missing), or magenta (orphaned); an orphan gets its own hue because it
is a leftover awaiting a reap, with no place on a severity scale. In the
summary only a non-zero counter is painted, so a clean repository reads
at a glance. Every state is in the text as well, so a stripped run loses
the color and nothing else.

## Commands

Beyond `apply`, `status`, `rollback`, and `debug journal`, Patina ships
five commands for setting up a repository and migrating existing
dotfiles into management. Each of the mutating commands accepts two
common flags:

- `--json` emits a structured JSON envelope instead of human-readable
  output. For read-only commands this is a pure formatting switch.
- `--yes` proceeds without the interactive confirmation prompt. The
  commands that overwrite or delete data (`remove`, `promote`, and
  `doctor --fix`) follow the same prompt semantics as `apply`: a bare
  invocation in an interactive terminal prompts before mutating; a
  non-interactive shell refuses to mutate unless you pass `--yes`.
  `init` and `add` do not have a confirm-before-mutate gate. `init`
  writes unconditionally (it refuses only if a manifest already
  exists), and accepts `--yes` for parity without acting on it. `add`
  prompts only for an omitted mode or module, and only in an interactive
  terminal. In a non-interactive shell it refuses *those specific*
  missing inputs. Once mode and module are supplied it writes without
  prompting.

| Command   | Purpose                                                                                       |
| --------- | --------------------------------------------------------------------------------------------- |
| `init`    | Scaffold a root `patina.toml` and persist the default-repository pointer.                     |
| `add`     | Bring an existing dotfile under management: copy it into a module and write a `[[file]]` entry for a file source or a `[[directory]]` entry for a directory source.|
| `remove`  | Unmanage a target: drop its entry and replace the target with a regular file holding the last-applied content. |
| `promote` | Copy a drifted copy-mode target's current bytes back into its repository source, then re-apply. |
| `doctor`  | Inspect the environment for known problems (UNC repository paths, missing Windows Developer Mode, OS-too-old, missing default repo, missing `git`). |
| `remote`  | Manage remote git sources: `list` the pins, `check` upstream tips, `update` a pin through the update gate, `prune` cached checkouts. See [Remote sources](#remote-sources). |

`patina remove` has a `--purge` flag: instead of leaving a regular file
behind with the last-applied content, `--purge` deletes the target
outright.

`patina doctor` is read-only by default and reports its findings as
warnings. With `--fix`, it walks the findings it knows how to remediate,
prompts for confirmation on each, and applies the fix on accept. In a
non-interactive shell, `--fix` requires `--yes`.

Patina uses one exit-code scheme across every command:

- `0`: success.
- `1`: a generic error (config parse, IO, an undefined template
  variable, and so on).
- `2`: invalid usage, such as an unknown flag or two conflicting mode
  flags on `add`. `apply` also returns `2` when a `pre_apply`
  `must_succeed` hook fails.
- `3`: an `apply` `post_apply` `must_succeed` hook failed, and its file
  operations were rolled back.
- `4`: exclusive-lock acquisition timed out (another `patina` process
  held the lock).
- `5`: the interactive prompt was declined, or, on Windows, the
  one-time elevation UAC prompt was refused.

### Windows symbolic-link elevation

Creating symbolic links on Windows requires either Developer Mode or an
elevated session. When Patina needs the privilege and Developer Mode is
off, it offers a one-time elevation: a single UAC prompt appears, and
accepting it toggles Developer Mode on via the bundled
`patina-elevate.exe` helper so future runs need no elevation. If you
decline the UAC prompt, Patina exits with code `5` and points you at
`patina doctor --fix`, which offers the same Developer Mode remediation.

## Windows Defender exclusions

On Windows, Microsoft Defender scans file I/O in real time. A dotfiles
repository is a pile of small git objects, and `apply` reads and writes
many links and copies. That per-access scan is therefore pure overhead
for paths you already trust. `patina defender` adds Defender **path exclusions** for
the repository and its deployed targets to remove it. The command is
Windows-only and does not appear in `--help` on macOS or Linux.

An exclusion is a permanent hole in your antivirus coverage, so this is
never something `patina apply` does on its own. You run it deliberately,
you see every path first, and you consent before anything changes:

| Command                   | Purpose                                                                             |
| ------------------------- | ----------------------------------------------------------------------------------- |
| `patina defender status`  | Show the current exclusions against the desired set. Read-only and unprivileged.    |
| `patina defender apply`   | Add every desired exclusion that is missing and remove the patina-owned ones the current plan no longer manages. |
| `patina defender clear`   | Remove every patina-owned exclusion.                                                |

### What Patina can see without administrator

Not the exclusion list. `Get-MpPreference` returns it only to an elevated
caller. Unelevated it reports `N/A: Must be an administrator to view
exclusions` and exits successfully, so there is nothing to compare against.

Without administrator, Patina reports state from its own ledger and labels
it as such: `recorded` and `not recorded` rather than `present` and
`missing`, under a note saying where the state came from. `--json` carries
the same distinction as `current_readable: false`. The practical limit is
that an exclusion you delete by hand in the Defender UI goes unnoticed.
Run `patina defender status` from an elevated shell to see it: that reads
the live list and reports `present` or `missing` against it.

The desired set is exactly the repository root plus **one** exclusion per
managed target: a folder exclusion for a directory entry
(`symlink` / `symlink-tree` / `copy`) and a file exclusion for a file
entry (`symlink` / `copy` / template). A `symlink-tree` of forty files
contributes the one declared target directory, never forty entries.
Patina emits exact paths only, with no wildcards and no process or
extension exclusions. It refuses to exclude a UNC path, a drive root, or a
system directory (`%SystemRoot%`, `%ProgramFiles%`, and friends).

### Reading the listing

The listing carries the exclusion kind as **color on the path**, and the
state as a colored tag after it. The tags share one column, sized to the
widest path, so a reader scans the state straight down a long list:

```text
  C:\Users\kevin\dotfiles      [present]
  C:\Users\kevin\.gitconfig    [missing]
  C:\Users\kevin\.config\nvim  [present, not recorded by patina]
```

| Element      | Meaning            |
| ------------ | ------------------ |
| Blue path    | A file exclusion   |
| Magenta path | A folder exclusion |

| State tag                                  | Meaning                                                   |
| ------------------------------------------ | --------------------------------------------------------- |
| Green `[present]`                          | Excluded in Defender, and Patina's ledger records it       |
| Yellow `[present, not recorded by patina]` | Excluded in Defender, but Patina does not own it           |
| Red `[missing]`                            | Not excluded; `apply` would add it                         |
| Green `[recorded]`                         | Ledger records it; the live list was not readable          |
| Red `[not recorded]`                       | Ledger does not record it; the live list was not readable  |

`[present, not recorded by patina]` is worth acting on. The path is already
excluded, so `apply` will not touch Defender for it. But the ledger does
not own it, and **`clear` will not reap it**. You get this when you excluded the
path by hand, or when a Patina run applied it without recording the result.
Running `apply` adopts it: the ledger converges on the whole desired set, so
the entry becomes `present` and `clear` can reverse it afterwards.

Spotting an unowned exclusion needs the live list, so it only shows up on an
elevated run. Unprivileged there is nothing to compare against, and you see
the two ledger-derived states instead.

Color is the only place the kind appears, so it is gone wherever ANSI is
stripped: a pipe, a redirect, `--color never`, `NO_COLOR`. Use `--json` when
you need this as data. Every entry there carries an explicit `kind` (`file`
or `folder`) and `state` (`owned`, `unmanaged`, `absent`, `recorded`,
`unrecorded`).

`apply` and `clear` preview the additions and removals, then prompt before
acting; a non-interactive shell requires `--yes`. Accepting raises one UAC
prompt (the main `patina.exe` never runs elevated, only a small bundled
helper does). Declining the prompt exits `5`.

The helper is also what verifies the change, since it is the only part of
Patina elevated enough to read the exclusion list back. It re-reads after
writing and records the verdict, which the waiting `patina.exe` picks up.
Three outcomes exit `1`, and they say different things:

| Outcome                                | What it means                                                                        |
| -------------------------------------- | ------------------------------------------------------------------------------------ |
| Defender rejected the change           | The write returned success and changed nothing. Usually Tamper Protection or a Defender managed by policy (Intune, GPO). Check `Get-MpComputerStatus`. |
| The helper could not apply the request | It never reached Defender: a path it refused, or a request file it could not read.     |
| The helper reported no result          | Nobody observed the outcome. The exclusions may have been applied without being recorded, so re-run `apply`, which is idempotent. |

The distinction matters because the fix differs. Patina will not report a
success it could not confirm, and it will not blame Defender for an outcome
it never saw.

Patina records only the exclusions it added, in a per-machine ledger.
`apply` therefore reaps a stale patina-owned exclusion, and **never
touches a user-added exclusion**. `clear` removes only what Patina owns.

If you are on Windows 11, consider a [Dev Drive](https://learn.microsoft.com/windows/dev-drive/)
(ReFS) in Defender *performance mode*. It scans asynchronously instead of
not at all, so it is the lower-risk choice where it applies.

## Watch service

`patina watch` runs a per-user background watcher. It re-applies your
configuration when the source repository changes, and surfaces drift when
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
you next log in. You may want the watcher to keep running across logout,
for example on a server you SSH in and out of. Enable lingering for your
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
(runit, s6, OpenRC) instead. Patina does not ship service templates for
these init systems in v1.0.

### Drift notifications

The watcher hashes every non-symlink managed target when it changes, and
compares the result against the hash recorded at the last apply. Those
targets are copy-mode files, copied directory trees, and rendered
templates. On divergence it
emits a desktop notification titled "Patina: drift detected" naming the
target, and records the event in a drift cache at
`<state>/patina/drift.cache`. Notifications are rate-limited to at most
one per target per 60-second window. Symlink targets are not watched for
drift: editing a symlinked file is editing the source, which the source
watcher already catches.

Drift surfaces two ways, and you do not need the watcher running to see
it the second way:

- As the desktop notification above, **only while the watcher is
  running**.
- As `drifted` in `patina status`, **always**. `patina status` decides
  drift by re-hashing the target live, independent of the watcher. A
  file you edit and then revert to its recorded bytes therefore reports
  `clean`, even though the watcher logged the intervening edit. The
  drift cache is
  the watcher's own notification ledger; `patina status` does not read
  it.

Resolve a drifted target either way:

- `patina apply` reverts the target to the source content.
- `patina promote` updates the source from the target's current bytes,
  then re-applies.

## Remote sources

An entry can draw its source from someone else's git repository instead
of from your own. Declare the repository once in your root manifest, then
name it from any entry that wants its bytes:

```toml
# patina.toml (root)
[[remote]]
url = "https://github.com/blader/humanizer"
ref = "main"          # optional; defaults to the remote's default branch
# name = "humanizer"  # optional; taken from the URL's last segment
```

```toml
# agent-configs/patina.toml
[[file]]
source = "shared/AGENTS.md"          # no `remote`: this module's own tree
target = "~/.claude/CLAUDE.md"

[[file]]
source = "SKILL.md"                  # a path inside the humanizer checkout
remote = "humanizer"
target = "~/.claude/skills/humanizer/SKILL.md"
```

Entries with and without a `remote` key sit side by side in one manifest,
and one manifest may draw on several remotes. An entry with no `remote`
key resolves against its module directory exactly as it always has.

The commit each machine materializes lives in `patina.lock`, next to
your root `patina.toml`, and is committed like any other file. That is
what makes a remote update flow through your repository: you bump the pin
on the machine you work from, commit it, and every other machine catches
up with `git pull && patina apply`.

Bumping a pin is the moment third-party code changes what lands on your
machines, so `patina remote update` slows it down. A candidate commit
must not be dated in the future, and must descend from the pin you
already have. It must not predate the pin's own timestamp, and must be at
least `min_age` old (72 hours unless you say otherwise). Every byte still
passes the ordinary diff-and-prompt loop before it reaches your
filesystem. Patina never treats remote content as a template, and never
reads it as configuration.

Cached checkouts live in the per-machine [state
directory](#state-directory), never in your repository, and are pruned
automatically once no journal record needs them.

Read [`REMOTE_SOURCES.md`](REMOTE_SOURCES.md) for the whole model. It
covers the lockfile format, the cache layout, and each gate check with
what it can and cannot stop. It also covers the shell snippets for the
background update notice, and the multi-machine flow.

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
undefined. **Patina does not detect cloud-sync directories in v1.0.**
Keep both the state directory and your dotfiles repository off the
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
ends up in either the pre-apply or the post-apply state, never a torn
intermediate. The next invocation reads the journal and rolls forward
or back to reach a consistent state. This guarantee covers process
termination (a `kill -9` or crash where the page cache survives); a
power loss or kernel panic mid-apply is out of scope for v1.0.

Two commands help you recover deliberately:

- `patina status` reports drift between what your configuration
  declares and what is currently on disk.
- `patina rollback` reverses the last successful apply by restoring the
  pre-apply bytes recorded in the journal. Afterwards the filesystem
  matches the pre-apply state in content and entry kind (file, symlink,
  or directory), modulo mode/timestamp bits and files you edited outside
  Patina.

For a post-mortem, `patina debug journal <path>` decodes the binary
journal into human-readable form so you can see exactly what the
interrupted or completed apply intended to do. The parallel
`patina debug drift-cache <path>` decodes the watcher's binary drift
cache (`<state>/patina/drift.cache`). It prints the version envelope, the
journal timestamp the cache is bound to, and one block per recorded
divergence. Each block names the target path, the expected and actual
hashes, and the detection time. Both refuse a file written by a newer Patina with a typed
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
- **`patina status` reports `drifted` but no desktop notification
  appeared.** Notifications only fire while the watcher is running, and
  are rate-limited to one per target per 60 seconds; `patina status`
  reports drift from a live re-hash regardless. Resolve with `patina
  apply` (revert to source) or `patina promote` (update source from
  target).
