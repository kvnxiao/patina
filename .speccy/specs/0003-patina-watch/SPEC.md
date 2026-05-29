---
id: SPEC-0003
slug: patina-watch
title: Patina watch — filesystem event loop, per-OS service install, drift detection
status: in-progress
created: 2026-05-25
supersedes: []
---

# SPEC-0003: Patina watch — filesystem event loop, per-OS service install, drift detection

## Summary

SPEC-0001 ships the engine; SPEC-0002 ships the complete CLI plus
Windows symbolic link elevation. SPEC-0003 ships the **watch
subsystem** — an optional per-user background process that monitors
the dotfiles repository for source changes and triggers re-applies
automatically, and monitors copy-mode / template-rendered targets
for external edits (drift) and notifies the user when they happen.

The watcher subscribes only to paths that the most recent apply
journal recorded; new repository files require an explicit
`patina apply` to register subscriptions. This matches the explicit
mental model that tools like `stow` and `dotter` provide and avoids
the surprise of "I added a file to the repo and Patina deployed it
without me asking". A 500ms hardcoded debounce coalesces editor save
bursts (write-temp + rename + metadata update sequences) into single
re-apply events. The watcher uses the `notify` crate wrapped in
`notify-debouncer-full` for cross-platform native filesystem events
(inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on
Windows).

Drift detection runs only against non-symbolic-link targets (symbolic
links cannot drift because they resolve to the source). When a
deployed copy-mode or template-rendered file's `blake3` hash diverges
from the journal-recorded expected hash, the watcher emits an
OS-native desktop notification via `notify-rust` so the user learns of
the edit proactively. The DRIFTED *classification* itself is not the
watcher's to make: `patina status` already reports any content target
whose live bytes differ from the recorded hash (SPEC-0001 REQ-018),
watcher or no watcher. The watcher's drift cache is its notification
ledger, not a status input. There is no `on_drift` hook in v1.0; the
user resolves drift by re-running `patina apply` (to revert to source)
or by running `patina promote` (defined in SPEC-0002, to update the
source from the target).

The watcher coordinates with the CLI through the advisory file lock
defined in SPEC-0001 REQ-023. The CLI has priority: when a manual
`patina apply` is running, the watcher's pending re-apply cycle
skips and the next filesystem event re-arms it.

Per-OS service install uses each platform's standard user-scope
mechanism: macOS LaunchAgent in `~/Library/LaunchAgents/`, Linux
`systemd --user` unit in `~/.config/systemd/user/`, Windows per-user
Scheduled Task with a logon trigger. None of these require admin or
sudo at install time. Linux `systemd --user` services do not survive
logout unless lingering is enabled — and lingering is **explicitly
out of scope for v1.0**: Patina does not invoke `loginctl
enable-linger` itself and does not ship a `--linger` flag. Users
who want their watcher to survive logout run
`sudo loginctl enable-linger $USER` manually; the project docs
(`docs/USER_GUIDE.md`) include the snippet. A
`--linger` flag is a v1.1 candidate if real users surface the
need.

On Windows, file writes during apply can hit transient
`ERROR_SHARING_VIOLATION` errors from antivirus scans, cloud-sync
processes, or editors holding briefly-exclusive locks. SPEC-0003
adds a retry-with-backoff policy (50ms → 100 → 200 → 400 → 800 → 1600
ms, total ~3.15s budget) before the engine surfaces the violation as
an apply error. The retry policy lives in `patina-core`'s Windows
code path; this SPEC is where the policy is specified, not where it
is first useful — manual `patina apply` invocations benefit from the
retry too, but the brainstorm grouped it with the watcher because the
watcher's frequent re-applies amplify the issue's impact.

## Goals

<goals>
- A user runs `patina watch install` on macOS, Linux, or Windows
  without admin or sudo and a per-user background service is
  registered that starts at login.
- The installed watcher detects a change to a managed source file
  in the repository within ~500ms and triggers a re-apply that
  serializes cleanly with any concurrent manual `patina apply`.
- The installed watcher detects an external edit to a copy-mode or
  template-rendered target on disk, emits an OS-native desktop
  notification within ~500ms, and the file is marked DRIFTED on
  the next `patina status` invocation.
- A user runs `patina watch uninstall` and the per-user service is
  removed; no residual files remain in
  `~/Library/LaunchAgents/`, `~/.config/systemd/user/`, or the
  Windows Task Scheduler under HKCU scope.
- A user on a non-systemd Linux distribution runs
  `patina watch --foreground` and the watcher runs inline in the
  terminal, suitable for the user to wrap in their own supervisor
  (runit, s6, OpenRC).
- A `patina apply` on Windows that races against an antivirus scan
  briefly holding a target file recovers transparently via the
  retry-with-backoff policy and completes without surfacing an
  error to the user.
</goals>

## Non-goals

<non-goals>
- No `on_change` or `on_drift` user-defined hook events. The watch
  loop fires `pre_apply` and `post_apply` hooks on every
  watcher-triggered re-apply, identically to manual applies; no
  paths-globbed event hook is introduced. Deferred past v1.0.
- No auto-promotion of drifted targets back to source. The user
  must explicitly run `patina promote` (defined in SPEC-0002).
  Auto-promotion would violate the safety prior that "Patina never
  silently sync state".
- No recursive watch on the entire repository directory tree. The
  watcher subscribes only to paths that the journal records.
  New repository files require manual `patina apply` to register.
- No watcher daemon binary separate from `patina.exe` / `patina`.
  The watcher runs as the same binary invoked with the
  `watch --foreground` or `watch start` subcommand.
- No watcher Inter-Process Communication (IPC) protocol. CLI / watcher
  coordination is via the SPEC-0001 advisory file lock, not via
  sockets or named pipes.
- No OpenRC, runit, s6, or SysV-init service templates shipped in
  v1.0. Non-systemd Linux is served by `patina watch --foreground`
  inside the user's own supervisor.
- No Windows Service (system-scope) install. Patina is a per-user
  tool; system-scope services require admin at install time and
  are out of scope.
- No drift-detection polling fallback when native FS events are
  unavailable (e.g., on NFS, SMB, or other network filesystems
  where inotify/FSEvents/RDCW have known limitations).
  `patina doctor` (SPEC-0002 REQ-005) warns when the repository is
  on a UNC path; SPEC-0003 does not engineer a workaround.
- No watcher metrics surfaced via a CLI subcommand, in-memory ring
  buffer, or HTTP endpoint. Metrics go to the structured `tracing`
  log only.
- No `patina watch reload` to re-read configuration without
  restarting the service. Configuration changes (e.g., new
  modules, edited `patina.toml`) take effect on the next manual
  `patina apply`, which the watcher then re-subscribes around.
