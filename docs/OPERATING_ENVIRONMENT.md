# Operating environment

Patina stores per-machine state in the locations below. In v1.0, keep the
state directory and repository off cloud-sync mounts, and use
`systemd --user` lingering when the watcher must survive logout.

---

## Where Patina stores state

Patina writes its journal, backups, lock file, and drift cache to a
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
├── logs/                rotating watcher logs, created lazily by `patina watch`
├── default_repo         persisted dotfiles repo pointer (UTF-8 text)
├── profile              persisted profile name (UTF-8 text)
├── lock                 advisory file lock (fs2)
└── drift.cache          postcard-encoded drift events written by `patina watch`
```

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

---

## Linux: surviving logout with `loginctl enable-linger`

By default, `systemd --user` services (including the `patina watch`
service installed by `patina watch install`) stop when your last
login session ends. If you SSH into a server, run
`patina watch install`, then SSH out, the watcher dies with your
session.

**Patina does not invoke `loginctl enable-linger` for you in v1.0.** The
main `patina` process runs unprivileged and never prompts for sudo. Users
who need the watcher to survive logout must enable lingering separately. A
`--linger` flag is a v1.1 candidate.

### When you need lingering

Enable it on a machine you SSH into intermittently, or on one that should
run the watcher across reboots without a console login. Skip it on a
desktop or laptop where you stay logged in, and on any machine where you
only ever run `patina apply` by hand.

### How to enable it

One shot, requires sudo:

```sh
sudo loginctl enable-linger $USER
```

Verify:

```sh
loginctl show-user $USER | grep Linger
# Linger=yes
```

To disable later:

```sh
sudo loginctl disable-linger $USER
```

`patina watch uninstall` does **not** call `disable-linger`. Both
commands need sudo, and Patina commands run unprivileged.

### Without systemd

`patina watch install` writes a `systemd --user` unit. A non-systemd
Linux (Void, Devuan with sysvinit, Alpine without OpenRC-systemd parity)
cannot run one. Run the watcher inline with `patina watch --foreground`
under your own supervisor instead: runit, s6, or OpenRC. Templates for
other init systems are a v1.1 candidate.
