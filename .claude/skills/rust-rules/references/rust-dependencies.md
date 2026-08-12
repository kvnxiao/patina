---
paths: **/*.{rs,toml}
description: "Cargo dependency management; caret ranges with a committed Cargo.lock, preferred crates, when to pin, feature minimalism, MSRV single-sourcing."
---

# Dependency Management

## Default to Caret/Semver Ranges

For both libraries and applications, declare dependencies with caret/semver ranges (Cargo's default for unprefixed versions). A committed `Cargo.lock` is what guarantees reproducibility, not exact-pin specs in `Cargo.toml`.

```toml
[dependencies]
serde = { version = "1.0", features = ["derive"] }   # ≡ ^1.0, allows 1.x updates
tokio = { version = "1.35", features = ["rt-multi-thread", "net", "macros"] }
```

Commit `Cargo.lock` for binaries (always) and for libraries when reproducibility of the test/CI environment matters. Caret bounds let `cargo update` pull in patch fixes; the lockfile freezes the actual resolved versions for reproducible builds.

## Preferred Crates

When a task has a clear best-in-class crate in the Rust ecosystem, default to it. Documented here so dependency choices don't get re-litigated in every PR.

| Domain | Use | Avoid |
|---|---|---|
| Dates and times | `jiff` | `chrono`, `time` |
| Filesystem IO | `fs-err` | bare `std::fs` (errors lose path context) |
| UTF-8 paths | `camino` (`Utf8Path` / `Utf8PathBuf`) | `std::path::Path` / `PathBuf` outside OS-API boundaries |

Deviations need a comment explaining the reason, similar to exact-version pins. A transitive dependency pulling in `chrono` or `time` is acceptable; first-party code reaching for them is not.

## Pin Exact Versions Only When Necessary

Exact pins (`"=1.2.3"`) restrict Cargo's resolver, prevent normal updates, and can cause version-resolution conflicts in workspaces or downstream consumers. Use them in narrow cases only, and **always include a comment** explaining the reason and the condition under which the pin should be removed:

- **Patch-version regression.** A patch release introduced a bug or behavior change that breaks you. Pin to the last good version until upstream fixes it, and link the issue.
- **Behavioral dependency on a specific version.** You rely on a quirk that isn't part of the crate's contract and could shift across patches. Prefer fixing your code over pinning, but pin if the fix is non-trivial.
- **`cargo install` distribution.** Binaries published via `cargo install` ignore `Cargo.lock` by default unless `--locked` is passed; if you can't guarantee `--locked`, pinning is the only way to lock end-user versions.
- **Resolver conflict resolution.** A transitive-version conflict requires a specific version to keep the dep graph valid.
- **Tightly-coupled internal crate pair.** A facade crate that re-exports a private helper relying on the facade's unstable internals (`serde` ↔ `serde_core`, `thiserror` ↔ `thiserror-impl`, `jiff` ↔ `jiff-static`) pins the helper with `=` to the exact same version. They ship in lockstep and call each other's private APIs, so a mismatched pair would break. This is the one place a `=` pin is the norm rather than a smell: you own both crates and bump them together.

```toml
[dependencies]
# Patch regression: 1.0.196 has a bug in #[serde(flatten)] handling.
# Re-evaluate after 1.0.198 ships. https://github.com/serde-rs/serde/issues/XXXX
serde = "=1.0.195"

# Facade/impl pair (you own both): ship in lockstep, rely on private APIs.
thiserror-impl = { version = "=2.0.18", path = "impl" }
```

If none of the above apply, use a caret range. **Do not pin pre-emptively for "stability"**; the lockfile already provides that, and exact pins make security/patch updates a manual chore.

## Enable Only Needed Features

```toml
[dependencies]
tokio = { version = "1.35", features = ["rt-multi-thread", "net", "macros"] }
serde = { version = "1.0", features = ["derive"] }
# Not: features = ["full"]
```

## Review Dependencies Regularly

```bash
# Check for outdated dependencies
cargo outdated

# Audit for security vulnerabilities
cargo audit

# Check for unused dependencies
cargo machete
```

## Single-Source the MSRV

Declare `rust-version` once in `[package]` (or `[workspace.package]`) and have CI read the MSRV from there, for example by installing the toolchain from the declared `rust-version` and running `cargo check`. Don't also hard-code the version number in CI YAML; a second copy drifts out of sync.
