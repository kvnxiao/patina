---
paths: **/*.{rs,toml}
---

# Multi-Crate Workspaces

> **Scope:** This rule applies only when the project is structured as a multi-crate Cargo workspace (i.e. the root `Cargo.toml` contains a `[workspace]` table). For a single-crate project, ignore the workspace-specific guidance below; use crate-level `[lints]`, `[dependencies]`, and `[package]` sections per the other rule files in this directory.

## Workspace Structure: Root-Level Crates

**Prefer placing workspace crates at the root level** rather than nested in a `crates/` directory.

### Recommended Structure

```
my-project/
├── Cargo.toml          # Workspace root
├── Cargo.lock          # Shared lock file
├── my-core/            # Core library crate
│   ├── Cargo.toml
│   └── src/
├── my-cli/             # CLI binary crate
│   ├── Cargo.toml
│   └── src/
└── my-utils/           # Utilities crate
    ├── Cargo.toml
    └── src/
```

**Why root-level is better:**
- Shorter import paths in IDEs
- Easier navigation - less nesting
- Simpler to understand project structure
- Matches common Rust ecosystem conventions

## Workspace Root `Cargo.toml`

```toml
[workspace]
members = ["my-core", "my-cli", "my-utils"]
resolver = "2"

[workspace.dependencies]
tokio = { version = "1.35", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
thiserror = "2.0"

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

# Clippy lints: configure the project's strict set under
# `[workspace.lints.clippy]` (not `[lints.clippy]`) at the workspace root,
# alongside a `clippy.toml` that sets `allow-expect-in-tests = true`.
[workspace.lints.clippy]
# … project-specific strict lints …

[workspace.package]
edition = "2024"
rust-version = "1.95"
license = "MIT OR Apache-2.0"

[profile.release]
opt-level = 3
lto = "thin"
strip = true
```

Member crates inherit the lint set via `[lints] workspace = true` (see the member-crate example below).

## Member Crate `Cargo.toml`

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

# Crate-specific dependencies
uuid = { version = "1.6", features = ["v4"] }

[lints]
workspace = true
```

## Workspace Best Practices

### 1. Dependency Management

Use workspace dependencies for shared crates:

```toml
# Root Cargo.toml
[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1.35", default-features = false }

# Member crates can enable additional features
# my-server/Cargo.toml
[dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "net"] }
```

### 2. Inter-Crate Dependencies

Use path dependencies for workspace members:

```toml
[dependencies]
my-core = { path = "../my-core", version = "0.2.0" }
```

### 3. Avoid Circular Dependencies

```toml
# BAD: Circular dependency
# my-core depends on my-utils
# my-utils depends on my-core

# GOOD: Create third crate for shared types
# my-types (no dependencies on other workspace crates)
# my-core depends on my-types
# my-utils depends on my-types
```

## Workspace Commands

```bash
# Build entire workspace
cargo build

# Build specific crate
cargo build -p my-cli

# Check all workspace members
cargo check --workspace

# Run tests for specific crate
cargo test -p my-core

# Run tests with all features
cargo test --all-features
```

## Workspace Checklist

- [ ] Use root-level crate directories (not `crates/` folder)
- [ ] Configure `[workspace.dependencies]` for shared deps
- [ ] Set `[workspace.lints]` for consistent code quality
- [ ] Use `[workspace.package]` for shared metadata
- [ ] Define clear boundaries between crates
- [ ] Avoid circular dependencies
- [ ] Use path dependencies with versions
- [ ] Test entire workspace with `cargo test --workspace`
