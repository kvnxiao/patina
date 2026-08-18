# List the available recipes.
default:
    @just --list

# Run lint and tests, stopping at the first failure.
check: lint test

# Run the local lint gates in CI's order, stopping at the first failure.
lint: lint-fmt lint-clippy lint-docs lint-deny

# Check formatting with nightly rustfmt; install the nightly toolchain and rustfmt component.
lint-fmt:
    cargo +nightly fmt --all --check

# Run Clippy with warnings denied on the configured OS targets.
lint-clippy:
    cargo clippy --workspace --all-targets --all-features --locked --target x86_64-unknown-linux-gnu -- -D warnings
    cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-gnu -- -D warnings
    {{ if os() == "macos" { "cargo clippy --workspace --all-targets --all-features --locked --target aarch64-apple-darwin -- -D warnings" } else { "echo 'lint-clippy: skipping aarch64-apple-darwin (compiles Objective-C; needs a macOS host - CI lints it on macos-latest)'" } }}

# Build workspace documentation with warnings denied; reject broken and private doc links.
lint-docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# Check license, advisory, ban, and source policies with cargo-deny.
lint-deny:
    cargo deny check

# Format the workspace with nightly rustfmt.
fmt:
    cargo +nightly fmt --all

# Run the workspace tests on the current host.
test:
    cargo test --workspace --locked

# Build the release binaries, including `patina-elevate` on Windows.
build:
    cargo build --release --locked -p patina
    {{ if os() == "windows" { "cargo build --release --locked -p patina-elevate --features patina-elevate/windows" } else { "echo 'build: skipping patina-elevate (Windows-only Developer Mode UAC helper)'" } }}

# On Windows, `patina.exe` resolves `patina-elevate.exe` beside itself; `--force`
# reinstalls the current working-tree build when its version remains `0.1.0`.
# Install `patina` and, on Windows, `patina-elevate` into Cargo's bin directory.
install:
    cargo install --path patina-cli --locked --force
    {{ if os() == "windows" { "cargo install --path patina-elevate --features windows --locked --force" } else { "echo 'install: skipping patina-elevate (Windows-only Developer Mode UAC helper)'" } }}
