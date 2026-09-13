---
paths: **/*.{rs,toml}
description: "Multi-crate Cargo workspace layout; root-level crates, workspace dependencies, lints, and package inheritance, inter-crate deps, and avoiding cycles."
---

# Multi-Crate Workspaces

> **Scope:** This rule applies only when the project is structured as a multi-crate Cargo workspace
> (i.e. the root `Cargo.toml` contains a `[workspace]` table). For a single-crate project, ignore
> the workspace-specific guidance below and use crate-level `[lints]`, `[dependencies]`, and
> `[package]` sections instead.

## Workspace Structure: Root-Level Crates (Default)

Default to root-level crate directories for a small workspace. Use a `crates/` directory when the
repository has many top-level concerns or enough members that grouping improves navigation.

### Recommended Structure

```
my-project/
├── Cargo.toml
├── Cargo.lock
├── my-core/
│   ├── Cargo.toml
│   └── src/
├── my-cli/
│   ├── Cargo.toml
│   └── src/
└── my-utils/
    ├── Cargo.toml
    └── src/
```

Root-level members keep paths short and expose crate boundaries directly.

## Workspace Root `Cargo.toml` (Default)

Set `resolver = "3"` explicitly in a virtual workspace. A virtual workspace has no root package
edition from which Cargo can infer the resolver; without the field, Cargo warns and defaults to
resolver `"1"`. See
[Cargo resolver versions](https://doc.rust-lang.org/cargo/reference/resolver.html#resolver-versions).

```toml
[workspace]
members = ["my-core", "my-cli", "my-utils"]
resolver = "3"

[workspace.dependencies]
tokio = { version = "1.35", features = ["rt-multi-thread", "macros"] }
serde = { version = "1.0", features = ["derive"] }
thiserror = "2.0"

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -2 }

[workspace.package]
edition = "2024"
rust-version = "1.95"
license = "MIT OR Apache-2.0"
```

Member crates inherit the lint set via `[lints] workspace = true` (see the member-crate example
below).

## Member Crate `Cargo.toml` (Default)

Default member manifests to inherited workspace metadata, dependencies, and lints. Keep a field
local when the member intentionally differs.

```toml
[package]
name = "my-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio.workspace = true
serde.workspace = true
thiserror.workspace = true

uuid = { version = "1.6", features = ["v4"] }

[lints]
workspace = true
```

## Workspace Best Practices (Default)

### 1. Dependency Management

Default dependencies used by several members to `[workspace.dependencies]`. Keep a dependency local
when its version or feature policy is member-specific.

```toml
[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1.35", default-features = false }

[dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "net"] }
```

### 2. Inter-Crate Dependencies

Default inter-member dependencies to versioned path dependencies. Omit the version only when the
workspace will never publish the dependent crate.

```toml
[dependencies]
my-core = { path = "../my-core", version = "0.2.0" }
```

### 3. Keep the dependency graph acyclic

Cargo rejects cycles among normal dependencies at build time. Cargo permits a dev-dependency cycle,
but a library's unit-test binary can then link two copies of that library with incompatible type
identities. Default to an acyclic architecture; move shared contracts into a lower-level crate when
a dev-dependency would point back to its dependent. See
[Cargo's dev-dependency cycle guidance](https://doc.rust-lang.org/cargo/reference/resolver.html#dev-dependency-cycles).

## Workspace Commands

```bash
cargo build

cargo build -p my-cli

cargo check --workspace

cargo test -p my-core

cargo test --all-features
```
