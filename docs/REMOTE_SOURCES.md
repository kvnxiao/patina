# Local and remote sources

Patina materializes declarations from two kinds of sources. A **local
source** is a path inside your dotfiles repository, the model the rest
of the documentation describes. A **remote source** is a path inside
someone else's git repository that Patina clones, pins, and keeps
current for you: a third-party skill or prompt library you want
deployed like a dotfile without hand-copying it on every upstream
change.

This page describes the remote model end to end. It covers how the root
manifest declares a remote, how an entry selects one, and where the
content is cached. It also covers how updates are gated against
supply-chain risk, and how several machines share one deterministic view
through a committed lockfile.

## Local sources

A local source is what `[[file]]` and `[[directory]]` entries declare
everywhere else in these docs: a path relative to the module directory,
materialized as a symlink, rendered template, or byte copy. See
`docs/USER_GUIDE.md` "Declaring dotfiles" for the entry kinds and
modes. Everything on this page is additive; a repository that declares
no remotes is unaffected by any of it.

## The remote registry

Every remote is declared once, in the root `patina.toml`, as a
`[[remote]]` table:

```toml
# patina.toml (root)
[patina]
root = true
remote_min_age = "72h"   # optional; the shipped default for the update gate

[[remote]]
url = "https://github.com/blader/humanizer"
ref = "main"             # optional; defaults to the remote's default branch
min_age = "0s"           # optional; overrides remote_min_age for this remote
# name = "humanizer"     # optional; derived from the URL
```

A remote's **name** is what entries refer to it by. It also keys the
remote's pin in the lockfile, its directory in the cache, and every
`patina remote` verb. Write `name` outright, or let Patina take it from
the last path segment of the URL with any trailing `.git` removed.
That gives `humanizer` for all of
`https://github.com/blader/humanizer.git`,
`git@github.com:blader/humanizer`, and `/srv/mirrors/humanizer.git`. A
name may contain letters, digits, `.`, `_`, and `-`, because it becomes
a directory name; a URL with no legal last segment is refused with a
message telling you to write `name`. For the same reason a name may not
end in a dot, and may not be a DOS device name. The device names are
`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, and `LPT0`-`LPT9`, with or
without an extension. Windows resolves both shapes to something other
than the directory you asked for, and `notice.` would land on Patina's
own notice file. Those are refused on every platform, so manifest
validity never depends on which machine reads it. Two remotes may not
answer to one name. The comparison ignores case and Unicode
normalization, so a manifest cannot mean two things on Linux and one
thing on macOS. Every
reference is matched the same way: an entry's `remote` key, a
`patina remote update <name>` argument, and a `patina.lock` key all
find a declaration whose spelling differs only in case. The names
`notice`, `pending`, and `last_check` are reserved (in any case): they
name Patina's own files beside the per-remote cache directories.

Durations accept `s`, `m`, `h`, and `d` suffixes (`"0s"`, `"30m"`,
`"72h"`, `"7d"`).

## Selecting a remote from an entry

A managed entry sources from a declared remote by naming it:

```toml
# agent-configs/patina.toml
[[file]]
source = "shared/AGENTS.md"          # no `remote` key: this module's own tree
target = "~/.claude/CLAUDE.md"

