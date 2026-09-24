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

Install from crates.io with a current Rust toolchain (MSRV 1.95):

```sh
cargo install patina --locked
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
| `remove` | Stop managing a target and preserve its applied contents as a regular file. With `--purge`, delete the target instead. Individual leaves of a tree-mode entry cannot be removed this way. |
| `promote` | Copy a changed copy-mode target back to its repository source, then apply again. Remote-backed targets cannot be promoted. |
| `apply` | Materialize declarations as symlinks / rendered templates / byte copies. Diff-and-prompt by default; plan-only in a non-TTY. |
| `status` | Classify each managed target: `clean` / `drifted` / `missing` / `orphaned`. |
| `rollback` | Reverse the most recent successful apply from the journal and backups. |
| `doctor` | Inspect the environment for known problems; `--fix` interactively remediates fixable findings. |
| `remote` | Manage third-party git sources: `list` the pins, `check` upstream tips, `update` a pin through the update gate, `prune` cached checkouts. |
| `watch` | `--foreground` runs the watcher inline; `install` / `uninstall` / `start` / `stop` / `restart` / `status` manage the per-OS background service. |
| `defender` | Windows only. `status` / `apply` / `clear` manage Microsoft Defender path exclusions for the repo and its targets. |
| `debug journal` / `debug drift-cache` | Decode the binary journal / drift cache for post-mortem inspection. |

`add`, `remove`, and `promote` accept absolute paths, paths beginning with
`~`, and paths relative to the current directory. For targets under `$HOME`,
`add` stores a `~`-relative path in the manifest. Other targets remain
absolute.

Every command except the `debug` family accepts `--json` for
deterministic structured output. Commands that prompt for confirmation
accept `--yes` to skip the prompt.

Use `-v key=value` to override variables for `apply` and `status`. The flag
is repeatable. When overrides affect a `when` expression, pass the same
values to both commands.

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

`patina` follows a spec-first workflow. Requirements and acceptance
scenarios are written before implementation, and the code is reviewed
against them. See [`AGENTS.md`](AGENTS.md) for the product north star and
the contributor workflow.

## Contributing

Read [`AGENTS.md`](AGENTS.md) first. It explains the conventions, the
development loop, and what "done" means. Both human and AI-agent
contributions follow the same rules.

### One-time setup (per clone)

```sh
# Activate the local git hooks (pre-commit: fmt + clippy; pre-push: `just check`):
git config core.hooksPath .githooks

# Install stable Rust (builds, tests, Clippy) and nightly rustfmt:
rustup toolchain install stable --profile minimal --component clippy
rustup toolchain install nightly --profile minimal --component rustfmt

# Add the targets `just lint` cross-checks:
rustup target add --toolchain stable x86_64-unknown-linux-gnu x86_64-pc-windows-gnu

# Install the dependency checkers `just dependencies` runs:
cargo +stable install --locked cargo-audit cargo-machete cargo-deny
```

`just check-msrv <package>` also needs Bash and `jq`.

See [`.githooks/README.md`](.githooks/README.md) for details, the git 2.54
`hook.*` alternative, and bypass options.

Formatting uses nightly rustfmt because `.rustfmt.toml` sets unstable
options. For rust-analyzer, set the formatter to nightly:

```json
{
  "rust-analyzer.rustfmt.overrideCommand": ["rustup", "run", "nightly", "rustfmt"]
}
```

### Local quality gate

```sh
just check        # = just lint + just test + just doc + just dependencies
just fix          # apply Clippy fixes and formatting, then run just lint
```

On Windows, `just test` needs Developer Mode or an elevated shell. Without
either, each `patina` CLI symlink test launches `patina-elevate`, which
raises a UAC prompt, and the `patina-core` symlink tests fail.

CI runs `just lint` and `just test` natively on macOS, Linux, and Windows;
the Linux test job runs `just coverage` in place of `just test`. `just doc`,
`just dependencies`, and `just check-msrv` for each package run on Linux.
Watch the PR checks after pushing.

## License

[MIT](LICENSE)
