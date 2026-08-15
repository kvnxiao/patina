# Patina User Guide

Patina is a cross-platform dotfile manager whose source of truth is
your centralized git repository. You declare configuration in
`patina.toml` files and run `patina apply`; Patina materializes each
declaration at the right target as a symbolic link, rendered template
output, or byte copy.

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
enabled or an elevated (UAC) session. Without one of the two, Patina
names the missing privilege in the error.

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

The table fixes the source kind. Both tables share the `mode` names:
`symlink` and `copy` mean the same thing either way, with the table
supplying the file-or-directory context.

- A `[[file]]` accepts `mode = "symlink"` (the default, a symbolic link
  to the source file) or `mode = "copy"` (a byte copy). A `.tmpl` source
  is always rendered as a template and takes no explicit `mode`.
- A `[[directory]]` accepts three modes. `mode = "symlink"` is the
  default: a single atomic symbolic link to the whole directory.
  `mode = "symlink-tree"` makes one symbolic link per leaf file, so the
  target mirrors the source tree. `mode = "copy"` makes a recursive byte
  copy of the tree.

Use `target` for a single destination or `targets = [...]` to fan one
source out to many.

Neither a `source` nor a `target` may contain an ASCII control character
(`U+0000` through `U+001F` or `U+007F`), which covers tab, newline, and
carriage return. Patina refuses the whole manifest at parse time and
names the offending character by code point, since a control character is
invisible in an editor. Spaces and non-ASCII characters are fine:
`~/Application Support/café` is a legal target.

Every listing Patina prints (`status`, the apply diff, the Defender
listing, the `debug journal` dump) puts one path per line in
tab-separated columns. A tab would open a column the row never closes. A
newline would split one row in two, letting a filename forge a row that
reads as Patina's own output.

### Excluding generated files with `ignore`

A `symlink-tree` or `copy` `[[directory]]` deploys every file under its
source: the `__pycache__/*.pyc` a Python run left behind, `.DS_Store` on
macOS, `Thumbs.db` on Windows. An `ignore` list drops those paths from
the enumeration:

```toml
# Root patina.toml: patterns every tree-mode entry starts from.
[patina]
root = true
ignore = [".DS_Store", "Thumbs.db", "desktop.ini"]
```

```toml
# Any module: patterns for this entry, appended after the repo-wide list.
[[directory]]
source = "scripts"
target = "~/bin"
mode = "symlink-tree"
ignore = ["__pycache__/", "*.pyc"]
```

Patterns use gitignore syntax: `*` and `?` wildcards, `**` for any depth,
`{a,b}` alternates, a trailing `/` for directories only, a leading `/` to
anchor, and a leading `!` to bring back a path an earlier pattern excluded.
Last match wins, so a per-entry `!keep.pyc` overrides a repo-wide `*.pyc`.
Git's exception to that carries over: a `!` cannot bring back a path
inside an excluded directory. `["build/", "!build/keep.txt"]` still drops
`keep.txt`, because the walk stops at `build` and never looks inside.
Exclude the files themselves when you need one back.

Where this departs from git, deliberately:

- **Patterns anchor at the entry's source directory**, both levels. A
  repo-wide `/build` means "`build` at the top of each entry's source", not
  one directory at the repository root.
- **Matching ignores case on every platform.** One `Thumbs.db` pattern also
  covers `thumbs.db`. Git decides this per clone, which would make one
  manifest behave three ways across macOS, Linux, and Windows.
- **Every pattern is authored in a manifest.** Patina reads no
  `.gitignore` or `.patinaignore` file, and a `.gitignore` inside a remote
  checkout is third-party content it never opens.
- **No default patterns.** A repository deploys what its own manifests
  say. `patina init` scaffolds the root block above commented out, ready
  to uncomment.

`ignore` is accepted on the `[[directory]]` modes that enumerate a tree:
`symlink-tree` and `copy`. Declaring it on a `[[file]]`, on a
`[[directory]]` with `mode = "symlink"`, or under `[patina]` in a module
manifest is a parse error. `ignore` filters a tree walk. A `[[file]]`
deploys a single path, and a whole-directory `symlink` deploys one link
that exposes everything beneath it. Only the root manifest's
`[patina] ignore` is read. Each error names where the patterns do belong.