- No `--linger` flag on `patina watch install`. Linux's
  `systemd --user` services stop at logout by default; users who
  want survive-logout behavior run
  `sudo loginctl enable-linger $USER` manually per the docs
  (`docs/USER_GUIDE.md`). Patina does not invoke
  `loginctl enable-linger` itself. A `--linger` flag is a v1.1
  candidate; see DEC-005 for rationale.
</non-goals>

## User stories

<user-stories>
- As a user actively editing dotfiles during a development session,
  I want a background process that re-applies my changes as I save,
  so I do not have to remember to run `patina apply` after every
  edit.
- As a user who occasionally edits a deployed file in place (during
  debugging or quick experimentation), I want a desktop
  notification when I do so, so I do not later wonder why my next
  `patina apply` reverted my work.
- As a Linux user, I want my watcher to start automatically when I
  log in and stop when I log out, so I do not have a runaway
  background process I forgot about — and if I want it to survive
  logout (e.g., on a server I SSH into and out of), I want the docs
  to point me at the one-shot `sudo loginctl enable-linger $USER`
  command so I can opt into it manually.
- As a Windows user, I want my `patina apply` invocations to be
  resilient to transient antivirus scan locks so a quick apply does
  not fail because Windows Defender held my `.zshrc` open for 80ms
  during a routine scan.
- As a user on a non-systemd Linux distribution (Void, Devuan,
  Alpine without OpenRC patches), I want a documented way to run
  the watcher under my own supervisor.
</user-stories>

## Assumptions

