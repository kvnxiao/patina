clippy_scope := "--workspace --all-targets --all-features --locked"

# `lint` runs Clippy for the host target and for these targets, so it lints
# the Linux and Windows `cfg` code on any host. Building for
# `aarch64-apple-darwin` compiles Objective-C, so `lint` checks that target
# only on a macOS host.
cross_targets := if os() == "windows" { "x86_64-unknown-linux-gnu" } else if os() == "linux" { "x86_64-pc-windows-gnu" } else { "x86_64-unknown-linux-gnu x86_64-pc-windows-gnu" }

# List the available recipes.
default:
    @just --list

# Run lint, tests, docs, and dependency checks, stopping at the first failure.
check: lint test doc dependencies

# Format the workspace with nightly rustfmt.
fmt:
    cargo +nightly fmt --all

# Check formatting, then run Clippy with warnings denied on the host and cross targets.
lint:
    cargo +nightly fmt --all --check
    cargo +stable clippy {{ clippy_scope }} -- -D warnings
    for target in {{ cross_targets }}; do cargo +stable clippy {{ clippy_scope }} --target "$target" -- -D warnings || exit 1; done

# Apply Clippy fixes and formatting, then verify the result with `lint`.
fix:
    cargo +stable clippy --fix {{ clippy_scope }} --allow-dirty
    just fmt
    just lint

# Run the workspace tests on the host.
test:
    cargo +stable test --workspace --all-features --locked

# Run the workspace tests under coverage instrumentation and write `lcov.info`.
coverage:
    cargo +stable llvm-cov --workspace --all-features --locked --fail-under-lines 0 --lcov --output-path lcov.info

# Build workspace documentation with warnings denied.
doc:
    RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings" cargo +stable doc --workspace --all-features --no-deps --locked

# Check advisories, unused dependencies, and license, ban, and source policy.
dependencies:
    cargo +stable audit
    cargo +stable machete
    cargo +stable deny check

# Check one package with the toolchain named by its resolved `rust-version`.
[positional-arguments]
check-msrv package:
    #!/usr/bin/env bash
    set -euo pipefail
    metadata=$(cargo +stable metadata --no-deps --format-version 1 --locked)
    msrv=$(jq -er --arg name "$1" '
        .workspace_members as $members
        | .packages[]
        | select(.id as $id | $members | index($id))
        | select(.name == $name)
        | .rust_version // error("selected package must declare rust-version")
    ' <<< "$metadata")
    rustup toolchain install "$msrv" --profile minimal
    cargo +"$msrv" check --package "$1" --all-targets --all-features --locked

# Build the release binaries, including `patina-elevate` on Windows.
build:
    cargo +stable build --release --locked -p patina
    {{ if os() == "windows" { "cargo +stable build --release --locked -p patina-elevate --features patina-elevate/windows" } else { "echo 'build: skipping patina-elevate (Windows-only Developer Mode UAC helper)'" } }}

# On Windows, `patina.exe` resolves `patina-elevate.exe` beside itself; `--force`
# reinstalls the current working-tree build when its version remains `0.1.0`.
# Install `patina` and, on Windows, `patina-elevate` into Cargo's bin directory.
install:
    cargo +stable install --path patina-cli --locked --force
    {{ if os() == "windows" { "cargo +stable install --path patina-elevate --features windows --locked --force" } else { "echo 'install: skipping patina-elevate (Windows-only Developer Mode UAC helper)'" } }}
