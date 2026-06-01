# patina

Your dotfiles, oxidized to perfection.

`patina` is a symlink-first, multi-destination, layered dotfile manager
written in Rust. It manages your dotfiles across macOS, Linux, and Windows
from a single centralized git repository: it primarily creates symbolic
links from that repo to system locations, with optional templated copies,
profile-based selection, and per-OS variation. First-class support for
multi-destination links (one source → many targets), an always-on watcher
with per-OS service installation, and crash-safe transactional apply.

**Status:** Pre-release (`0.1.0`), in active development. The v1.0 command
surface is implemented and tested on macOS, Linux, and Windows, but there
are no published binaries yet — install from source (below). Interfaces may
still shift before a tagged release.

## Install

No prebuilt binaries yet. Build from source with a current Rust toolchain
(MSRV 1.95):

```sh
cargo install --path patina-cli --locked
patina --version
```

On Windows, creating symbolic links requires either Developer Mode enabled
or an elevated (UAC) session; `patina doctor --fix` walks you through
enabling Developer Mode.

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
| `add` | Bring an existing dotfile under management — copy it into a module and write a `[[file]]` entry. |
| `remove` | Unmanage a target (replace it with a plain file holding the last-applied content); `--purge` deletes it outright. |
| `promote` | Copy a drifted copy-mode target's current bytes back into its repo source, then re-apply. |
| `apply` | Materialize declarations as symlinks / rendered templates / byte copies. Diff-and-prompt by default; plan-only in a non-TTY. |
| `status` | Classify each managed target: `CLEAN` / `DRIFTED` / `MISSING` / `ORPHANED`. |
| `rollback` | Reverse the most recent successful apply from the journal and backups. |
| `doctor` | Inspect the environment for known problems; `--fix` interactively remediates fixable findings. |
| `watch` | `--foreground` runs the watcher inline; `install` / `uninstall` / `start` / `stop` / `restart` / `status` manage the per-OS background service. |
| `debug journal` / `debug drift-cache` | Decode the binary journal / drift cache for post-mortem inspection. |

Every command accepts `--json` for deterministic structured output;
mutating commands accept `--yes` to skip the confirmation prompt.

## Documentation

- **Users** — [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md): install, declaring
  dotfiles, the apply flow, the watch service, recovery, and troubleshooting.
- **Operations** — [`docs/OPERATING_ENVIRONMENT.md`](docs/OPERATING_ENVIRONMENT.md):
  state-directory layout, the cloud-sync caveat, and Linux `enable-linger`.
- **Architecture** — [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): crate
  layout, journal format, apply phases, and recovery.
- **Agents** — [`AGENTS.md`](AGENTS.md) (also reachable as `CLAUDE.md`).

## Design

`patina` is built spec-first. Each slice of the product is designed,
decomposed, implemented, and reviewed against written requirements with
acceptance scenarios before it ships. See [`AGENTS.md`](AGENTS.md) for the
product north star and the contributor workflow.

## Contributing

Read [`AGENTS.md`](AGENTS.md) first — it explains the conventions, the
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

CI runs the same gates natively across macOS, Linux, and Windows. A green
local `just check` is necessary but not sufficient — the full per-OS test
behaviour matrix, the MSRV build, and coverage run only in CI.

## License

[MIT](LICENSE)