<assumptions>
- The `notify` crate's `notify-debouncer-full` wrapper is the
  best-maintained cross-platform debouncing layer in early-2026
  Rust. Should it become unmaintained, the documented fallback is
  to use `notify` directly with hand-rolled debouncing inside
  Patina (a localized change in the watcher's event loop).
- Native FS events are reliable on local filesystems on all three
  supported OSes: inotify (Linux ext4/btrfs/xfs/zfs),
  FSEvents (macOS APFS), ReadDirectoryChangesW (Windows NTFS,
  ReFS). Network filesystems (NFS, SMB, CIFS) and FUSE mounts
  have known limitations; `patina doctor` warns when the repo is
  on a UNC path on Windows (SPEC-0002 REQ-005) and the user is
  responsible for choosing local-FS repository placement.
- `notify-rust`'s desktop notification API covers all three target
  OSes (DBus org.freedesktop.Notifications on Linux,
  NSUserNotification on macOS, ToastNotificationManager on
  Windows). If a user's macOS has Notification Center disabled,
  the notification is silently dropped — Patina does not work
  around this; the journal still records the drift.
- Windows per-user Scheduled Tasks (HKCU scope, logon trigger) do
  not require admin to create, update, or delete. This is true on
  Windows 10 1607+ and all Windows 11 releases.
- The 500ms debounce window is tuned empirically based on common
  editor save sequences (nvim: write-temp + rename;
  VS Code: write + chmod + stat; intellij: similar). A future
  amendment may make this configurable; v1.0 does not.
- The Windows `ERROR_SHARING_VIOLATION` retry budget (~3.15s
  cumulative) is enough to outlast typical antivirus scans
  (≤500ms) and cloud-sync uploads (≤2s) on a modern machine. Users
  on very slow or heavily contended systems may see occasional
  apply failures; the journal-and-retry pattern recovers cleanly
  on next invocation.
- The advisory lock from SPEC-0001 REQ-023 handles the
  watcher-vs-CLI race correctly. If `fs2`'s cross-platform
  guarantees prove insufficient, the fallback (per SPEC-0001
  Decisions) is to swap in `fd-lock` — a localized change.
</assumptions>

## Requirements

<requirement id="REQ-001">
### REQ-001: `patina watch install` registers a per-user background service without admin

The `patina watch install` subcommand creates the platform-specific
service registration that launches the watcher at user login. The
subcommand does NOT require admin or sudo on its default path. The
service file or task descriptor is written to a per-user location
and the supervisor is invoked to load the unit.

<done-when>
- On macOS, the command writes
  `~/Library/LaunchAgents/com.patina.watcher.plist` containing a
  valid `launchd` plist with `RunAtLoad = true`, `KeepAlive` for
  on-failure restart, and `ProgramArguments` pointing at the
  current Patina binary's canonical absolute path.
- On macOS, the command invokes `launchctl bootstrap
  gui/$(id -u) ~/Library/LaunchAgents/com.patina.watcher.plist`
  to load the service.
- On Linux, the command writes
  `~/.config/systemd/user/patina-watcher.service` containing a
  valid systemd unit with `Restart=on-failure`,
  `WantedBy=default.target`, and `ExecStart` pointing at the
  current Patina binary's canonical absolute path with the
  `watch --foreground` subcommand.
- On Linux, the command invokes `systemctl --user enable
  --now patina-watcher.service` to enable and start it.
- On Windows, the command creates a per-user Scheduled Task named
  `Patina Watcher` with a logon trigger, `RunLevel = Limited`
  (non-elevated), and an action pointing at the current
  Patina binary with `watch --foreground`.
- On Windows, the task is registered via `winsafe`'s `taskschd`
  feature using the same HKCU-scoped APIs that
  `schtasks /create /sc onlogon` would use.
- None of the install paths require admin or sudo. Linux users
  who want the watcher to survive logout run
  `sudo loginctl enable-linger $USER` manually; Patina does not
  invoke this command and ships no `--linger` flag in v1.0 (see
  `docs/USER_GUIDE.md` for the snippet).
- The command exits 1 with a typed error if the service is
  already installed (the user must `patina watch uninstall` first
  before re-installing).
</done-when>

<behavior>
- Given a macOS host with no prior Patina installation, when
  `patina watch install` runs as a normal user, then
  `~/Library/LaunchAgents/com.patina.watcher.plist` exists,
  `launchctl list com.patina.watcher` reports the service is
  loaded, and the command exits 0.
- Given a Linux host with `systemd --user` available, when
  `patina watch install` runs as a normal user, then
  `~/.config/systemd/user/patina-watcher.service` exists,
  `systemctl --user is-enabled patina-watcher.service` returns
  `enabled`, and the command exits 0.
- Given a Windows host running as a non-administrator account,
  when `patina watch install` runs, then a Scheduled Task named
  `Patina Watcher` exists in the HKCU scope with a logon
  trigger, and the command exits 0.
</behavior>

<scenario id="CHK-001">
Given a macOS test host with no prior installation,
when `patina watch install --json` runs as a normal user,
then `~/Library/LaunchAgents/com.patina.watcher.plist` exists with
mode `0644`, the JSON output's `result` field is `installed`, and
`launchctl print gui/$(id -u)/com.patina.watcher` reports the
service.
</scenario>

<scenario id="CHK-002">
Given a Linux test host with systemd,
when `patina watch install --json` runs as a normal user,
then `~/.config/systemd/user/patina-watcher.service` exists, the
JSON output's `result` field is `installed`, and `systemctl --user
is-active patina-watcher.service` returns `active`.
</scenario>

<scenario id="CHK-003">
Given a Windows test host running as a standard user,
when `patina watch install --json` runs,
then a HKCU-scoped Scheduled Task named `Patina Watcher` exists,
its trigger is `OnLogon`, and the JSON output's `result` field is
`installed`.
</scenario>
</requirement>

<requirement id="REQ-003">
### REQ-003: `patina watch uninstall`, `start`, `stop`, `restart`, `status` manage the registered service

The watch command suite includes lifecycle subcommands operating
on the installed service. Each is a thin wrapper over the
platform's native service management primitives; none require
admin or sudo on the default path. `status` is the only read-only
subcommand and acquires the shared advisory lock per SPEC-0001
REQ-023.

<done-when>
- `patina watch uninstall` removes the service file or task and
  stops the running watcher first. On Linux, uninstall does not
  invoke `loginctl disable-linger`; users who manually enabled
  lingering must manually disable it
  (`sudo loginctl disable-linger $USER`).
- `patina watch start` invokes the platform supervisor to start
  the service: `launchctl start com.patina.watcher` on macOS,
  `systemctl --user start patina-watcher` on Linux, the
  Scheduled Task `Run` action on Windows.
- `patina watch stop` invokes the supervisor to stop without
  uninstalling: `launchctl stop`, `systemctl --user stop`, task
  `End` action.
- `patina watch restart` is equivalent to `stop` followed by
  `start`.
- `patina watch status` queries the supervisor for liveness
  state, last-fired time, and last-exit code, and prints a
  human-readable summary; with `--json` it emits a structured
  object containing `installed`, `running`, `last_fired_at`,
  `last_exit_code`, `subscriptions_count`,
  `re_applies_since_start`.
- All subcommands except `status` and `--foreground` acquire the
  exclusive advisory lock per SPEC-0001 REQ-023.
- The lifecycle subcommands are no-ops with a clear stderr
  message when the service is not installed (rather than
  spurious errors from the supervisor).
</done-when>

<behavior>
- Given a previously installed Patina service, when
  `patina watch uninstall --yes` runs, then the service file or
  task is removed, the running watcher (if any) is stopped, and
  exit code is 0.
- Given a stopped Patina service, when `patina watch start`
  runs, then the supervisor starts the service and a subsequent
  `patina watch status` reports `running = true`.
- Given no installed service, when `patina watch start` runs,
  then stderr names "service not installed; run `patina watch
  install` first" and exit code is 1.
</behavior>

<scenario id="CHK-005">
Given a Linux test host with the Patina service installed and
running,
when `patina watch uninstall --yes --json` runs,
then `~/.config/systemd/user/patina-watcher.service` does not
exist, `systemctl --user list-unit-files patina-watcher.service`
reports nothing, and the JSON output's `result` field is
`uninstalled`.
</scenario>

<scenario id="CHK-006">
Given a macOS test host with the service installed but stopped,
when `patina watch status --json` runs,
then the JSON output contains `installed = true`,
`running = false`, and `last_exit_code` is the supervisor's most
recent recorded value (or `null` if never run).
</scenario>
</requirement>

<requirement id="REQ-004">
### REQ-004: `patina watch --foreground` runs the watcher inline for debugging and non-systemd Linux

The `patina watch --foreground` subcommand runs the watcher loop
in the current terminal, attached to the invoking shell. It is
the supported escape hatch for non-systemd Linux distributions
(users wrap it in their own supervisor) and the documented path
for debugging the watcher's behavior interactively. Ctrl-C
(SIGINT on POSIX, equivalent on Windows) cleanly shuts down the
watcher; the watcher releases its subscriptions and exits 0.

<done-when>
- `patina watch --foreground` does NOT acquire the exclusive
  advisory lock (the watcher acquires per-re-apply locks
  internally; the foreground process itself is not a long-held
  lock holder).
- The foreground watcher logs its subscriptions and ongoing
  events to stderr via `tracing` at info level.
- The foreground watcher exits 0 on SIGINT after releasing all
  FS subscriptions cleanly.
- A SIGTERM produces the same clean exit path as SIGINT.
- On Windows, `Ctrl-C` (or `Ctrl-Break`) triggers the same
  cleanup; no Windows-specific signal handling is exposed beyond
  what `tokio::signal::windows::ctrl_c` provides.
- The foreground watcher acquires no per-user-service registration
  state; it is purely an in-process lifecycle.
</done-when>

<behavior>
- Given a Patina repository with one managed file, when
  `patina watch --foreground` is launched and the source file
  is touched, then the watcher logs a re-apply event to stderr
  within 500ms (plus debounce) and the target file's hash matches
  the new source.
- Given a running foreground watcher, when SIGINT is sent, then
  the process logs a shutdown event, releases subscriptions, and
  exits 0 within 1 second.
</behavior>

<scenario id="CHK-007">
Given a test harness that spawns `patina watch --foreground` as
a subprocess with `RUST_LOG=patina_core=info`, waits 500ms,
modifies a source file in the test repository, and waits another
1500ms,
when the harness checks the subprocess's stderr,
then stderr contains the substring `re-apply` (or equivalent log
event marker) and the test repository's target file's hash equals
the new source's hash.
</scenario>

<scenario id="CHK-008">
Given a running foreground watcher,
when the test harness sends SIGINT (or `Ctrl-C` on Windows) to
the process,
then the process exits 0 within 1 second and stderr contains the
substring `shutdown`.
</scenario>
</requirement>

<requirement id="REQ-005">
### REQ-005: Watcher subscribes only to journal-recorded paths; new repo files require manual `patina apply` to register

The watcher reads the most recent committed journal — the
`<state>/patina/journal/<ts>.COMMIT` record (SPEC-0001 REQ-029); the
`<ts>.plan` is deleted at commit, so the COMMIT record is the source
of truth — and subscribes via `notify` only to the per-target source
paths and non-symlink target paths it records. Each recorded target
carries its canonical source (REQ-029), so the watcher recovers the
source→target mapping for symlink, copy, and template targets without
re-reading the repository. The watcher does not recursively watch the
repository directory tree. Files added to
the repository without a subsequent `patina apply` are not
watched and do not trigger re-applies.

<done-when>
- After `patina apply --yes` completes, the watcher (if running)
  has FS subscriptions exactly equal to the union of source paths
  and non-symlink target paths listed in the journal.
- A new file added to the repository directory but not declared
  in any `[[file]]` entry does not appear in the watcher's
  subscription list.
- A new `[[file]]` entry added to a module's `patina.toml` does
  not affect the watcher until the user runs `patina apply`
  (which writes a new journal and triggers the watcher to
  re-read).
- The watcher detects journal updates by subscribing via `notify`
  to the per-machine state directory's `journal/` subdirectory
  (path defined in SPEC-0001 REQ-016). When a new `.plan` or
  `.COMMIT` file appears, the watcher acquires the shared advisory
  lock, re-reads the most recent committed journal, and
  re-computes its FS subscriptions. This is the sole mechanism;
  there is no polling and no CLI-to-watcher IPC.
- Symbolic-link-mode targets are not separately subscribed; only
  their source paths in the repository are. (Modifying the source
  is equivalent to modifying the symlinked target.)
</done-when>

<behavior>
- Given a Patina repository with three managed files (two
  symlinks, one copy-mode), when `patina apply --yes` completes
  followed by `patina watch --foreground`, then the watcher's
  logged subscription list contains 4 paths: the three source
  paths plus the one copy-mode target.
- Given a running watcher and a fresh file added to the repository
  not declared in any module, when the new file is modified, then
  no re-apply event fires.
- Given a running watcher and a new `[[file]]` entry added to a
  module's `patina.toml`, when the source file is modified
  (without re-running `patina apply`), then no re-apply event
  fires.
- Given the same state followed by `patina apply --yes` (which
  writes a new journal), when the source file is modified, then
  a re-apply event fires within the debounce window.
</behavior>

<scenario id="CHK-009">
Given a Patina repository with two `[[file]]` symlink entries and
one `[[file]]` copy-mode entry, a fresh apply, and a running
foreground watcher,
when the watcher's internal subscription list is inspected via the
`tracing` log,
then the log contains exactly four watched paths (three source
paths in the repository plus one target path on the system) plus
the per-machine state directory's `journal/` path (the
journal-rescan subscription).
</scenario>

<scenario id="CHK-017">
Given a running foreground watcher with an initial subscription
set and a parallel test process that runs
`patina apply --yes` (creating a new `.plan` and `.COMMIT` in
`<state>/patina/journal/`),
when 1000ms passes after the CLI exits,
then the watcher's `tracing` log records a journal-rescan event
naming the new `.COMMIT` filename, and the watcher's updated
subscription list reflects the new journal's paths.
</scenario>
</requirement>

<requirement id="REQ-006">
### REQ-006: Watcher coalesces burst FS events with a hardcoded 500ms debounce, then re-applies

The watcher uses `notify-debouncer-full` with a 500ms debounce
window. Burst FS events (a typical editor save produces 3-5
events: write to tempfile, rename, modify metadata, stat) within
the window are coalesced into a single re-apply trigger. The
debounce duration is hardcoded in v1.0; no configuration knob
exposes it.

<done-when>
- A series of FS events arriving within a 500ms window produces
  exactly one re-apply event.
- A FS event arriving 501ms after the previous one produces a
  separate re-apply event.
- The debounce duration is hardcoded as a constant in the
  watcher code; root `patina.toml` does not accept a
  `[watcher] debounce_ms` key, and parsing such a key produces
  a typed warning (forward-compatible: future versions may add
  the knob without breaking older repositories).
- The re-apply trigger acquires the exclusive advisory lock; if
  the CLI holds the lock, the watcher's re-apply skips this
  cycle and the next FS event re-arms the debounce.
</done-when>

<behavior>
- Given a running foreground watcher and a test that touches a
  watched source file five times within 100ms, when the touches
  complete and 500ms passes, then exactly one re-apply event
  fires.
- Given the same harness with touches 600ms apart, when the
  touches complete, then two re-apply events fire (one per
  touch).
- Given a running foreground watcher and a parallel manual
  `patina apply --yes` holding the exclusive lock, when a source
  file is modified, then the watcher's re-apply attempts the
  lock, observes it held, and skips this cycle; a subsequent FS
  event re-arms the debounce.
</behavior>

<scenario id="CHK-010">
Given a test harness that touches a watched source file five
times within 100ms and then waits 1000ms,
when the watcher's `tracing` log is inspected,
then exactly one re-apply log event is present.
</scenario>
</requirement>

<requirement id="REQ-007">
### REQ-007: Drift detection hashes non-symlink targets on change and notifies on divergence

For every non-symlink target in the journal (copy mode,
copy-tree files, template-rendered output), the watcher
subscribes to the target path. When the target's FS event fires,
the watcher computes the file's `blake3` hash and compares to the
journal-recorded expected hash — a 32-byte `blake3` digest per
SPEC-0001 REQ-029, so the freshly computed and recorded hashes use
the same algorithm and are directly comparable. If they differ, the
watcher emits an OS desktop notification via `notify-rust`, writes a
DRIFTED event to the structured log, and records the divergence in a
small drift cache at `<state>/patina/drift.cache`.

The drift cache is the watcher's *notification ledger* — it backs the
per-target rate limit (DEC-004), the `patina debug drift-cache`
decode surface, and the watcher's own metrics. It is **not** consulted
by `patina status`. `patina status` classifies a content target as
DRIFTED by SPEC-0001 REQ-018's own live comparison — freshly hashing
the target and comparing to the recorded `blake3` (REQ-029) — so a
drifted file is reported whether or not the watcher is running, and a
file edited and then reverted to its recorded bytes reports CLEAN (the
live hash matches) even though the watcher logged the intervening
edit. Routing the DRIFTED verdict through a stale cache entry instead
would contradict that, so the cache deliberately does not feed status.

<done-when>
- A FS event on a watched non-symlink target triggers a hash
  computation against the target's current bytes.
- If the computed hash differs from the journal-recorded hash,
  `notify-rust` emits a desktop notification with title
  "Patina: drift detected" and body naming the target path.
- The drift cache file is updated atomically (write to a tempfile
  and rename) so concurrent `patina status` reads never observe
  a half-written cache.
- The drift cache survives watcher restarts; the next
  `patina apply` clears it when its journal becomes the new
  truth.
- The drift cache is **not** read by `patina status`. `patina status`
  derives DRIFTED solely from SPEC-0001 REQ-018's live re-hash, so a
  content target reverted to its recorded bytes after an edit reports
  CLEAN even while the cache still holds the intervening edit event.
- A FS event on a symlink target is NOT processed by drift
  detection (symlinks cannot drift because they resolve to the
  source).
- The drift cache uses a per-machine, per-machine-state-dir
  location; no drift data crosses machines.
- Drift notification is rate-limited per target: at most one
  notification per target per 60-second window, regardless of
  how many FS events fire.
- The drift cache file is `postcard`-encoded with a `u16` major
  version envelope at offset 0, mirroring the journal version
  envelope (SPEC-0001 REQ-011). A future format change bumps the
  major; older binaries refuse to decode and emit a typed error
  naming both versions. The schema records, per entry: the
  target path (`Utf8PathBuf`), `expected_hash` (32-byte
  `blake3`), `actual_hash` (32 bytes), and an internal
  `detected_at_unix` integer (non-user-facing). The file's
  top-level record carries the version envelope, the journal
  timestamp this cache is against, and the entries array.
- A `patina debug drift-cache <path>` subcommand decodes the
  cache to human-readable form, parallel to SPEC-0001 REQ-020's
  `patina debug journal <path>`. It prints the version envelope,
  the journal timestamp the cache is bound to, and one block per
  entry naming the target path, expected and actual hashes, and
  the human-rendered detection time. An invalid path produces a
  typed error and exit 1; a cache from a newer Patina (version
  envelope major exceeds the running binary's) is refused with a
  typed error naming both versions.
</done-when>

<behavior>
- Given a managed copy-mode target at `~/.gitconfig`, when the
  user edits the file externally (e.g., with `echo "new" >>
  ~/.gitconfig`), then within ~500ms the watcher emits a
  desktop notification, the drift cache contains an entry for
  `~/.gitconfig`, and `patina status` reports DRIFTED for that
  path.
- Given the same scenario where the user touches the file twice
  within 60 seconds, when both touches complete, then at most
  one notification is emitted (the second touch is rate-limited).
- Given a symbolic link target whose source file is modified,
  when the FS event fires on the target path, then no drift
  detection runs (the watcher knows the target is a symlink and
  the change is propagated via the source path).
</behavior>

<scenario id="CHK-011">
Given a Patina apply that materialized `~/.gitconfig` as a copy
of `<repo>/git/gitconfig` (hash H1), a running foreground
watcher, and a test that overwrites `~/.gitconfig` with content
hashing to H2 ≠ H1,
when 1000ms passes,
then `<state>/patina/drift.cache` contains an entry naming
`~/.gitconfig` with `expected_hash = H1, actual_hash = H2`, and
`notify-rust`'s test-mode capture records exactly one
notification.
</scenario>

<scenario id="CHK-012">
Given the post-state of CHK-011 (`~/.gitconfig` now holds content
hashing to H2 ≠ the recorded H1),
when `patina status --json` runs,
then the JSON output's `files` array contains an entry with
`path` containing `.gitconfig` and `state = "drifted"` — derived
from SPEC-0001 REQ-018's live re-hash, not from the drift cache —
and the `drifted` aggregate counter is at least 1.
</scenario>

<scenario id="CHK-018">
Given the post-state of CHK-011 (a populated
`<state>/patina/drift.cache` file),
when `patina debug drift-cache <state>/patina/drift.cache` runs,
then stdout contains the substrings `version:`, the journal
timestamp, the target path (`.gitconfig`), and both hash values
(expected and actual); the process exits 0.
</scenario>
</requirement>

<requirement id="REQ-008">
### REQ-008: Watcher and CLI coordinate via the SPEC-0001 advisory file lock; CLI has priority

The watcher acquires the exclusive advisory file lock at
`<state>/patina/lock` (defined in SPEC-0001 REQ-023) before each
re-apply cycle. If the lock is held (by a manual `patina apply`,
`patina rollback`, `patina promote`, `patina add`, or
`patina remove`), the watcher skips this cycle and the next FS
event re-arms the debounce. The CLI does NOT yield to the
watcher; if the CLI invocation arrives while the watcher holds
the lock, the CLI blocks until the watcher's current re-apply
finishes (typically <1s for small repositories). For read-only
CLI commands (`status`, `doctor` without `--fix`), the shared-lock
semantics from SPEC-0001 REQ-023 apply.

<done-when>
- The watcher's re-apply path acquires the exclusive lock with a
  non-blocking attempt; on contention, the cycle skips.
- The watcher logs a `lock_contention_skip` event via `tracing`
  on each skip so the structured-log metrics (REQ-009) can count
  occurrences.
- A manual `patina apply` that arrives while the watcher holds
  the lock blocks until the watcher releases; the CLI does not
  exit early on contention (no timeout-4 exit unless the wait
  exceeds the SPEC-0001 60s budget).
- The watcher releases the lock immediately after the re-apply
  completes; the lock is NOT held across multiple debounce
  cycles.
- Stale-lock detection: if the lock file's holder PID is dead
  (no process by that PID exists), the watcher logs a warning
  but does not forcibly clear the lock (the OS releases it when
  the holder process dies; if the file system retains a stale
  fd ownership, doctor's recommended remediation is to delete
  the lock file manually after confirming no patina process is
  running).
</done-when>

<behavior>
- Given a running watcher and a concurrent manual `patina apply
  --yes`, when both attempt the lock, then the watcher's
  contended cycle skips, the CLI completes normally, and the
  watcher's next FS event triggers a fresh re-apply.
- Given a running watcher that has held the lock for 100ms
  during a re-apply, when a concurrent `patina apply --yes`
  arrives, then the CLI blocks for ~the remainder of the
  watcher's apply (a few hundred ms), then proceeds and
  completes; the watcher's own subsequent re-apply skips
  because the CLI's apply changed the journal.
</behavior>

<scenario id="CHK-013">
Given a foreground watcher and a test harness that synchronously
launches a `patina apply --yes` while the watcher is mid-re-apply,
when both complete,
then the watcher's `tracing` log contains zero
`lock_contention_skip` events for the CLI's apply (the watcher
already held the lock; the CLI blocked), and the journal contains
exactly two committed plans (the watcher's plus the CLI's).
</scenario>
</requirement>

<requirement id="REQ-009">
### REQ-009: Watcher emits metrics via structured `tracing` logs only

The watcher records operational metrics (re-applies completed,
re-applies skipped due to lock contention, drift events
detected, drift notifications emitted, debounce intervals
observed, FS subscription counts) as structured `tracing`
events. There is no in-memory ring buffer, no CLI subcommand to
query metrics, and no HTTP endpoint. The watcher's metrics are
extracted via the structured log file written by `tracing-appender`
under `<state>/patina/logs/` — a directory and rotating-log stack
this SPEC introduces. SPEC-0001 REQ-016 defines the state-directory
root and its `journal/` and `backups/` subdirectories but neither a
`logs/` directory nor any logging configuration; SPEC-0003 owns both.

<done-when>
- Each re-apply event produces a `tracing` event at info level
  with fields including `re_apply.id`, `re_apply.duration_ms`,
  `re_apply.files_changed`.
- Each drift detection produces a `tracing` event at warn level
  with fields `drift.path`, `drift.expected_hash`,
  `drift.actual_hash`.
- Each lock contention skip produces a debug-level event with
  field `skip.reason = "lock_held"`.
- The watcher does NOT expose a `patina watch metrics` subcommand
  or any other in-process metrics surface.
- The watcher creates `<state>/patina/logs/` lazily on first start
  (SPEC-0001's `state_dir` resolution does not create it) and writes
  its log there via `tracing-appender` with daily rotation, keeping
  the 7 most recent files.
</done-when>

<behavior>
- Given a running watcher and three watched source files each
  modified once within 5 seconds, when the watcher's log file
  is inspected after 30 seconds, then the log contains three
  info-level re-apply events with distinct `re_apply.id`
  fields.
- Given a watcher and a concurrent CLI apply causing a lock
  skip, when the log is inspected, then exactly one debug-level
  event with `skip.reason = "lock_held"` is present.
</behavior>

<scenario id="CHK-014">
Given a foreground watcher with `RUST_LOG=patina_core=info`
and a test that modifies three watched source files in
succession (>500ms apart),
when the test inspects the watcher's stderr,
then exactly three info-level events with the message substring
`re_apply` are present, each with distinct `re_apply.id`
fields.
</scenario>
</requirement>

<requirement id="REQ-010">
### REQ-010: Windows file writes retry on `ERROR_SHARING_VIOLATION` with exponential backoff

The Windows-only code path in `patina-core`'s apply pipeline
retries file write operations that fail with
`ERROR_SHARING_VIOLATION` (Win32 error code 32) using exponential
backoff: 50ms, 100ms, 200ms, 400ms, 800ms, 1600ms (six retries,
total ~3.15 seconds of wait). After the sixth retry, the engine
surfaces the violation as a typed error and the apply fails or
rolls back per the normal apply pipeline (REQ-013 in SPEC-0001).
The retry policy applies to all file writes — symlink creation,
byte copies, template-rendered output writes — regardless of
whether the apply was triggered by the CLI or the watcher.

<done-when>
- A file write that fails with `ERROR_SHARING_VIOLATION` retries
  after a 50ms delay; if the second attempt also fails, retries
  after 100ms; and so on through 200, 400, 800, 1600 ms.
- On retry success, no error is surfaced to the user; a debug-
  level `tracing` event records the number of retries used.
- On exhaustion of the six retries, a typed error is returned to
  the apply pipeline; the engine handles it per the normal
  failure pathway (rollback or abort).
- The retry policy applies only on Windows. On macOS and Linux,
  no retry wrapping is performed; FS write failures surface
  immediately as normal errors.
- The retry behavior is observable in the `tracing` log when
  enabled at debug level: each retry emits a `fs_write_retry`
  event with fields `attempt`, `delay_ms`, `error`.
</done-when>

<behavior>
- Given a Windows test host with a test harness that holds
  `~/.zshrc` open with `FILE_SHARE_NONE` for 250ms during an
  apply attempt, when `patina apply --yes` runs and writes
  `~/.zshrc`, then the write succeeds after approximately 5
  retries (50+100+200+400+800 ≈ 1550ms cumulative; actual count
  depends on timing), the apply completes successfully, and no
  error is surfaced.
- Given a Windows test host where a process holds `~/.zshrc`
  open with `FILE_SHARE_NONE` for 10 seconds, when
  `patina apply --yes` runs, then after ~3.15s of retries the
  engine surfaces `ERROR_SHARING_VIOLATION`, the apply
  rolls back or aborts per the apply pipeline, and exit code
  matches the normal failure path.
- Given a macOS or Linux test host with the same scenario, when
  `patina apply --yes` runs, then the first write attempt
  fails or succeeds based on the OS's normal semantics; no
  retry wrapping occurs.
</behavior>

<scenario id="CHK-015">
Given a Windows test host and a harness that opens `~/.zshrc`
with `FILE_SHARE_NONE` for 250ms during an apply,
when `patina apply --yes` runs (with
`RUST_LOG=patina_core=debug`),
then the apply completes with exit code 0, stdout reports the
target as applied, and stderr contains at least one
`fs_write_retry` debug event with `attempt < 6`.
</scenario>

<scenario id="CHK-016">
Given a Linux test host and a harness that opens `~/.zshrc`
with `O_EXCL | O_WRONLY` (no Linux equivalent of
`FILE_SHARE_NONE` for FS writes; the equivalent test is to
make the directory non-writable briefly),
when `patina apply --yes` runs,
then the write fails immediately with the OS's normal error
and no retry events appear in the `tracing` log.
</scenario>
</requirement>

## Decisions

<decision id="DEC-001">
The watcher subscribes only to paths listed in the most recent
committed journal — never recursively to the repository directory.
The alternative (recursive watch on the repo, automatic pickup of
new files) was rejected because it surprises users: a file added
to the repo without an explicit `apply` could land at an
unexpected target. The explicit-apply contract preserves the
"stow / dotter / yadm" mental model the user explicitly approved
during the brainstorm: changes to the dotfiles set require a
deliberate `patina apply` to take effect.
</decision>

<decision id="DEC-002">
Debounce is hardcoded at 500ms in v1.0. Alternatives considered:

- User-configurable via `[watcher] debounce_ms` in root
  `patina.toml` — adds a config surface for what is essentially
  a tuning constant; defer to v1.1 if real-world usage shows the
  500ms default is wrong for common editor patterns.
- Adaptive debouncing based on observed event rate — too
  complex for v1.0; deterministic timing is easier to reason
  about and to test.

The 500ms value was chosen because it accommodates the longest
common editor save-burst sequences (~200-300ms) with margin and
still feels responsive to the user.
</decision>

<decision id="DEC-003">
Drift detection emits a desktop notification but never auto-
syncs the target back to source. Auto-sync would directly
violate the brainstorm's safety prior 3.3 ("Never silently sync
state"). The user has two explicit recourses: (1) re-run
`patina apply` to discard the drift and reapply source, or (2)
run `patina promote <target>` (SPEC-0002) to update the source
from the drifted target. Both require deliberate user action.
</decision>

<decision id="DEC-004">
Drift detection is rate-limited at most one notification per
target per 60-second window. A user repeatedly saving a deployed
file should not produce a notification storm. The 60-second
window is hardcoded; configurability is deferred to v1.1.
</decision>

<decision id="DEC-005">
No `--linger` flag in v1.0; Linux survive-logout is documented as
a manual one-shot. Patina's invariant is "the main process never
prompts for elevated privilege"; carrying `loginctl enable-linger`
inside `patina watch install` would compromise that for the
minority of users (servers SSHed in and out of) who actually need
lingering. Alternatives considered and rejected:

- **Default-on lingering**: forces a sudo prompt on every desktop
  user's install, even those who don't need survive-logout.
- **Opt-in `--linger` flag**: still couples Patina to PAM and
  embeds a sudo invocation in the CLI's call tree. Adds CLI
  surface for a niche need.
- **Manual `loginctl enable-linger` documented in docs** (chosen):
  zero CLI surface, fully auditable for the user, no implicit
  privilege escalation. The docs (`docs/USER_GUIDE.md`)
  carry the exact command and a one-line explanation.

A real `--linger` flag remains a v1.1 candidate if users surface a
need that the docs cannot fill.
</decision>

<decision id="DEC-006">
The Windows transient-violation retry budget is 6 attempts with
exponential backoff (50/100/200/400/800/1600 ms), totaling
~3.15s of wait. The 50ms start is short enough to handle the
sub-100ms antivirus scan case quickly; the 1600ms cap is long
enough to outlast typical cloud-sync uploads of small files.
Beyond 6 retries, the engine treats the violation as a real
failure; users with persistent locks have a configuration
problem (a process holding the file long-term) that
retry-with-backoff cannot solve.
</decision>

<decision id="DEC-007">
The watcher does NOT yield the lock to incoming CLI invocations.
The brainstorm-approved "CLI > watcher priority" model means
the watcher skips its own cycle when the CLI holds the lock, but
once the watcher acquires the lock, it does NOT release early
to accommodate a CLI invocation. This keeps the watcher's
re-apply atomic from the lock's perspective. The CLI's wait is
bounded by SPEC-0001's 60s timeout.
</decision>

<decision id="DEC-008">
Symbolic link targets are not separately watched by drift
detection. Modifying a symlinked target IS modifying the source;
the source watcher catches it. Watching both the source and
the symlink target would produce duplicate FS events and
require deduplication logic that adds no information.
</decision>

<decision id="DEC-009">
Watcher metrics are emitted via `tracing` structured logs only.
No `patina watch metrics` subcommand, no in-memory ring buffer,
no HTTP endpoint. Users wanting structured metrics extract them
from the rotated log files at `<state>/patina/logs/` — a directory
and `tracing-appender` daily-rotation (keep-7) logging stack this
SPEC introduces; SPEC-0001 REQ-016 defines only the state-directory
root plus `journal/` and `backups/`. The brainstorm explicitly
rejected the in-memory ring buffer alternative as scope creep.
</decision>

<decision id="DEC-010">
Non-systemd Linux distributions (Void, Devuan with non-systemd
init, Alpine without OpenRC-systemd parity) are served by
`patina watch --foreground`. The user wraps the foreground
watcher in their preferred supervisor (runit, s6, OpenRC). The
brainstorm explicitly deferred shipping templates for these
init systems past v1.0 due to low user count and high template-
maintenance burden.
</decision>

## Open Questions

All four self-review questions resolved 2026-05-26 by user
direction; SPEC content updated in the same revision.

- [x] a. **Linux `--linger` flow.** Deferred to v1.1. Patina's
  CLI in v1.0 does NOT ship a `--linger` flag and does NOT invoke
  `loginctl enable-linger`. REQ-002 was deleted from this SPEC;
  REQ-001 and REQ-003 were rewritten to remove every `--linger`
  reference; DEC-005 was rewritten to record the "docs-only" stance.
  The project docs (`docs/operating-environment.md` at the time of
  the 2026-05-26 amend; subsequently renamed to `docs/USER_GUIDE.md`
  per the 2026-05-27 cross-SPEC reference rename) carry the
  one-shot `sudo loginctl enable-linger $USER` snippet for users
  who actually need survive-logout behavior.
- [x] b. **Watcher journal-rescan mechanism.** Pinned in REQ-005:
  the watcher subscribes via `notify` to the per-machine state
  directory's `journal/` subdirectory and re-reads the most recent
  committed journal whenever a new `.plan` or `.COMMIT` file
  appears there. No polling, no IPC. CHK-017 covers this
  specifically.
- [x] c. **Drift cache file format.** Pinned in REQ-007: postcard
  with a `u16` major version envelope at offset 0, mirroring the
  journal's version envelope (SPEC-0001 REQ-011). A new
  `patina debug drift-cache <path>` subcommand decodes for support
  cases, parallel to `patina debug journal <path>`. CHK-018
  exercises the decoder.
- [x] d. **`ERROR_SHARING_VIOLATION` retry budget configurability.**
  Hardcoded for v1.0 confirmed. The 3.15s budget covers ~99% of
  typical antivirus/cloud-sync transient holds; configurability
  remains a v1.1 candidate if real users surface persistent-lock
  workloads.

## Changelog

<changelog>
| Date       | Author       | Summary |
|------------|--------------|---------|
| 2026-05-25 | human/kevin  | Initial draft. Locks the watch subsystem: per-journal-entry subscriptions, 500ms hardcoded debounce, drift detection via hash-compare on non-symlink targets with `notify-rust` notifications, per-OS service install (LaunchAgent / `systemd --user` / per-user Scheduled Task), `--linger` opt-in for Linux, `--foreground` escape hatch for non-systemd and debugging, Windows `ERROR_SHARING_VIOLATION` retry-with-backoff (6 attempts, ~3.15s), watcher acquires SPEC-0001 advisory lock with CLI priority. |
| 2026-05-26 | human/kevin  | Resolve all four self-review questions. (a) Defer Linux `--linger` flag to v1.1: delete REQ-002, rewrite REQ-001/REQ-003 to remove `--linger` references, rewrite DEC-005 to record the docs-only stance, add a non-goal entry; the user runs `sudo loginctl enable-linger $USER` manually per `docs/operating-environment.md`. (b) Pin watcher journal-rescan mechanism in REQ-005: subscribe via `notify` to `<state>/patina/journal/` and re-read on new `.plan` or `.COMMIT` files; add CHK-017. (c) Lock drift cache format in REQ-007 as postcard with a `u16` version envelope at offset 0; add `patina debug drift-cache <path>` subcommand parallel to `patina debug journal`; add CHK-018. (d) Confirm `ERROR_SHARING_VIOLATION` retry budget remains hardcoded for v1.0. |
| 2026-05-27 | human/kevin via assistant | Rename the docs target from `docs/operating-environment.md` to `docs/USER_GUIDE.md` everywhere SPEC-0003 references it (4 body sites in Assumptions, Non-goals, REQ-001 prose, and DEC-005; the historical 2026-05-26 row above retains the original name as a point-in-time snapshot). SPEC-0001's REQ-027 now formalises `docs/USER_GUIDE.md` with named structural anchors. The `sudo loginctl enable-linger $USER` snippet for survive-logout watcher behavior lands inside `docs/USER_GUIDE.md` in a section SPEC-0003's implementer adds (e.g. extending `## Troubleshooting` or introducing a `## Watch service` section); REQ-027 does not constrain the section name. No requirement-level change in SPEC-0003; this is a cross-SPEC reference rename driven by the SPEC-0001 amend. |
| 2026-05-29 | human/kevin via assistant | Align with the SPEC-0001 REQ-029 amendment and correct a phantom dependency. REQ-005: the watcher reads subscriptions from the committed `<ts>.COMMIT` record (not the `<ts>.plan`, which is deleted at commit), recovering per-target source paths via REQ-029's `Content.source` — fixes the previously unsatisfiable "read sources from the `.plan`". REQ-007: note the drift `blake3` now matches the journal's recorded `blake3` (REQ-029) — same algorithm, directly comparable. REQ-009 / DEC-009 / cross-SPEC handoffs: SPEC-0003 now OWNS the `<state>/patina/logs/` directory and the `tracing-appender` daily-rotation (keep-7) stack; dropped the false attribution to SPEC-0001 REQ-016, which defines only the state-directory root plus `journal/` and `backups/`. Not yet decomposed, so no TASKS reconciliation. |
| 2026-05-29 | human/kevin via assistant | Resolve a status/drift-cache conflict surfaced by reviewing the shipped SPEC-0001 implementation. SPEC-0001's `patina status` already classifies a content target as DRIFTED by live `blake3` re-hash vs the recorded hash (REQ-018, `status/classify.rs`) — standalone and authoritative on *current* content. REQ-007's prior wording had `patina status` read the watcher's `drift.cache` and report DRIFTED from it, which is redundant and wrong for an edited-then-reverted file (live hash CLEAN, cache stale). Reword the Summary and REQ-007 so the drift cache is explicitly the watcher's *notification ledger* (per-target rate limit + `patina debug drift-cache` + watcher metrics), NOT a status input; add a `<done-when>` bullet pinning "the drift cache is not read by `patina status`"; reword CHK-012's premise to derive the DRIFTED verdict from the live H2 ≠ H1 divergence rather than a cache read. No new behaviour and no dependency change; not yet decomposed, so no TASKS reconciliation. |
</changelog>

## Notes

### Cross-SPEC handoffs

SPEC-0003 depends on:

- **SPEC-0001**'s engine, journal format (including the committed
  `ApplyRecord` per REQ-029 — the per-target source paths the
  watcher subscribes around and the blake3 content hash drift
  detection compares against), advisory file lock at
  `<state>/patina/lock`, and per-machine state-directory root. This
  SPEC adds the `logs/` subdirectory and the `tracing-appender`
  daily-rotation logging stack; SPEC-0001 does not define them.