If a pattern you add excludes a path a previous apply already deployed, the
next `patina apply` removes it. The diff labels that removal `ignored` so
the deletion is traceable to the pattern you just wrote, and `patina doctor`
warns while the removal is still pending:

```text
remove /home/kevin/bin/stale.pyc (ignored)
  - /home/kevin/dotfiles/py/scripts/stale.pyc
```

An entry whose every leaf is ignored deploys nothing and leaves its target
directory absent. `apply` reports it as unchanged, not as pending work.

`patina add` refuses a path that a tree entry's `ignore` list already
excludes, because the new `[[file]]` entry would deploy exactly what the
tree entry was told to skip. Pass `--force` to declare it anyway.

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

One MiniJinja engine evaluates every `when` expression, renders every
template, and resolves the `[[auto_match]]` profile rules, under
strict-undefined semantics. A reference to a variable that was never
defined fails the run with an error, so a typo such as `patina.oss` for
`patina.os` stops you rather than yielding a silently-false predicate.
Built-in facts such as `patina.os` and `patina.hostname` are always
available. `patina.profile` is undefined during profile resolution, so an
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

Profiles select the machine-specific variable set layered on top of the
repo-shared one.

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
   `patina.toml` and one whose `when` is now false. Patina backs the
   target up and deletes it on apply, inside the consent diff like every
   other change. One exception: a dropped target that now sits inside
   another entry's target is left alone, because that entry owns the
   path. Where the owner is a whole-directory `symlink`, the dropped path
   leads through the link into the owner's source.
3. **Prompt.** In an interactive terminal, Patina asks for
   confirmation before writing anything. In a non-interactive shell
   (CI, a piped invocation), it falls through to plan-only and writes
   nothing.

Re-running `patina apply` against unchanged source is a no-op: the same
plan, no writes, and byte-identical stdout. Patina never overwrites a
file it does not own without taking a backup first.

On a terminal the diff is colorized: green additions, red removals, bold
entry headers, styled warnings and errors. The confirmation prompt gets
its own color, and its `[y/N]` keys are painted apart from the prose and
from each other, a green affirmative `y` against a red default `N`. Piped
or redirected output is always plain. Stdout therefore stays
byte-identical between runs. The `--color` flag (global, before or after any
subcommand) forces the choice: `auto` (the default) colors a terminal and
strips otherwise, `always` colors even when piped, `never` disables
color. `NO_COLOR` in the environment is honoured under `auto`.

Every multi-row listing lines its columns up the same way, sized to the
widest cell: `patina status`, `patina remote list`, `patina remote
update`, `patina watch status`, `patina doctor`, and the Defender
listing. Painted cells pad by printable width, so a piped run and a
terminal run stay aligned identically.

`patina status` prints one row per managed target, then a summary line of
the counters:

```text
clean     /home/u/.zshrc
drifted   /home/u/.gitconfig
missing   /home/u/.config/nvim/init.lua
orphaned  /home/u/.oldrc
clean: 1  drifted: 1  missing: 1  orphaned: 1
```

On a terminal the state word is green (clean), yellow (drifted), red
(missing), or magenta (orphaned). An orphan sits off that severity scale,
a leftover awaiting a reap, and gets its own hue. The summary paints only
a non-zero counter, so a clean repository reads at a glance. Every state
is in the text as well, and a stripped run loses the color alone.

## Commands

Beyond `apply`, `status`, `rollback`, and `debug journal`, Patina ships
commands for setting up a repository, migrating existing dotfiles into
management, and tracking remote sources. Two flags run across them:

- `--json` emits a structured JSON envelope in place of human-readable
  output. For read-only commands this is a pure formatting switch.
- `--yes` proceeds without the interactive confirmation prompt. The
  commands that overwrite or delete data (`remove`, `promote`, and
  `doctor --fix`) follow the same prompt semantics as `apply`: a bare
  invocation in an interactive terminal prompts before mutating; a
  non-interactive shell refuses to mutate unless you pass `--yes`.
  `init` and `add` gate differently. `init` writes unconditionally,
  refusing only where a manifest already exists, and accepts `--yes` for
  parity without acting on it. `add` prompts for an omitted mode or
  module, and only in an interactive terminal; a non-interactive shell
  refuses *those specific* missing inputs. Once mode and module are
  supplied, `add` writes without prompting.

