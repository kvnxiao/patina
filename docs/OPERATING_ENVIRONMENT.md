# Operating Environment

This page covers two operational footguns Patina deliberately does
**not** detect at runtime in v1.0. They live here so you can avoid
them rather than rediscover them through degraded apply behaviour.

Both topics are tracked as v1.1 candidates; detection may be added in a
future release if real users surface the failure modes.

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
├── default_repo         persisted dotfiles repo pointer (UTF-8 text)
├── profile              persisted profile name (UTF-8 text)
├── lock                 advisory file lock (fs2)
└── drift.cache          postcard-encoded drift events written by `patina watch`
```

---

## Don't put state or your repo on a cloud-sync mount

**Patina does not detect cloud-sync directories in v1.0.** No
warning, no refusal, no doctor check. The detection was considered
and explicitly removed because every detection strategy was either
incomplete (hardcoded provider name list rots)
or intrusive (process inspection, filesystem xattr probing).

You are responsible for keeping the **per-machine state directory**
and your **dotfiles repository** off the following kinds of mounts:

- iCloud Drive (`~/Library/Mobile Documents/`)
- OneDrive (`~/OneDrive`, `~/OneDrive - <org>`)
- Dropbox (`~/Dropbox`)
- Box / Box Sync (`~/Box`, `~/Box Sync`)
- Google Drive (Drive File Stream, Drive for Desktop)
- Syncthing-managed directories
- Any FUSE-backed cloud mount with deferred uploads

### Why this matters

Patina's crash-safety guarantee depends on the journal being written
atomically and surviving a kill-9. Cloud-sync
providers intermediate file writes through their own queueing
layer — your local `fsync` returns before the provider has uploaded,
and the provider may rename, version, or delay files for reasons
Patina cannot observe. Specifically:

- **Backups can be silently versioned**, which makes "restore the
  last-applied bytes" non-deterministic.
- **Journal files can appear out of order** during recovery if the
  provider reorders uploads, breaking the per-operation cursor.
- **The advisory file lock** (`fs2` over `flock(2)` /
  `LockFileEx`) is not well-defined on cloud-mounted filesystems;
  two `patina apply` invocations could interleave.

For the repository itself, the failure mode is subtler: a long-running
upload holds the source file open with exclusive sharing semantics
on Windows, racing with `patina apply`'s reads.

### What to do instead

Pick a local-disk directory for both:

```sh
# Linux/macOS
mkdir -p ~/dotfiles
git clone <your repo> ~/dotfiles

# Windows (Powershell)
New-Item -ItemType Directory -Path C:\Users\<you>\dotfiles -Force
git clone <your repo> C:\Users\<you>\dotfiles
```

The state directory is already on local disk by default per the
table above; you'd have to actively override `XDG_STATE_HOME` to
move it onto a cloud-sync mount.

---

## Linux: surviving logout with `loginctl enable-linger`

By default, `systemd --user` services (including the `patina watch`
service installed by `patina watch install`) stop when your last
login session ends. If you SSH into a server, run
`patina watch install`, then SSH out, the watcher dies with your
session.

**Patina does not invoke `loginctl enable-linger` for you in v1.0.**
The reason: every other Patina command runs as the unprivileged user;
the main `patina` process never prompts for sudo. We do not want to
break that invariant for the minority of users who actually need
survive-logout behavior. A `--linger` flag is a v1.1 candidate.

### When you need lingering

You probably want lingering if:

- You run Patina on a server you SSH into intermittently and want
  drift detection / re-applies to continue between sessions.
- You want the watcher to keep running across reboots without
  needing to log in via the console.

You probably do **not** need lingering if:

- You're on a desktop / laptop where you stay logged in.
- You only use Patina for deterministic apply runs and don't care
  about the watcher between sessions.

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

`patina watch uninstall` does **not** call `disable-linger` for the
same reason it does not call `enable-linger`.

### Without systemd

If you're on a non-systemd Linux (Void, Devuan with sysvinit-style,
Alpine without OpenRC-systemd parity), Patina has no preinstalled
service template for your init system. The supported path is to run
the watcher inline with `patina watch --foreground` inside your own
supervisor (runit, s6, OpenRC); templates for other init systems remain
a v1.1 candidate.
