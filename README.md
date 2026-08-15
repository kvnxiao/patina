# patina

[![codecov](https://codecov.io/gh/kvnxiao/patina/branch/main/graph/badge.svg)](https://codecov.io/gh/kvnxiao/patina/tree/main)

Your dotfiles, oxidized to perfection.

`patina` is a dotfile manager written in Rust. One centralized git
repository drives macOS, Linux, and Windows. Most targets are symbolic
links back into that repo; a `.tmpl` source renders through MiniJinja, and
a byte copy covers what a link cannot. Profiles and per-OS variation pick
what lands on this machine, and one source can fan out to many targets. A
background watcher re-applies on change, installed as a per-OS service.
Apply is transactional: a `kill -9` mid-run leaves either the pre-apply
or the post-apply state.

**Status:** Pre-release (`0.1.0`), in active development. The v1.0 command
surface is implemented and tested on macOS, Linux, and Windows. Interfaces
may still shift before a tagged release.

## Install

No prebuilt binaries yet. Build from source with a current Rust toolchain
(MSRV 1.95):

```sh
cargo install --path patina-cli --locked
patina --version
```

On Windows, creating symbolic links requires either Developer Mode enabled
or an elevated (UAC) session. `patina doctor --fix` offers to turn
Developer Mode on under a single UAC prompt.

## Quick start

```sh
patina init                 # scaffold a root patina.toml + persist the repo pointer
patina add ~/.zshrc         # bring an existing dotfile under management
patina apply                # materialize managed files (diff-and-prompt by default)
patina status               # report drift between the repo and your machine
patina watch install        # auto-reapply on change via a per-OS background service
```

## Commands

| Command | What it does |
| --- | --- |
| `init` | Scaffold a root `patina.toml` and persist the default-repo pointer. |
| `add` | Bring an existing dotfile under management: copy it into a module and write a `[[file]]` or `[[directory]]` entry by source kind. |
| `remove` | Unmanage a target (replace it with a plain file holding the last-applied content); `--purge` deletes it outright. |
| `promote` | Copy a drifted copy-mode target's current bytes back into its repo source, then re-apply. |
| `apply` | Materialize declarations as symlinks / rendered templates / byte copies. Diff-and-prompt by default; plan-only in a non-TTY. |
| `status` | Classify each managed target: `clean` / `drifted` / `missing` / `orphaned`. |
| `rollback` | Reverse the most recent successful apply from the journal and backups. |
| `doctor` | Inspect the environment for known problems; `--fix` interactively remediates fixable findings. |
| `remote` | Manage third-party git sources: `list` the pins, `check` upstream tips, `update` a pin through the update gate, `prune` cached checkouts. |
| `watch` | `--foreground` runs the watcher inline; `install` / `uninstall` / `start` / `stop` / `restart` / `status` manage the per-OS background service. |
| `defender` | Windows only. `status` / `apply` / `clear` manage Microsoft Defender path exclusions for the repo and its targets. |
| `debug journal` / `debug drift-cache` | Decode the binary journal / drift cache for post-mortem inspection. |

Every command except the `debug` family accepts `--json` for
deterministic structured output. Commands that prompt for confirmation
accept `--yes` to skip the prompt.

## Documentation

- [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) for users: install, declaring
  dotfiles, the apply flow, the watch service, recovery, and troubleshooting.
- [`docs/OPERATING_ENVIRONMENT.md`](docs/OPERATING_ENVIRONMENT.md) for
  operations: state-directory layout, the cloud-sync caveat, and Linux
  `enable-linger`.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for architecture: crate
  layout, journal format, apply phases, and recovery.
- [`AGENTS.md`](AGENTS.md) for agents, also reachable as `CLAUDE.md`.

## Design

`patina` is built spec-first. Every slice gets written requirements and
acceptance scenarios before anyone implements it, and review checks the
code against them. See [`AGENTS.md`](AGENTS.md) for the product north star
and the contributor workflow.

## Contributing

Read [`AGENTS.md`](AGENTS.md) first. It explains the conventions, the
development loop, and what "done" means. Both human and AI-agent
contributions follow the same rules.

### One-time setup (per clone)

```sh
# Activate the local git hooks (pre-commit: fmt + clippy; pre-push: `just check`):
git config core.hooksPath .githooks

# Install the nightly toolchain used for formatting:
rustup toolchain install nightly --component rustfmt
```

See [`.githooks/README.md`](.githooks/README.md) for details, the git 2.54
`hook.*` alternative, and bypass options.

### Local quality gate

```sh
just check        # = just lint + just test; run before opening a PR
```

CI runs the same gates natively across macOS, Linux, and Windows, plus the
per-OS test-behaviour matrix, the MSRV build, and coverage. Watch the PR
checks after pushing.

## License

[MIT](LICENSE)
