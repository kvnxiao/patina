# patina — local git hooks

Git hooks for patina contributors. They mirror the fast gate CI runs on every
PR, so a clean commit means a clean PR check.

## What runs

| Hook | Checks |
|---|---|
| `pre-commit` | Fast inner-loop gate: `cargo +nightly fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `pre-push` | Full local gate: `just check` (= `just lint` — fmt + clippy + **docs** (`cargo doc -D warnings`) + `cargo deny` — then `just test` — `cargo test --workspace --locked`) |

`pre-commit` keeps the per-commit loop fast (format + lint only); `pre-push` runs the heavier, comprehensive gate once before code leaves your machine — including the `docs` and `cargo deny` checks a fast commit gate skips. `pre-push` needs [`just`](https://github.com/casey/just) on `PATH`.

CI still runs gates neither hook can reproduce on one box — the Windows/macOS/Linux test matrix, the MSRV (Rust 1.95) build, and coverage — so a green `pre-push` is necessary, not sufficient. Watch the PR checks after pushing.

Both hooks are a no-op when no `Cargo.toml` exists yet.

## Activation (one-time per clone)

**Git does not auto-apply hooks from a committed directory** for [well-known security reasons](https://www.collabora.com/news-and-blog/news-and-events/git-hooks-upgraded-whats-new-git-254-and-coming-255.html) — that policy did not change in git 2.54. Each contributor must wire up the hooks once after cloning:

```sh
git config core.hooksPath .githooks
```

That sets your local `.git/config` to point at this directory, and all hooks here run on relevant git events.

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

Git 2.54 introduced a config-based `hook.*` namespace that lets you declare hooks in `.git/config` rather than via filenames. Patina's hook setup is simple enough that the file-based `core.hooksPath` approach above is fine, but if you prefer the new mechanism:

```sh
git config hook.patina-fmt-clippy.event   pre-commit
git config hook.patina-fmt-clippy.command "$(pwd)/.githooks/pre-commit"
git hook list                              # inspect what runs
```

Either approach is **local-only and not committed** — both write to `.git/config`, which lives outside the worktree. There is no git mechanism (yet) to ship hook config inside the repo and have it auto-apply on clone.

## On Linux/macOS: executable bit

Each hook needs the executable bit set. After committing the file, mark it executable in git's index:

```sh
git update-index --chmod=+x .githooks/pre-commit .githooks/pre-push
```

On Windows this is a no-op but recording the bit ensures Linux/macOS contributors don't have to chmod after pulling.
