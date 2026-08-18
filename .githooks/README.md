# patina local git hooks

Git hooks for Patina contributors. They run the local checks that form the
corresponding pull-request gate.

## What runs

| Hook | Checks |
|---|---|
| `pre-commit` | Fast inner-loop gate: `cargo +nightly fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `pre-push` | Full local gate: `just check` (= `just lint`, which is fmt + clippy + **docs** (`cargo doc -D warnings`) + `cargo deny`, then `just test`, which is `cargo test --workspace --locked`) |

`pre-commit` keeps the per-commit loop fast: format and lint only. `pre-push` runs the heavier gate once before code leaves your machine, adding the `docs` and `cargo deny` checks the commit gate skips. It needs [`just`](https://github.com/casey/just) on `PATH`.

CI still runs gates neither hook can reproduce on one box: the Windows/macOS/Linux test matrix, the MSRV (Rust 1.95) build, and coverage. Watch the PR checks after pushing.

Both hooks are a no-op when no `Cargo.toml` exists yet.

## Activation (one-time per clone)

**Git does not auto-apply hooks from a committed directory** for [well-known security reasons](https://www.collabora.com/news-and-blog/news-and-events/git-hooks-upgraded-whats-new-git-254-and-coming-255.html), and that policy did not change in git 2.54. Each contributor must wire up the hooks once after cloning:

```sh
git config core.hooksPath .githooks
```

The command sets `core.hooksPath` in your local `.git/config`, so Git runs the
matching hook from this directory.

You also need the nightly Rust toolchain (the `pre-commit` hook uses `cargo +nightly fmt`):

```sh
rustup toolchain install nightly --component rustfmt
```

### Verify

```sh
git config --get core.hooksPath   # should print: .githooks
rustup toolchain list             # should include 'nightly-...'
```

## Bypass / disable

- One-off bypass: `git commit --no-verify` / `git push --no-verify` (CI will still gate the PR).
- Disable entirely: `git config --unset core.hooksPath`.

## Git 2.54 `hook.*` namespace (optional)

Git 2.54 introduced a config-based `hook.*` namespace that declares hooks in `.git/config` instead of by filename. The `core.hooksPath` approach above covers Patina's setup, but if you prefer the new mechanism:

```sh
git config hook.patina-fmt-clippy.event   pre-commit
git config hook.patina-fmt-clippy.command "$(pwd)/.githooks/pre-commit"
git hook list                              # inspect what runs
```

Either approach is **local-only and not committed**: both write to `.git/config`, outside the worktree. Git still offers no way to ship hook config inside the repo and have it auto-apply on clone.

## On Linux/macOS: executable bit

Each hook needs the executable bit set. After committing the file, mark it executable in git's index:

```sh
git update-index --chmod=+x .githooks/pre-commit .githooks/pre-push
```

The command does nothing on Windows, and recording the bit saves Linux/macOS contributors a `chmod` after pulling.
