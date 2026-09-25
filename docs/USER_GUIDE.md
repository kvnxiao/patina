# Patina User Guide

Patina is a cross-platform dotfile manager whose source of truth is
your centralized git repository. You declare configuration in
`patina.toml` files and run `patina apply`; Patina materializes each
declaration at the right target as a symbolic link, rendered template
output, or byte copy.

## Installation

Patina is a single binary. Install it from crates.io with a current Rust
toolchain:

```sh
cargo install patina --locked
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

Spell each `target` absolutely or with a leading `~`; Patina expands `~` to
your home directory. Because `patina apply` resolves a relative `target`
against its current directory, the same relative spelling can name a
different file on each invocation. `patina add` writes a `~`-relative target
under your home directory and an absolute target elsewhere.

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

In a module manifest, `[variables]` is local to that module. Its values are
available to templates, `when` expressions, and hooks declared in the same
`patina.toml`. Other modules resolve the name from their own table or from a
broader layer, regardless of manifest discovery order.

```toml
# Root patina.toml: repo-shared defaults plus a per-profile override.
[variables]
editor = "nvim"

[profiles.work.variables]
editor = "code"
```

Profiles select the machine-specific variable set layered on top of the
repo-shared one.

Both `apply` and `status` accept repeated `-v key=value` overrides. Use the
same values when checking an apply:

```sh
patina apply -v machine=laptop
patina status -v machine=laptop
```

Without the override, `status` evaluates the configured value. It may report a
previously applied target as orphaned when `when` becomes false. If no lower
layer defines the variable, `status` returns an undefined-variable error.
During apply, the same override set drives both materialization and orphan
reaping.

`doctor` does not accept variable overrides. If its ignored-target check cannot
rebuild the managed set without them, it skips that finding.

## Apply flow

Run `patina apply` to materialize your declarations. Apply is a
diff-and-prompt loop by default:

1. **Plan.** Patina discovers your repository, parses every
   `patina.toml`, resolves variables and the active profile, and
   renders templates into a concrete list of operations.
2. **Diff.** Patina compares the planned end-state against what is
   actually on disk and prints the diff. A target a prior apply
   materialized, but the current plan no longer manages, shows as a
   `remove <target>` block. The block covers an entry you dropped from a
   `patina.toml` and one whose `when` is now false. Patina backs the
   target up and deletes it on apply, inside the consent diff like every
   other change. One exception: a dropped target that is now inside
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

### Apply hooks

Declare a `[[hook]]` to run a shell command before or after apply writes its
targets:

```toml
[[hook]]
event = "post_apply"
command = "fc-cache -f"
when = "patina.os == 'linux'"
```

After an earlier apply has committed, a run with no target changes or orphans
returns before the hook phase. For every run that reaches the hook phase,
Patina first resolves all hook shells on `PATH`. An unknown shell aborts before
the planned target changes.

When an apply reaches the hook phase, Patina uses this order:

1. Run `pre_apply` hooks.
2. Write the journal and materialize the planned changes.
3. Run `post_apply` hooks.
4. Commit the apply.

Hook failures stop the sequence when `must_succeed = true`, which is the
default. A failed `pre_apply` hook exits `2` before file operations begin. A
failed `post_apply` hook rolls back the file operations and exits `3`. Set
`must_succeed = false` on one hook, or pass `--force-deploy` to downgrade every
hook failure for that invocation to a warning.

An optional `when` expression uses the variables from the hook's manifest.
The optional `shell` field replaces the platform default: `bash` on macOS and
Linux, or `pwsh` on Windows. Patina passes `command` to the selected shell
verbatim; it does not render the command as a template.

A hook's stdout goes to Patina's stdout, and its stderr to Patina's stderr.
Under `patina apply --json`, Patina sends each hook's stdout to Patina's stderr
instead, so stdout contains only the JSON document.

### Changing an entry's mode

Editing an entry's `mode` from `symlink` to `copy`, or a `[[directory]]`
entry from `symlink` to `symlink-tree`, converges in one apply; the next
apply is a no-op. `patina apply` compares the target's entry kind as well
as its content, so a symlink where a copy is declared is planned as work
even when the linked bytes match.

A mode edit on an existing entry previews as a `replace` block naming
both kinds:

```text
replace /home/u/.zshrc (symlink -> file)
  - (symlink -> /repo/zsh/zshrc)
  + (text, 27 bytes)