`add` also accepts `--force`, which overrides its ignore-conflict refusal
(see [Excluding generated files](#excluding-generated-files-with-ignore)).
`--force` and `--yes` are separate axes: one overrides a refusal, the
other skips a prompt.

| Command   | Purpose                                                                                       |
| --------- | --------------------------------------------------------------------------------------------- |
| `init`    | Scaffold a root `patina.toml` and persist the default-repository pointer.                     |
| `add`     | Bring an existing dotfile under management: copy it into a module and write a `[[file]]` entry for a file source or a `[[directory]]` entry for a directory source.|
| `remove`  | Unmanage a target: drop its entry and replace the target with a regular file holding the last-applied content. |
| `promote` | Copy a drifted copy-mode target's current bytes back into its repository source, then re-apply. |
| `doctor`  | Inspect the environment for known problems (UNC repository paths, missing Windows Developer Mode, OS-too-old, missing default repo, missing `git`, targets a new `ignore` pattern stranded). |
| `remote`  | Manage remote git sources: `list` the pins, `check` upstream tips, `update` a pin through the update gate, `prune` cached checkouts. See [Remote sources](#remote-sources). |

`patina remove --purge` deletes the target outright, where a bare
`remove` leaves a regular file holding the last-applied content.

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
off, it offers a one-time elevation. A single UAC prompt appears, and
accepting it turns Developer Mode on through the bundled
`patina-elevate.exe` helper. Later runs need no prompt. Declining exits
`5` and points you at
`patina doctor --fix`, which offers the same remediation.

## Windows Defender exclusions

On Windows, Microsoft Defender scans file I/O in real time. A dotfiles
repository is a pile of small git objects, and `apply` reads and writes
many links and copies. That per-access scan costs you throughput over
paths you already trust. `patina defender` adds Defender **path
exclusions** for the repository and its deployed targets. The command is
Windows-only and stays out of `--help` on macOS and Linux.

An exclusion is a permanent hole in your antivirus coverage. `patina
apply` never opens one on its own. You run the command deliberately, you
see every path first, and you consent before anything changes:

| Command                   | Purpose                                                                             |
| ------------------------- | ----------------------------------------------------------------------------------- |
| `patina defender status`  | Show the current exclusions against the desired set. Read-only and unprivileged.    |
| `patina defender apply`   | Add every desired exclusion that is missing and remove the patina-owned ones the current plan no longer manages. |
| `patina defender clear`   | Remove every patina-owned exclusion.                                                |

### What Patina can see without administrator

`Get-MpPreference` returns the exclusion list only to an elevated caller.
Unelevated it reports `N/A: Must be an administrator to view exclusions`
and exits successfully, leaving nothing to compare against.

Patina then reports state from its own ledger and labels it as such:
`recorded` and `not recorded` in place of `present` and `missing`, under
a note saying where the state came from. `--json` marks the same
distinction as `current_readable: false`. Patina misses one case: an
exclusion you delete by hand in the Defender UI. Run `patina defender
status` from an elevated shell to catch it. That run reads the live list
and reports `present` or `missing` against it.

The desired set is the repository root plus **one** exclusion per
managed target: a folder exclusion for a directory entry
(`symlink` / `symlink-tree` / `copy`) and a file exclusion for a file
entry (`symlink` / `copy` / template). A `symlink-tree` of forty files
contributes its one declared target directory. Patina emits exact paths
only, with no wildcards and no process or extension exclusions, and it
declines a UNC path, a drive root, or a system directory
(`%SystemRoot%`, `%ProgramFiles%`, and friends).

### Reading the listing

The listing paints the exclusion kind onto the path itself and puts the
state in a colored tag after it. The tags share one column, sized to the
widest path: the states read straight down a long list.

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

`[present, not recorded by patina]` is worth acting on. The path is
already excluded, so `apply` leaves Defender alone for it, and
**`clear` skips it** because the ledger does not own it. You get this tag
when you excluded the path by hand, or when a Patina run applied it
without recording the result. Running `apply` adopts it: the ledger
converges on the whole desired set, the entry becomes `present`, and
`clear` can reverse it afterwards.

Spotting an unowned exclusion needs the live list, so the tag appears
only on an elevated run. Unprivileged you see the two ledger-derived
states instead.

Color is the only place the kind appears, and it goes wherever ANSI is
stripped: a pipe, a redirect, `--color never`, `NO_COLOR`. Use `--json`
when you need this as data. Every entry there names an explicit `kind`
(`file` or `folder`) and `state` (`owned`, `unmanaged`, `absent`,
`recorded`, `unrecorded`).

`apply` and `clear` preview the additions and removals, then prompt before
acting; a non-interactive shell requires `--yes`. Accepting raises one UAC
prompt (the main `patina.exe` never runs elevated, only a small bundled
helper does). Declining the prompt exits `5`.

The helper also verifies the change, since it is the only part of Patina
elevated enough to read the exclusion list back. It re-reads after
writing and records the verdict for the waiting `patina.exe`. Three
outcomes exit `1`, and each says something different:

| Outcome                                | What it means                                                                        |
| -------------------------------------- | ------------------------------------------------------------------------------------ |
| Defender rejected the change           | The write returned success and changed nothing. Usually Tamper Protection or a Defender managed by policy (Intune, GPO). Check `Get-MpComputerStatus`. |
| The helper could not apply the request | It never reached Defender: a path it refused, or a request file it could not read.     |
| The helper reported no result          | Nobody observed the outcome. The exclusions may have been applied without being recorded, so re-run `apply`, which is idempotent. |

Patina records only the exclusions it added, in a per-machine ledger.
`apply` therefore reaps a stale patina-owned exclusion and **never
touches a user-added one**. `clear` removes only what Patina owns.

On Windows 11, consider a [Dev Drive](https://learn.microsoft.com/windows/dev-drive/)
(ReFS) in Defender *performance mode*. It scans asynchronously rather
than skipping the scan, the lower-risk choice where it applies.

## Watch service

`patina watch` runs a per-user background watcher. It re-applies your
configuration when the source repository changes, and surfaces drift when
a managed target is edited outside Patina. Its default path needs neither
admin nor sudo.

The lifecycle subcommands manage a background service registered with
your OS supervisor:

| Command                  | Purpose                                                            |
| ------------------------ | ------------------------------------------------------------------ |
| `patina watch install`   | Register the watcher to launch at login. Exits 1 if already installed; run `uninstall` first to re-register. |
| `patina watch uninstall` | Stop the running watcher and remove the service registration.      |
| `patina watch start`     | Ask the supervisor to start the installed service.                 |
| `patina watch stop`      | Ask the supervisor to stop the service without removing it.        |
| `patina watch restart`   | Stop then start the installed service.                             |
| `patina watch status`    | Report the service's installed / running state, last-exit code, and the watcher's subscription and re-apply counters. Read-only. |

`patina watch --foreground` runs the watcher loop inline instead,
attached to the current terminal, and shuts down cleanly on Ctrl-C
(SIGINT) or SIGTERM. The installed background service runs that same
loop under your supervisor.

`install` writes a per-user service descriptor whose location depends on
the OS:

| OS      | Service descriptor                                      | Supervisor       |
| ------- | ------------------------------------------------------- | ---------------- |
| macOS   | `~/Library/LaunchAgents/com.patina.watcher.plist`       | `launchd`        |
| Linux   | `~/.config/systemd/user/patina-watcher.service`         | `systemd --user` |
| Windows | Scheduled Task named `Patina Watcher` (HKCU, logon trigger) | Task Scheduler |

### Linux caveats

A `systemd --user` service stops when you log out and starts again when
you next log in. To keep the watcher running across logout, say on a
server you SSH in and out of, enable lingering for your user once:

```sh
sudo loginctl enable-linger $USER
```

`patina watch install` targets `systemd --user`, so a distribution
without it (Void, Devuan with a non-systemd init, Alpine) gets no service
descriptor; run `patina watch --foreground` under runit, s6, or OpenRC
instead. [`OPERATING_ENVIRONMENT.md`](OPERATING_ENVIRONMENT.md) covers
both cases, including why Patina runs `enable-linger` for nobody.

### Drift notifications

When a non-symlink managed target changes, the watcher hashes it and
compares the result against the hash recorded at the last apply. Those
targets are copy-mode files, copied directory trees, and rendered
templates. On divergence it emits a desktop notification titled
"Patina: drift detected" naming the target, and records the event in a
drift cache at `<state>/patina/drift.cache`. Notifications are
rate-limited to at most one per target per 60-second window. Editing a
symlinked target is editing the source, which the source watcher already
catches, so symlinks stay out of the drift hashing.

Drift surfaces two ways:

- As the desktop notification above, **only while the watcher is
  running**.
- As `drifted` in `patina status`, **always**. `patina status` decides
  drift by re-hashing the target live, independent of the watcher. A
  file you edit and then revert to its recorded bytes therefore reports
  `clean`, even though the watcher logged the intervening edit. The
  drift cache is the watcher's own notification ledger, and
  `patina status` never reads it.

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
key resolves against its module directory.

`patina.lock` records the commit each machine materializes. It sits next
to your root `patina.toml` and is committed like any other file. A remote
update therefore moves like any other dotfile change: you bump the pin on
the machine you work from, commit it, and every other machine catches up
with `git pull && patina apply`.

Bumping a pin is the moment third-party code changes what lands on your
machines, so `patina remote update` slows it down. A candidate commit
must not be dated in the future, must descend from the pin you already
have, must be no older than the pin's own timestamp, and must be at least
`min_age` old (72 hours unless you say otherwise). Every byte then passes
the ordinary diff-and-prompt loop before it reaches your filesystem.
Patina never renders remote content as a template, and never reads it as
configuration.

Cached checkouts live in the per-machine [state
directory](#state-directory) and are pruned automatically once no
journal record needs them.

Read [`REMOTE_SOURCES.md`](REMOTE_SOURCES.md) for the whole model: the
lockfile format, the cache layout, each gate check with what it can and
cannot stop, the shell snippets for the background update notice, and the
multi-machine flow.

## State directory

Patina writes its journal, backups, advisory lock, and drift cache to a
**per-machine state directory** outside your dotfiles repository, at
`~/.local/state/patina/` on Linux, `~/Library/Application
Support/patina/` on macOS, and `%LOCALAPPDATA%\patina\` on Windows.

Both that directory and your dotfiles repository must sit on local disk.
A cloud-sync mount (iCloud Drive, OneDrive, Dropbox, Box, Google Drive,
Syncthing) queues and versions writes behind your back, and that breaks
the crash-safety guarantee under Recovery. **Patina does not detect one
in v1.0.** See
[`OPERATING_ENVIRONMENT.md`](OPERATING_ENVIRONMENT.md) for the directory
layout, the `XDG_STATE_HOME` override, and what each failure mode looks
like.

## Recovery

An interrupted apply converges deterministically on the next run. Kill
`patina apply` mid-write and the filesystem ends up in either the
pre-apply or the post-apply state; the next invocation reads the journal
and rolls forward or back to reach a consistent one. That covers process
termination (a `kill -9` or crash where the page cache survives). A power
loss or kernel panic mid-apply is out of scope for v1.0.

Two commands recover deliberately:

- `patina status` reports drift between what your configuration
  declares and what is currently on disk.
- `patina rollback` reverses the last successful apply by restoring the
  pre-apply bytes recorded in the journal. Afterwards the filesystem
  matches the pre-apply state in content and entry kind (file, symlink,
  or directory), modulo mode/timestamp bits and files you edited outside
  Patina.

For a post-mortem, `patina debug journal <path>` decodes the binary
journal into human-readable form, showing what the interrupted or
completed apply intended to do. `patina debug drift-cache <path>` does
the same for the watcher's binary drift cache
(`<state>/patina/drift.cache`): the version envelope, the journal
timestamp the cache is bound to, and one block per recorded divergence,
each naming the target path, the expected and actual hashes, and the
detection time. Both refuse a file written by a newer Patina with a typed
error naming the version mismatch, and both exit 1 on an invalid path.

## Troubleshooting

- **`patina apply` writes nothing and only prints a plan.** Apply falls
  through to plan-only when stdin is not a TTY. Run it in an interactive
  terminal to get the confirmation prompt.
- **Symlink creation fails on Windows.** Enable Developer Mode, or run
  the command from an elevated (UAC) session.
- **A template render fails with an undefined-variable error.** Patina
  uses strict-undefined semantics on purpose. Define the variable in the
  appropriate scope or profile; there is no empty default to fall back
  on.
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
