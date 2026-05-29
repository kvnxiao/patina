# patina

Your dotfiles, oxidized to perfection.

`patina` is a symlink-first, multi-destination, layered dotfile manager written in Rust. It manages your dotfiles across macOS, Linux, and Windows machines. It primarily creates symlinks from a centralized dotfiles repository to system locations, with optional templated copies, profile-based selection, and per-OS variation. First-class support for multi-destination symlinks (one source → many targets), always-on watch with OS service installation, and transactional apply.

**Status:** Pre-release. Implementation tracked under SPEC 0001. See `specs/0001-mvp/STATUS.md`.

## Quick start (planned, post-v1.0)

```sh
# install (TBD — cargo install / homebrew / scoop)
patina init                     # scaffold a new dotfiles repo
patina add ~/.zshrc             # bring an existing dotfile under management
patina apply                    # deploy all managed files to the system
patina watch install            # auto-deploy on file change, surviving reboots
```

## Documentation

- **For users:** `docs/user/` — getting started, watch mode, platform setup, migration from other tools.
- **For contributors:** `docs/dev/` — architecture, testing, ADRs.
- **For agents:** `AGENTS.md` (also at `CLAUDE.md`, `.cursorrules`).

## Design

`patina` is built SPEC-first. All design lives under `specs/`:

- `specs/0001-mvp/` — v1.0.0 MVP design and implementation phases.
- `specs/0002-merge-modes/` — v1.1.0 follow-up: merge modes for app-managed config files.

See `specs/README.md` for the SPEC process.

## Contributing

Read `AGENTS.md` first. It explains the workflow, conventions, and what "done" means.

### One-time setup (per clone)

```sh
# Activate the local pre-commit hooks (fmt + clippy, same gate as CI):
git config core.hooksPath .githooks

# Install the nightly toolchain used for formatting:
rustup toolchain install nightly --component rustfmt
```

See `.githooks/README.md` for details, the git 2.54 `hook.*` alternative, and bypass options.

### Workflow TL;DR

1. Pick up a 📋 Ready phase from a SPEC's `STATUS.md`.
2. Implement, write tests, write ADRs as needed.
3. Open a PR; CI gates merge.
4. STATUS.md updates to ✅ Complete on merge.

Both human and AI agent contributions are welcome, following the same rules.

## License

[MIT](LICENSE)