- **SPEC-0002**'s `patina promote` command as the explicit-
  promotion path for drifted targets, `patina doctor` warnings
  for UNC paths and missing Developer Mode, and the `--json`
  conventions across all CLI commands.
- The `winsafe` crate's `taskschd` feature, already declared as
  a SPEC-0002 dependency, is reused here for the Windows
  Scheduled Task creation.

SPEC-0003 introduces no new dependencies on subsequent SPECs
(there are no v1.0 SPECs after this one).

### Rejected design alternatives

- **Recursive watch on the repository directory**. Rejected
  (DEC-001): surprises users by deploying new files without an
  explicit `apply`.
- **User-configurable debounce window**. Rejected (DEC-002):
  config surface for a tuning constant; defer.
- **Auto-sync drifted targets back to source**. Rejected
  (DEC-003): violates the safety prior on silent sync.
- **No rate-limiting on drift notifications**. Rejected
  (DEC-004): a rapidly-saved file would produce a notification
  storm.
- **`--linger` flag on `patina watch install` (any default)**.
  Deferred to v1.1 (DEC-005): keeping the CLI lean and the main
  process never-elevated wins over the convenience of a flag.
  Users who want survive-logout run `sudo loginctl enable-linger
  $USER` manually per the docs.
- **Per-file watcher daemon binary**. Rejected: the `patina`
  binary itself runs as the watcher when invoked with
  `watch --foreground`; no separate daemon binary.