[[file]]
source = "SKILL.md"                  # a path inside the humanizer checkout
remote = "humanizer"
target = "~/.claude/skills/humanizer/SKILL.md"
```

An entry with no `remote` key resolves its source against its module
directory. An entry with one resolves against the
cached checkout of that remote's pinned rev, so its module directory
contributes only the manifest line. The two live side by side in one
manifest, and one manifest may draw on several remotes.

Selecting a subset of a remote is nothing new: you declare entries for
exactly the files and directories you want, and the rest of the
repository never leaves the cache. All the existing entry machinery
works unchanged: `mode`, `when`, `target` / `targets`, module
`[variables]`, and `[[hook]]` entries you author yourself.

Naming a remote no `[[remote]]` table declares is an error at plan
time, before anything is written.

A module manifest may not declare a remote of its own. Patina rejects a
module-level `[remote]` table by name rather than ignoring it, because
ignoring it would silently resolve that module's entries against the
module directory.

## Trust boundaries

Remote content is third-party input, and Patina holds these lines:

- Patina never reads configuration out of a checkout. A `patina.toml`
  inside the remote repository is inert bytes. Mappings, hooks, and
  variables come only from manifests in your own repository.
- Remote sources are never templates. A `.tmpl` suffix on an entry that
  names a remote gets no implicit render; the file is plain bytes under
  the declared mode. Third-party files full of `{{ }}` would otherwise
  explode under strict-undefined rendering, or worse, render. The rule
  is per entry, so a local `.tmpl` still renders in the same manifest.
- A remote source may supply only bytes from within its own checkout.
  An entry whose source resolves outside the checkout is refused at plan
  time. That covers both a `..` in the declared source and a symbolic
  link the checkout ships. Symlinks in a checkout are materialized as
  inert files holding their target text, so the resolver cannot follow
  one out of the checkout. Should a cached checkout hold a real
  symbolic link anyway, a directory source containing one fails the
  plan rather than deploying through it. Patina never writes such a
  link, so its presence means the cache was made or altered by
  something else.
- A remote's `url` and `ref` are passed to `git` as positional
  arguments, and may not begin with `-`. A manifest therefore cannot
  smuggle a git option (for example `--upload-pack`) into a fetch.
- Every byte still passes the consent diff. Remote updates reach your
  filesystem only through the same diff-and-prompt loop as local
  changes.
- Pin bumps are gated by age; see "The update gate" below, including
  an honest statement of what the gate cannot stop.

## The lockfile

`patina.lock` lives next to the root `patina.toml` and is committed to
your repository. It is the single shared statement of which remote
commit every machine applies:

```toml
version = 1

[remotes.humanizer]
url = "https://github.com/blader/humanizer"
ref = "main"
rev = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"
updated_at = "2026-08-11T14:00:00Z"
```

- `rev` is the full commit SHA every machine materializes. `apply`
  reads nothing else from the entry, so plan output stays
  deterministic and byte-identical across machines and runs.
- `updated_at` (RFC 3339, UTC) records when the pin was last bumped.
  It is written only by `patina remote update` and rides the same
  commit as the `rev` change, so the lockfile never needs a commit of
  its own. Its sole consumer is the update gate's backdating check.
- An entry naming a remote with no lock entry is an error at plan time;
  the message points at `patina remote update <name>` to create the
  first pin.
- Two `[remotes.<name>]` tables that address one remote are refused
  rather than resolved. Patina's own writer replaces a pin instead of
  joining a second one. A folded-equivalent pair therefore means a
  hand-edit or an unfinished merge. Guessing which one wins would apply a
  different commit than the machine that wrote the file.

The file is written through a same-directory temporary and a rename. A
process killed mid-write therefore leaves either the old pins or the new
ones, never a truncated file with no pins at all.

The lockfile is a statement about the root manifest's declarations, not
about what this machine happens to use. Two consequences follow.
`patina remote update` with no argument covers **every** declaration,
including one no entry currently names. The committed lock therefore
stays complete for machines whose active entries differ from yours. And a
pin whose `[[remote]]` you deleted is stale by definition: a
`patina apply` that may write drops it and says so. A preview reports the
stale pin and leaves the file alone, because a preview never writes your
repository. A preview here means a non-interactive apply without `--yes`,
or any `--json` run.

Reading `patina.lock` is itself lazy: it happens on the first entry
that selects a remote, so a repository that uses none never reads (or
fails on) a stray lockfile.

## The remote cache

Checkouts live in the per-machine state directory, never in your
repository:

```
<state>/remotes/
├── notice                       plain-text pending-update notice
├── pending                      the same, one remote name per line
├── last_check                   background-check throttle stamp
└── <remote name>/
    ├── repo.git/                bare fetch repository
    └── <sha>/                   immutable checkout, one per pinned rev