```

A whole-directory `symlink` switched to a tree mode replaces the root as
one block, `replace <target> (symlink -> tree)`, with the leaf count on
the inserted side and no per-leaf blocks.

The `replace` verb identifies a mode edit when the last committed apply
materialized the same target from the same source under a different kind.
When a deleted entry's target is claimed by a different entry from a
different source, the diff keeps the plain mode verb and uses a kind-aware
body. A `copy` or `render` over a live symlink shows
`- (symlink -> <link>)` rather than a content diff read through the link.
A `symlink` over a live regular file shows the file's size descriptor
rather than `(absent)`.

When a symbolic link is inside a managed tree, between the declared
target directory and its files, Patina stops the apply with an error
naming the link. Patina keeps those intermediate directories real and
never writes through a link planted there; remove the link and re-run
`patina apply`. Do not hide the link with an `ignore` pattern: previously
deployed files under the link become removable orphans, and the removal
resolves through the link. A symlink in the path *above* the declared
target (say a linked `~/.config`) is fine and resolves as usual.

On a terminal the diff is colorized: green additions, red removals, bold
entry headers, styled warnings and errors. The confirmation prompt is
colorized too: a green affirmative `y` and a red default `N`, each set
apart from the prose and from the other. Piped
or redirected output is always plain. Stdout therefore stays
byte-identical between runs. The `--color` flag (global, before or after any
subcommand) forces the choice: `auto` (the default) colors a terminal and
strips otherwise, `always` colors even when piped, `never` disables
color. `NO_COLOR` in the environment is honoured under `auto`.

Every multi-row listing lines its columns up the same way, sized to the
widest cell: `patina status`, `patina remote list`, `patina remote
update`, `patina doctor`, and the Defender listing. Painted cells pad by
printable width, so a piped run and a terminal run stay aligned
identically.

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
(missing), or magenta (orphaned). The summary includes only non-zero
counters. Every state also appears in the text, so a stripped run loses
only the color.

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
| `remove`  | Drop a managed target and preserve its applied contents as a regular file. Individual tree-mode leaves cannot be removed with this command. |
| `promote` | Copy a changed copy-mode target back to its repository source and record its bytes as applied. Remote-backed targets cannot be promoted. |
| `doctor`  | Inspect the environment for known problems (UNC repository paths, missing Windows Developer Mode, an outdated Windows build, a missing default repo, missing `git`, and targets stranded by a new `ignore` pattern). |
| `remote`  | Manage remote git sources: `list` the pins, `check` upstream tips, `update` a pin through the update gate, `prune` cached checkouts. See [Remote sources](#remote-sources). |

`patina remove --purge` deletes the target outright, where a bare
`remove` leaves a regular file with the last-applied content.

`remove` and `promote` save the current managed state without running an apply:

- `remove` replaces or deletes its target and edits the manifest that declares it.
- `promote` writes the target's repository source.
- Neither runs hooks, rewrites other drifted targets, or creates targets for
  new entries. Those changes remain in the next apply's diff.

Both commands record a checkpoint without waiting for the clock to advance.
If `remove` fails before committing, recovery restores its target and manifest.
After process termination, the next command that recovers restores them first;
running `remove` again can recover and retry even if the declaration was deleted.

`patina rollback` stops at the latest checkpoint from `remove` or `promote`.
It can reverse later applies but cannot undo those commands or earlier applies.
At the checkpoint, it leaves the managed state current and prints
`Nothing to roll back: reached the state saved by remove or promote.`
With `--json`, the result is `checkpoint`.

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

### Naming a path on the command line

`add`, `remove`, and `promote` interpret their path as a location on your
machine. A leading `~` expands to your home directory, and a relative path
resolves against the command's current directory. For example, `patina add
.wslconfig` from your home directory declares `~/.wslconfig` independently of
the repository location. `add` stores a `~`-relative target under your home
directory and an absolute target elsewhere.

For symlink and copy modes, the repository source preserves the target's file
name, including a leading dot: adding `.wslconfig` to module `wsl2` writes
`wsl2/.wslconfig` and records `source = ".wslconfig"`. Template mode writes
`wsl2/.wslconfig.tmpl` and records the appended `.tmpl` suffix.

If any filesystem entry occupies the derived repository source path, `add`
exits `1` and does not replace the entry or write the manifest declaration.
Choose a different module or remove the occupied repository source before
retrying.

If the path is your home directory, the repository, or an ancestor of the
repository, `patina add` exits `1`. A home-directory entry would claim every
file under it; staging the repository or an ancestor would recursively copy
the repository. Name the file or directory you want managed instead.

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
repository contains many small git objects, and `apply` reads and writes
many links and copies. Each access scan consumes throughput on
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
For an unelevated caller, it reports `N/A: Must be an administrator to view exclusions`
and exits successfully, leaving nothing to compare against.

Patina then reports state from its own ledger and labels it as such:
`recorded` and `not recorded` in place of `present` and `missing`, under
a note saying where the state came from. `--json` marks the same
distinction as `current_readable: false`. Patina misses one case: an
exclusion you delete by hand in the Defender UI. Run `patina defender
status` from an elevated shell to detect it. The status command reads the live list
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

The listing marks the exclusion kind on the path and puts the state in a
colored tag after it. The tags share one column sized to the widest path,
which aligns the states.

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

An unowned exclusion requires the live list, so its tag appears only on an
elevated run. An unprivileged run shows the two ledger-derived states
instead.

Color is the only place the kind appears, and it goes wherever ANSI is
stripped: a pipe, a redirect, `--color never`, `NO_COLOR`. Use `--json`
when you need this as data. Every entry there names an explicit `kind`
(`file` or `folder`) and `state` (`owned`, `unmanaged`, `absent`,
`recorded`, `unrecorded`).

`apply` and `clear` preview the additions and removals, then prompt before
acting; a non-interactive shell requires `--yes`. Accepting raises one UAC
prompt (the main `patina.exe` never runs elevated, only a small bundled
helper does). Declining the prompt exits `5`.

The helper also verifies the change because it is the only part of Patina
elevated enough to read the exclusion list back. It re-reads after
writing and records the verdict for the waiting `patina.exe`. Three
outcomes exit `1`, and each says something different:

| Outcome                                | What it means                                                                        |
| -------------------------------------- | ------------------------------------------------------------------------------------ |
| Defender rejected the change           | The write returned success and changed nothing. Usually Tamper Protection or a Defender managed by policy (Intune, GPO). Check `Get-MpComputerStatus`. |
| The helper could not apply the request | It never reached Defender: a path it refused, or a request file it could not read.     |
| The helper reported no result          | No result was reported. The exclusions may have been applied without being recorded, so re-run `apply`, which is idempotent. |

Patina records only the exclusions it added, in a per-machine ledger.
`apply` therefore reaps a stale patina-owned exclusion and **never
touches a user-added one**. `clear` removes only what Patina owns.

On Windows 11, consider a [Dev Drive](https://learn.microsoft.com/windows/dev-drive/)
(ReFS) in Defender *performance mode*. It scans asynchronously rather
than skipping the scan, the lower-risk choice where it applies.

## Resolving drift

On every run, `patina status` compares each managed target with the last
apply's journal record: a copied or rendered target must match the
recorded content hash, and a symbolic link must point at its recorded link
target. A file you edit and then revert to the bytes the last apply wrote
therefore reports `clean`.

Resolve a drifted target either way:

- `patina apply` reverts the target to the source content.
- `patina promote` updates a copy-mode target's source from the target's
  current bytes and records those bytes as applied, so `status` reports the
  target `clean`.

### Limits on promote and remove

Remote checkouts are immutable, so `promote` refuses a remote-backed target.
The error names the remote and exits `1`. Make the change upstream, then run
`patina remote update <name>`.

A tree-mode `[[directory]]` entry owns the tree as one manifest declaration.
`remove` cannot drop an individual `symlink-tree` or `copy` leaf because no
separate file entry exists for that leaf. The command names the declaring
manifest, exits `1`, and leaves the target unchanged. To stop managing the
leaf, add an `ignore` pattern or edit the directory entry.

When an interrupted apply is pending, `promote` reverts it before copying the
target. If that recovery changed the target, `promote` exits `1` without
writing the source: the target no longer has the bytes you asked to promote.
When the target existed before the recovery, the recovery's warning names the
copy it kept of the target's earlier contents.
Review the target, then run `promote` again.

## Remote sources

An entry can draw its source from someone else's git repository instead
of from your own. Declare the repository once in your root manifest, then
use its bytes from any entry that names it:

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

`patina.lock` records the commit each machine materializes. It is next to
your root `patina.toml` and is committed like any other file. A remote
update therefore moves like any other dotfile change: you bump the pin on
the machine you work from, commit it, and every other machine catches up
with `git pull && patina apply`.

Bumping a pin changes the third-party code that reaches your machines, so
`patina remote update` applies the update gate. A candidate commit
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

Patina writes its journal, backups, and advisory lock to a
**per-machine state directory** outside your dotfiles repository, at
`~/.local/state/patina/` on Linux, `~/Library/Application
Support/patina/` on macOS, and `%LOCALAPPDATA%\patina\` on Windows.

Both the state directory and your dotfiles repository must be on local disk.
A cloud-sync mount (iCloud Drive, OneDrive, Dropbox, Box, Google Drive,
Syncthing) queues and versions writes, which breaks the crash-safety
guarantee during recovery. **Patina does not detect cloud-sync mounts in
v1.0.** See
[`OPERATING_ENVIRONMENT.md`](OPERATING_ENVIRONMENT.md) for the directory
layout, the `XDG_STATE_HOME` override, and what each failure mode looks
like.

### Records from an earlier build

Patina reads only its current plan and commit-record layouts. These layouts
can change before v1.0 without a migration. A record that a pre-release build
wrote in an earlier layout does not decode, and Patina skips it as if that apply had never
committed:

- `patina status` does not report its targets.
- `patina rollback` does not reverse it.
- `patina remove` and `patina promote` treat its targets as unmanaged.
- `patina apply` does not remove a target that only an undecodable record
  lists.

Patina also does not remove unreferenced remote checkouts while an undecodable
record remains. An earlier plan layout may also prevent recovery. Before
upgrading across a layout change, do the following on each machine:

1. Use the old binary to recover any interrupted operation, for example with
   `patina apply --yes`. Resolve recovery errors before continuing.
2. Stop all Patina processes.
3. Delete the `journal/` directory inside the state directory. This discards
   earlier rollback history.
4. Upgrade and run `patina apply --yes` to record the managed targets again.

## Recovery

An interrupted apply or removal converges deterministically on the next command that
recovers. Kill `patina apply` mid-write and the next `patina apply --yes`,
interactive `patina apply`, `patina rollback`, `patina remove`, or
`patina promote` first reverts the interrupted apply to its pre-apply state,
then does its own work against those files. `rollback` reverts only after you
confirm or pass `--yes`. `remove` and `promote` revert only after you confirm
and their target checks pass; when a check refuses or you decline, they warn
that the interrupted apply is pending and change nothing. `promote` can still
refuse after the recovery, when the recovery changed the target it was asked
to promote. Each command reports a recovery on stderr with
`reverted an interrupted apply to the state before it started`. The guarantee
covers process termination (a `kill -9` or crash where the page cache
survives). A power loss or kernel panic mid-apply is out of scope for v1.0.

A preview does not recover, because it must not write: `patina apply` in a
non-interactive shell without `--yes`, `patina apply --json` without `--yes`,
and `patina status`. While an interrupted apply is pending, each warns on
stderr that its output describes the files as the interrupted apply left them,
and that an interactive `patina apply` or `patina apply --yes` reverts it
first. When another `patina` process holds the lock for longer than the
five-second shared-lock wait, the warning says instead that an apply is running
or was interrupted. The exit code does not change.

Recovery reverts only what the interrupted apply changed. A file that existed
before the apply gets its original bytes back from the backup Patina took before
overwriting or removing it, or stays untouched if the apply never reached it. A
file the apply created is deleted. Before recovery overwrites or deletes a file,
it copies what is there to the `recovered/` directory of the state directory
and prints `kept a copy of <target> from before the recovery at <path>`, so a
file you edited or created after the crash is not lost. When a recovery retried
after a failure finds a target changed since its earlier copy, it prints the
line for both copies, the earlier one first. See
[`OPERATING_ENVIRONMENT.md`](OPERATING_ENVIRONMENT.md) for the layout.

Two other commands inspect or undo an apply:

- `patina status` reports drift between what your configuration
  declares and what is currently on disk.
- `patina rollback` reverses the last successful apply by restoring the
  pre-apply bytes recorded in the journal, including each file the apply
  removed because your configuration no longer managed it. Afterwards the
  filesystem matches the pre-apply state in content and entry kind (file,
  symlink, or directory), modulo mode/timestamp bits. Rollback does not
  touch a file the apply found already up to date, so an edit you made to
  that file after the apply stays. Before rollback replaces or deletes a
  file that changed since the apply, it copies that file to the
  `recovered/` directory of the state directory and prints
  `kept a copy of <target> from before the rollback at <path>`. A file that
  you created where the apply had removed a file counts as changed.

Rollback stops at a checkpoint from `remove` or `promote`, as described above.
Patina retains the ten newest committed operations, including checkpoints and
applies that only create files, together with their backups. Pending operations
remain available for recovery.

For a post-mortem, `patina debug journal <path>` decodes a binary journal
file into human-readable form. Given a path ending in `.COMMIT`, it lists
each target the commit recorded and each target the apply removed. It
decodes any other path as a plan, such as a `<id>.plan` file, and shows what
the interrupted apply intended to do. It refuses a file written by a newer
Patina with an error that reports both major versions, and exits 1 on that
refusal, on a file it cannot decode, or on a missing or unreadable path.

## Troubleshooting

- **`patina apply` writes nothing and only prints a plan.** Apply falls
  through to plan-only when stdin is not a TTY. Run it in an interactive
  terminal to get the confirmation prompt.
- **Symlink creation fails on Windows.** Enable Developer Mode, or run
  the command from an elevated (UAC) session.
- **A template render fails with an undefined-variable error.** Patina
  does not substitute an empty value for an undefined variable. Define the
  variable in the affected module, in the root `[variables]` table for all
  modules, or in the active profile.
- **Apply seems to hang.** Another `patina` process may hold the
  advisory lock. Patina waits up to a bounded timeout and then exits
  with the lock-timeout exit code; check for a concurrent apply.
- **Recovery behaves unexpectedly after a crash.** Confirm your state
  directory is on local disk and not a cloud-sync mount (see "State
  directory"). Use `patina debug journal` to inspect the journal that
  recovery read.
- **A preview or `patina status` warns that an interrupted apply is
  pending.** An earlier apply was killed before it committed. Run
  `patina apply --yes` or an interactive `patina apply` to revert it and
  apply again.
- **Apply exits 1 with "an interrupted apply has not been recovered".**
  Another `patina` process was killed mid-apply after this one recovered
  and before it started writing. Re-run the command; it recovers first.
- **`patina status` reports `orphaned` after an apply with `-v`.** Pass the
  same variable overrides to `status`. Without them, the entry's `when`
  expression can evaluate differently. See [Variables](#variables).