- **IPC-based CLI / watcher coordination**. Rejected: the
  advisory file lock from SPEC-0001 is the single coordination
  point; IPC adds complexity with no offsetting benefit.
- **In-memory metrics ring buffer + `patina watch metrics`
  subcommand**. Rejected (DEC-009): structured `tracing` logs
  cover the same need without exposing an API surface.
- **OpenRC / runit / s6 service file templates shipped with
  Patina**. Rejected (DEC-010): low user count, high template-
  maintenance burden; `--foreground` covers the use case.
- **Watcher polling fallback when native FS events
  unavailable**. Rejected: `patina doctor` warns on UNC paths;
  the user is responsible for local-FS repository placement.

### Tooling notes

SPEC-0003 introduces these new direct dependencies in
`patina-core` (or a new `patina-watcher` module within
`patina-core` — implementation detail):

- `notify` (cross-platform FS events) plus
  `notify-debouncer-full` (debounce wrapper).
- `notify-rust` (cross-platform desktop notifications).

`winsafe`'s `taskschd` feature, already declared in SPEC-0002,
is reused for the Windows Scheduled Task code path.

The `patina watch --foreground` mode is the integration-test
target for SPEC-0003: every scenario above is testable by
spawning the foreground watcher as a subprocess from
`assert_cmd` and inspecting its stderr / cache files.

### v1.1 candidates surfaced by this SPEC

- Configurable debounce window via `[watcher] debounce_ms` in
  root `patina.toml`.
- Cloud-sync provider detection in `patina doctor` (currently
  out of scope per SPEC-0002 non-goals; docs callout only).
- Configurable Windows `ERROR_SHARING_VIOLATION` retry budget.
- OpenRC / runit / s6 service file templates.
- `on_change` and `on_drift` user-defined hook events with
  path-glob targeting.
- `patina watch metrics` query subcommand exposing recent
  re-apply / drift / contention counts from the structured
  log.