```

A checkout is materialized on demand. The first entry that actually
selects a remote on this machine fetches it. A remote that only a
`when`-false entry names is never fetched at all. Pins are global,
checkouts are local.

Materialization happens at plan time, because the plan is computed
from the checkout's bytes. A preview is therefore not offline and not
write-free in the strictest sense: a non-interactive apply without
`--yes`, and any `--json` run, will fetch and write a checkout the
cache lacks. What a preview never writes is your repository or any
target: the lockfile rewrites are held back for exactly that reason.

The directory under `<state>/remotes/` is named by the remote's
folded name (one case, one Unicode normal form), not by the spelling
in the manifest. Respelling a declaration therefore keeps addressing the
checkouts already on disk, instead of cold-starting a second tree.

Git runs as a subprocess (`git` on `PATH`, verified by
`patina doctor`), so your existing authentication (SSH agent,
credential helpers) works untouched. Fetches are shallow by exact
SHA. Each pinned rev gets its own immutable checkout directory, so an
update never mutates content behind a live symlink: apply re-points
links to the new checkout under the ordinary journaled flow, and
`patina rollback` can re-point them back.

A checkout is written with line-ending translation off, and with
external git attribute sources (system and per-user) neutralized. The
same pinned commit therefore materializes the same bytes on every
machine, whatever a user's `core.autocrlf` or `core.attributesFile` says. This does not
yet cover an in-tree `.gitattributes` shipped in the remote, which can
still apply an `eol` or `filter` rule; against such a repository a
checkout is not byte-verbatim. Fully attribute-blind materialization
is a post-1.0 item.

After each successful apply, the cache is swept automatically.
Checkouts that no journal record on disk references are removed. A
remote the root manifest no longer declares loses its whole cache
directory, bare repository included, once no journal record points
into it. Rollback always has what it needs, and disk stays bounded at
roughly the current and previous rev per remote. The checkout of each
declared remote's currently pinned rev always survives, referenced or
not: a pin bumped but not yet applied is the warm cache an offline
apply depends on. `patina remote prune` runs the same sweep by hand.

An apply where no active entry selects a remote never reads
`patina.lock` while planning, so the sweep re-reads it before deciding
anything. If that read fails, no declared remote's cache is touched at
all: a checkout that might be the current pin is worth more than the
disk it occupies.

## Commands

The verbs split along a producer/consumer line:

| Command                      | Role     | Purpose                                                                 |
| ---------------------------- | -------- | ----------------------------------------------------------------------- |
| `patina apply`               | consumer | Converge this machine to the committed lock. Fetches any pinned rev an active entry needs and the cache lacks (by exact SHA, no gate), then the normal diff-and-prompt. |
| `patina remote update [name]`| producer | Fetch upstream, run the update gate, and bump `rev` / `updated_at` in the working-tree lockfile for you to review and commit. With no name, covers every declaration. Touches no targets. |
| `patina apply --update`      | producer | `remote update` for every remote, then apply, in one sitting. Runs only when the apply may mutate: it is skipped (with a note) on a preview, meaning a non-interactive apply without `--yes`, or any `--json` run. It never auto-accepts a gate concern, even under `--yes`. |
| `patina remote list`         | either   | Each declared remote's URL, ref, pinned rev, and pending-update state. Read-only. |
| `patina remote check`        | either   | `git ls-remote` only: compare upstream tips against the lock, refresh the notice file. No object download. Exits non-zero if any remote could not be reached. |
| `patina remote prune`        | either   | Remove cached checkouts unreferenced by any journal record (currently pinned revs always stay), plus the cache tree of any undeclared remote. |

`patina remote list` prints a header and one row per declaration, each
column sized to its widest cell:

```text
NAME       REF               REV                                       URL
humanizer  main              1f0c6c9b9f2e8a1d4b7c0e3a5d8f2b6c9e1a4d70  https://github.com/example/humanizer
starship   (default branch)  (unpinned)                                https://github.com/starship/starship  (update pending)
```

A terminal additionally gets color. The name is cyan, a declared ref
bright yellow, a recorded rev green, and the URL bright blue.
`(unpinned)` and `(update pending)` are yellow, and a ref the manifest
left to the remote's own default is dim. Every one of those facts is in
the text
as well, so a piped run, `--color never`, and `NO_COLOR` lose the color
and nothing else. `--json` carries the same rows plus each pin's
`updated_at`.

Failure shapes worth knowing:

- Plain `apply`, offline, warm cache: works fully.
- Plain `apply`, offline, cold cache: a typed error naming the remote
  and the missing rev. Nothing is partially applied.
- `apply --update`, offline: degrades to plain `apply` with a warning;
  pins are left unchanged.
- `apply --update` on a preview (a non-interactive apply without
  `--yes`, or `--json`): the producer pass is skipped with a note, so
  the preview writes nothing; run `patina remote update` first.
- A gate concern under `apply --update`: never waved through by the
  apply's own `--yes`. It is prompted on a TTY or held otherwise; use
  `patina remote update --yes` to accept one explicitly.
- A remote still inside its cooldown window: `remote update` reports
  when the candidate becomes eligible and leaves the pin unchanged;
  the rest of the run proceeds.

## The update gate

Bumping a pin is the moment third-party code changes what lands on
your machines, so it is the moment Patina slows down. A candidate tip
must clear four checks, evaluated after fetching it:

1. **Future check.** A committer time more than one hour ahead of the
   local clock is a hard reject.
2. **Ancestry check.** A tip that is not a descendant of the pinned
   `rev` means upstream history was rewritten; Patina requires explicit
   confirmation. This catches force-pushes, not additive commits.
3. **Backdating floor.** A committer time earlier than the lock's
   `updated_at` is anomalous and prompts for confirmation. It is a
   prompt rather than a reject because one honest workflow trips it: a
   maintainer fast-forwarding a long-lived branch whose commits carry
   old committer dates.
4. **Age gate.** The tip's committer time must be at least `min_age`
   old (the remote's own override, else `[patina] remote_min_age`, else
   72 hours).

Declining a confirmation prompt leaves the pin where it is and exits
`5`, the code every Patina command uses for a declined prompt. A pin
the gate held back on its own, through a cooldown or a verdict this
binary does not recognize, exits `0` instead: nobody was asked, so
nothing was refused.

The first pin of a newly declared remote is exempt from the age gate:
adopting a remote is a deliberate act whose content you are about to
review in the consent diff. The gate exists to slow down *unattended*
pin bumps. `--now` bypasses the age gate for one run, with a visible
warning.

The gate has one limit worth stating plainly: committer timestamps are
authored by whoever makes the commit. The checks stop untargeted,
fast-moving compromises, the common case where attackers race detection
windows and publish with honest timestamps. An attacker who backdates a
commit specifically to defeat this gate will pass it. Plain git offers
no unforgeable, machine-independent time source. The diff-and-prompt
loop remains the hard boundary in front of every byte.

## Multi-machine flow

The lockfile makes remote updates flow through your repository like
any other dotfile change:

```mermaid
flowchart LR
    up["upstream remote repo"] -->|"remote update: gate + pin bump"| main["main machine"]
    main -->|"commit + push patina.lock"| repo["your dotfiles repo"]
    repo -->|"git pull"| other["any other machine"]
    other -->|"patina apply"| files["targets converge"]
