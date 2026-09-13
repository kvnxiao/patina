---
paths: **/*.{rs,toml}
description: "Cargo dependency management; caret ranges with a committed Cargo.lock, preferred crates, when to pin, feature minimalism, MSRV single-sourcing."
---

# Dependency Management

## Default to Caret/Semver Ranges (Default)

For both libraries and applications, default dependency declarations to caret or SemVer ranges,
which Cargo uses for unprefixed versions. A committed `Cargo.lock` fixes the resolved versions; an
exact `Cargo.toml` requirement is not needed for reproducibility.

```toml
[dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1.35", features = ["rt-multi-thread", "net", "macros"] }
```

Default binaries to a committed `Cargo.lock`. For libraries, commit it when the test or CI
environment needs reproducible resolution. Caret bounds let `cargo update` select patch fixes while
the lockfile keeps normal builds fixed.

## Preferred Crates (Default)

For projects using this rule pack, default to the preferred crates below when their contracts fit
the task. Use an alternative when compatibility, platform support, an existing dependency graph, or
a missing capability makes it a better fit.

| Domain          | Preferred                             | Common alternative            |
| --------------- | ------------------------------------- | ----------------------------- |
| Dates and times | `jiff`                                | `chrono`, `time`              |
| Filesystem IO   | `fs-err`                              | `std::fs`                     |
| UTF-8 paths     | `camino` (`Utf8Path` / `Utf8PathBuf`) | `std::path::Path` / `PathBuf` |

Record lasting dependency policy and approved deviations in project documentation. A transitive
dependency on `chrono` or `time` does not require first-party code to adopt it.

## Pin Exact Versions Only When Necessary (Default)

Exact pins (`"=1.2.3"`) restrict Cargo's resolver, prevent normal updates, and can cause
version-resolution conflicts in workspaces or downstream consumers. Use them only for a named
constraint:

- **Patch-version regression.** A patch release introduced a bug or behavior change that breaks you.
  Pin to the last good version until upstream fixes it, and link the issue.
- **Behavioral dependency on a specific version.** You rely on a quirk that isn't part of the
  crate's contract and could shift across patches. Prefer fixing your code over pinning, but pin if
  the fix is non-trivial.
- **`cargo install` distribution.** Binaries published via `cargo install` ignore `Cargo.lock` by
  default unless `--locked` is passed; if you can't guarantee `--locked`, pinning is the only way to
  lock end-user versions.
- **Resolver conflict resolution.** A transitive-version conflict requires a specific version to
  keep the dep graph valid.
- **Tightly-coupled internal crate pair.** A facade crate that re-exports a private helper relying
  on the facade's unstable internals (`serde` ↔ `serde_core`, `thiserror` ↔ `thiserror-impl`, `jiff`
  ↔ `jiff-static`) pins the helper with `=` to the exact same version. They ship in lockstep and
  call each other's private APIs, so a mismatched pair would break. This is the one place a `=` pin
  is the norm rather than a smell — you own both crates and bump them together.

```toml
[dependencies]
# Temporary pin: remove after https://github.com/serde-rs/serde/issues/XXXX ships.
serde = "=1.0.195"

thiserror-impl = { version = "=2.0.18", path = "impl" }
```

For a temporary pin, add an adjacent comment with its removal condition and issue link. For a
lasting pin such as a coupled internal crate pair, record the policy in project documentation. If no
named constraint applies, use a caret range; the lockfile already fixes resolved versions for
reproducible builds.

## Enable Only Needed Features (Default)

Default dependency declarations to the features the project uses. Enable a full feature set when the
dependency contract requires it or selective features would create an unsupported combination.

```toml
[dependencies]
tokio = { version = "1.35", features = ["rt-multi-thread", "net", "macros"] }
serde = { version = "1.0", features = ["derive"] }
```

## Review Dependencies Regularly (Default)

Default dependency maintenance to automated update checks, security audits, and unused-dependency
detection. Substitute equivalent tools when the project already standardizes on them.

```bash
cargo outdated

cargo audit

cargo machete
```

## Single-Source the MSRV (Default)

Default `rust-version` to one declaration in `[package]` or `[workspace.package]`, and have CI read
the MSRV from it. A second version in CI YAML can drift unless external tooling requires and
verifies both values.
