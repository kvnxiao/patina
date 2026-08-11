# Local and Remote Sources

Patina materializes declarations from two kinds of sources. A **local
source** is a path inside your dotfiles repository, the model the rest
of the documentation describes. A **remote source** is a path inside
someone else's git repository that Patina clones, pins, and keeps
current for you: a third-party skill or prompt library you want
deployed like a dotfile without hand-copying it on every upstream
change.

This page describes the remote model end to end: how a module declares
a remote, where the content is cached, how updates are gated against
supply-chain risk, and how several machines share one deterministic
view of it all through a committed lockfile.

## Local sources

A local source is what `[[file]]` and `[[directory]]` entries declare
everywhere else in these docs: a path relative to the module directory,
materialized as a symlink, rendered template, or byte copy. See
`docs/USER_GUIDE.md` "Declaring dotfiles" for the entry kinds and
modes. Everything on this page is additive; a repository with no remote
modules behaves exactly as before.

## Remote-backed modules

A module becomes remote-backed by carrying a `[remote]` table in its
`patina.toml`. Every entry source in that module then resolves against
a cached checkout of the remote repository instead of the module
directory:

```toml
# humanizer/patina.toml — a remote-backed module
[remote]
url = "https://github.com/blader/humanizer"
ref = "main"          # optional; defaults to the remote's default branch
min_age = "0s"        # optional; overrides the global update gate

[[directory]]
source = "skills/humanizer"          # path inside the remote repository
target = "~/.claude/skills/humanizer"
mode = "copy"
```

Selecting a subset of the remote is nothing new: you declare entries
for exactly the files and directories you want, and the rest of the
repository never leaves the cache. All the existing entry machinery
works unchanged: `mode`, `when`, `target` / `targets`, module
`[variables]`, and `[[hook]]` entries you author yourself.

One module maps to one remote. To consume two repositories, declare
two modules. The module name doubles as the remote's name in the
lockfile and the cache.

The global default for the update gate lives in the root manifest:

```toml
# patina.toml (root)
[remotes]
min_age = "72h"       # the shipped default when the table is absent
```

Durations accept `s`, `m`, `h`, and `d` suffixes (`"0s"`, `"30m"`,
`"72h"`, `"7d"`).

## Trust boundaries

Remote content is third-party input, and Patina holds four lines:

- Patina never reads configuration out of a checkout. A `patina.toml`
  inside the remote repository is inert bytes. Mappings, hooks, and
  variables come only from manifests in your own repository.
- Remote sources are never templates. A `.tmpl` suffix on a remote
  source gets no implicit render; the file is plain bytes under the
  declared mode. Third-party files full of `{{ }}` would otherwise
  explode under strict-undefined rendering, or worse, render.
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
- A remote-backed module with no lock entry is an error at plan time;
  the message points at `patina remote update <name>` to create the
  first pin.

## The remote cache

Checkouts live in the per-machine state directory, never in your
repository:

```
<state>/remotes/
├── notice                       plain-text pending-update notice
├── last_check                   background-check throttle stamp
└── <module>/
    ├── repo.git/                bare fetch repository
    └── <sha>/                   immutable checkout, one per pinned rev
```

Git runs as a subprocess (`git` on `PATH`, verified by
`patina doctor`), so your existing authentication (SSH agent,
credential helpers) works untouched. Fetches are shallow by exact
SHA. Each pinned rev gets its own immutable checkout directory, so an
update never mutates content behind a live symlink: apply re-points
links to the new checkout under the ordinary journaled flow, and
`patina rollback` can re-point them back.

After each successful apply, checkouts that no journal record on disk
references are pruned automatically: rollback always has what it
needs, and disk stays bounded at roughly the current and previous rev
per remote. `patina remote prune` runs the same sweep by hand.

## Commands

The verbs split along a producer/consumer line:

| Command                      | Role     | Purpose                                                                 |
| ---------------------------- | -------- | ----------------------------------------------------------------------- |
| `patina apply`               | consumer | Converge this machine to the committed lock. Fetches any pinned rev missing from the cache (by exact SHA, no gate), then the normal diff-and-prompt. |
| `patina remote update [name]`| producer | Fetch upstream, run the update gate, and bump `rev` / `updated_at` in the working-tree lockfile for you to review and commit. Touches no targets. |
| `patina apply --update`      | producer | `remote update` for every remote, then apply, in one sitting.           |
| `patina remote list`         | either   | Each remote's URL, ref, pinned rev, and pending-update state. Read-only. |
| `patina remote check`        | either   | `git ls-remote` only: compare upstream tips against the lock, refresh the notice file. No object download. |
| `patina remote prune`        | either   | Remove cached checkouts unreferenced by any journal record.              |

Failure shapes worth knowing:

- Plain `apply`, offline, warm cache: works fully.
- Plain `apply`, offline, cold cache: a typed error naming the remote
  and the missing rev. Nothing is partially applied.
- `apply --update`, offline: degrades to plain `apply` with a warning;
  pins are left unchanged.
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
   old (per-remote override, else the `[remotes]` global, else 72
   hours).

The first pin of a newly declared remote is exempt from the age gate:
adopting a remote is a deliberate act whose content you are about to
review in the consent diff. The gate exists to slow down *unattended*
pin bumps. `--now` bypasses the age gate for one run, with a visible
warning.

One limit must be stated plainly: committer timestamps are authored
by whoever makes the commit. The checks stop untargeted, fast-moving
compromises, the common case where attackers race detection windows
and publish with honest timestamps. An attacker who backdates a
commit specifically to defeat this gate will pass it. Plain git
offers no unforgeable, machine-independent time source. The
diff-and-prompt loop remains the hard boundary in front of every
byte.

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
`patina status` surfaces the same pending-update state.

## Target collision validation

Remote trees multiply a risk that always existed: two entries
resolving to the same target, or a directory-mode entry silently
planting files over another entry's target. An upstream repository
can grow files you never anticipated into a tree you deploy.

Patina validates the plan before showing a diff, over the active
entry set. Validation runs after `when` filtering, so two entries
targeting the same path under mutually exclusive `when` guards are legal:

- Two active entries resolving to the same canonical target is an
  error.
- An active directory-mode entry whose target contains another active
  entry's target is an error.

Both apply equally to local and remote entries, and both fail the run
before anything is written.