```

A machine that sat idle for a week catches up with:

```sh
git pull && patina apply
```

One diff-and-prompt covers the accumulated changes. No gate math runs
on the consumer path, because a pinned rev is a decision you already
made and committed. Running `apply --update` on such a machine is safe (the
gate is machine-independent) but usually unwanted: it may bump pins
and strand an uncommitted lockfile change on a box you rarely touch.
Produce updates where you commit; consume everywhere else.

## Shell integration

The background check is notify-only and costs your prompt nothing.
`patina remote check --hook` self-throttles through the `last_check`
stamp (default: at most one real check per 24 hours) and maintains the
`notice` file; the shell side needs no logic beyond printing a file
and spawning one detached process per session, after the first
command rather than at startup.

A hook holds the shared lock only while it reads the manifest and the
lockfile, never across `ls-remote`. `git` has no timeout of its own. A
hook that kept the lock over the network could make a concurrent
`apply` wait out its own lock timeout. The `apply` would then fail
because a server went quiet.

fish (`conf.d/patina-remotes.fish`):

```fish
# Startup: print any pending notice. Builtins only; zero spawns.
for f in $XDG_STATE_HOME/patina/remotes/notice \
        $HOME/.local/state/patina/remotes/notice \
        $HOME/Library/Application\ Support/patina/remotes/notice \
        $LOCALAPPDATA/patina/remotes/notice
    if test -s "$f"
        while read -l line
            echo $line
        end < "$f"
        break
    end
