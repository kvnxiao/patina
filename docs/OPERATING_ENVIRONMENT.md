# Operating environment

Patina stores per-machine state in the locations below. In v1.0, keep the
state directory and repository off cloud-sync mounts.

---

## Where Patina stores state

Patina writes its journal, backups, and lock file to a
**per-machine state directory**. Your dotfiles repository is never
written to during `patina apply`.

| OS      | State directory                              | Override                  |
| ------- | -------------------------------------------- | ------------------------- |
| Linux   | `~/.local/state/patina/`                     | `$XDG_STATE_HOME/patina/` |
| macOS   | `~/Library/Application Support/patina/`      | (none in v1.0)            |
| Windows | `%LOCALAPPDATA%\patina\`                     | (none in v1.0)            |

Layout under the state directory:

```
patina/
├── journal/             postcard-encoded plan + COMMIT/ROLLED_BACK sentinels
├── backups/<ts>/        last-applied byte content, last 10 cycles retained
├── recovered/<ts>.<n>/  entries recovery or rollback replaced, last 10 passes retained
├── remotes/             bare remote repositories and immutable checkouts
├── default_repo         persisted dotfiles repo pointer (UTF-8 text)
├── profile              persisted profile name (UTF-8 text)
└── lock                 advisory file lock (fs2)
```

Each backup path mirrors an absolute target below `backups/<ts>/`. On Windows,
a drive prefix becomes its drive letter: `C:\Users\u\.gitconfig` maps to
`C/Users/u/.gitconfig`. A UNC prefix becomes
`__unc__/<host>/<share>`, which preserves both UNC boundaries in the backup
tree.

Before recovery overwrites or removes an entry at a target of an interrupted
apply, it copies that entry to `recovered/<ts>.<n>/<index>/<name>`. `<ts>` is
the interrupted apply's timestamp, `<n>` is one past the highest number already
used for that timestamp (starting at 1), `<index>` is the operation's position
in the apply's plan, and `<name>` is the target's file name. A recovery retried
after a failure reuses the earlier copy of an entry that has not changed since
that copy, and reports both the earlier copy and a new one for an entry that
has changed.

Before `patina rollback` replaces or deletes an entry that differs from the
rolled-back apply's record, it copies that entry into the same layout. `<ts>`
is then the rolled-back apply's timestamp, and `<index>` is the target's
position in the apply's record. For a target the apply removed, `<index>` is
the number of targets the record lists plus the target's position in the
record's list of removed targets.

The directory is separate from `backups/`, so no apply's backup
cycle can overwrite a kept copy. Each successful apply prunes all but the ten
newest `recovered/` directories, ordered by timestamp and then by `<n>` as a
number, as it does for `backups/`.

---

## Don't put state or your repo on a cloud-sync mount

**Patina does not detect cloud-sync directories in v1.0.** Nothing warns
you, nothing refuses, and `patina doctor` says nothing either. Every
detection strategy is incomplete or intrusive: a hardcoded provider
list becomes stale, while process inspection and filesystem xattr probing
inspect more of the machine than Patina requires.

You are responsible for keeping the **per-machine state directory**
and your **dotfiles repository** off the following kinds of mounts:

- iCloud Drive (`~/Library/Mobile Documents/`)
- OneDrive (`~/OneDrive`, `~/OneDrive - <org>`)
- Dropbox (`~/Dropbox`)
- Box / Box Sync (`~/Box`, `~/Box Sync`)
- Google Drive (Drive File Stream, Drive for Desktop)
- Syncthing-managed directories
- Any FUSE-backed cloud mount with deferred uploads

### Cloud-sync failure modes

Patina's crash-safety guarantee depends on the journal being written
atomically and surviving a `kill -9`. Cloud-sync providers route file
writes through their own queueing layer. Your local `fsync` returns
before the provider finishes uploading, and the provider may rename,
version, or delay files in ways Patina cannot observe:

- **Backups can be silently versioned.** Restoring the last-applied
  bytes then has no deterministic answer.
- **Journal files can appear out of order** during recovery if the
  provider reorders uploads, breaking the per-operation cursor.
- **The advisory file lock** (`fs2` over `flock(2)` /
  `LockFileEx`) is not well-defined on cloud-mounted filesystems;
  two `patina apply` invocations could interleave.

The repository has a separate failure mode. On Windows a long-running upload
holds the source file open with exclusive sharing semantics, racing
`patina apply`'s reads.

### What to do instead

Pick a local-disk directory for both:

```sh
# Linux/macOS
mkdir -p ~/dotfiles
git clone <your repo> ~/dotfiles

# Windows (PowerShell)
New-Item -ItemType Directory -Path C:\Users\<you>\dotfiles -Force
git clone <your repo> C:\Users\<you>\dotfiles
```

The state directory is on local disk by default. Moving it to a cloud-sync
mount requires overriding `XDG_STATE_HOME`.