end

# One deferred, detached check per session, after the first command,
# so the ~30 ms MSYS2 spawn never lands on the prompt path.
function __patina_remote_check --on-event fish_postexec
    functions -e __patina_remote_check
    patina remote check --hook &>/dev/null &
    disown
end
```

zsh (`.zshrc`):

```zsh
for f in ${XDG_STATE_HOME:-$HOME/.local/state}/patina/remotes/notice \
        "$HOME/Library/Application Support/patina/remotes/notice"; do
    [[ -s $f ]] && print -r -- "$(<$f)" && break
done
autoload -Uz add-zsh-hook
__patina_remote_check() {
    add-zsh-hook -d precmd __patina_remote_check
    patina remote check --hook &>/dev/null &!
}
add-zsh-hook precmd __patina_remote_check
```

PowerShell (`profile.ps1`):

```powershell
$notice = Join-Path $env:LOCALAPPDATA 'patina\remotes\notice'
if (Test-Path $notice) { Get-Content $notice }
Start-Process patina -ArgumentList 'remote','check','--hook' -WindowStyle Hidden
```

The notice distinguishes two situations. When upstream tips have moved
past your pins, it names the remotes and suggests
`patina apply --update`. When your own dotfiles repository is behind
its origin (the stale-server case: another machine already bumped the
pins), it says so and suggests `git pull && patina apply`
instead, since the pending changes are already decided and gated.
`patina status` surfaces the same pending-update state. A successful
`remote update` (including the one inside `apply --update`) drops the
remotes it settled from the notice on the spot. A stale announcement
therefore never outlives the bump it asked for.

## Target collision validation

Remote trees multiply a risk that always existed: two entries
resolving to the same target, or an entry silently planting files over
another entry's target. An upstream repository can grow files you never
anticipated into a tree you deploy.

Patina validates the plan before showing a diff, over the active
entry set. Validation runs after `when` filtering, so two entries
targeting the same path under mutually exclusive `when` guards are legal:

- Two active entries resolving to the same canonical target is an
  error.
- An active `[[directory]]` `mode = "symlink"` entry whose target
  contains another active entry's target is an error.

Both apply equally to local and remote-sourced entries, and both fail
the run before anything is written.

Only a whole-directory `symlink` owns everything under its target,
because it is the one mode that replaces the target path with a single
object. A `symlink-tree` or `copy` `[[directory]]` materializes one
object per source leaf, and journals each leaf as its own target. It
therefore claims those leaves and nothing between them. Another entry may
therefore deploy into a part of the same directory the tree does not
fill. That is what makes "add one upstream file to a directory my
repository also populates" expressible. Two entries writing one leaf is
still refused, naming the leaf and the directory target it came from.

The leaves are read from the source tree as it stands, so the verdict
depends on what the source currently holds: a file appearing upstream
under a tree source can newly fail a plan. That failure lands before
any write, which is what a tree growing an unexpected file should do.

Both comparisons ignore case and Unicode normal form, on every
platform. Windows and macOS resolve two targets differing only in case
to one file, and APFS does the same for two differing only in NFC/NFD
spelling. Linux resolves each pair to two files. Folding only where the
host needs it would let one manifest plan clean on Linux and fail on
macOS. The verdict belongs to the manifest, not the machine, so two
targets differing only in case or normalization are an error
everywhere; rename one.

Folding applies to the comparison only. Targets are created on disk in
the spelling the author wrote, because the programs that read them are
exact about their own paths.
