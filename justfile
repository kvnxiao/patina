# Patina developer task runner. `just` or `just --list` shows all recipes.
#
# `just lint` runs every linting / quality gate the CI workflow enforces
# (.github/workflows/ci.yml), in the same order, with the same commands, so a
# green `just lint` locally means those CI jobs will pass:
#
#   just recipe     CI job
#   ------------    --------------------
#   lint-fmt        Format (nightly)
#   lint-clippy     Clippy (<os>)   (CI lints each OS natively; this cross-lints them locally)
#   lint-docs       Docs
#   lint-deny       Cargo deny
#
# The cross-platform `cargo test` matrix is `just test`, kept separate
# because it exercises behaviour rather than lints. `just check` runs both
# (`lint` then `test`) — the full local gate, and what the pre-push hook runs.
#
# CI runs clippy natively on each OS (the `Clippy (<os>)` matrix). `just`
# runs on one OS, so `lint-clippy` instead CROSS-compiles the non-host
# targets — the only way to catch a `#[cfg(windows)]` lint/compile error
# from this Mac/Linux box before pushing. Caveat: the macOS target compiles
# Objective-C (`mac-notification-sys`), so it can only be cross-linted from a
# macOS host; off a Mac, `lint-clippy` skips it and CI's macos-latest leg is
# the backstop. CI-only gates `just` cannot reproduce on one dev box: the
# per-OS test *behaviour* matrix (clippy proves the cfg code compiles and
# lints, not that it runs correctly), the MSRV (Rust 1.95) build, and
# coverage. A green `just check` is necessary, not sufficient — watch the PR
# checks after pushing.
#
# One-time tooling the lints assume:
#   rustup toolchain install nightly --component rustfmt                # lint-fmt
#   rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-gnu    # lint-clippy (host OS target already installed)
#   cargo install cargo-deny                                            # lint-deny

# List the available recipes.
default:
    @just --list

# Full local gate (lint + test) — what the pre-push hook runs; stops at first failure.
check: lint test

# Run every CI lint/quality gate locally, in CI's order (stops at first failure).
lint: lint-fmt lint-clippy lint-docs lint-deny

# Format check with nightly rustfmt (CI "Format (nightly)"; needs nightly + rustfmt).
lint-fmt:
    cargo +nightly fmt --all --check

# Clippy (warnings denied) cross-linting the OS targets locally (CI lints each OS natively; needs the rustup targets above).
lint-clippy:
    cargo clippy --workspace --all-targets --all-features --locked --target x86_64-unknown-linux-gnu -- -D warnings
    cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-gnu -- -D warnings
    {{ if os() == "macos" { "cargo clippy --workspace --all-targets --all-features --locked --target aarch64-apple-darwin -- -D warnings" } else { "echo 'lint-clippy: skipping aarch64-apple-darwin (compiles Objective-C; needs a macOS host - CI lints it on macos-latest)'" } }}

# Rustdoc with warnings denied — broken/private doc links fail (CI "Docs").
lint-docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# License/advisory/bans/sources policy (CI "Cargo deny"; needs cargo-deny).
lint-deny:
    cargo deny check

# Apply nightly rustfmt in place (the fix-it companion to lint-fmt).
fmt:
    cargo +nightly fmt --all

# Workspace test suite, as each CI matrix OS runs it.
test:
    cargo test --workspace --locked

# Build the release binary `patina`, plus the `patina-elevate` helper on Windows.
build:
    cargo build --release --locked -p patina-cli
    {{ if os() == "windows" { "cargo build --release --locked -p patina-elevate --features patina-elevate/windows" } else { "echo 'build: skipping patina-elevate (Windows-only Developer Mode UAC helper)'" } }}

# patina.exe resolves its UAC Developer Mode helper as a sibling executable, so on
# Windows patina-elevate.exe must land in the same dir; --force reinstalls the
# current working-tree build despite the unchanged 0.1.0 version.
# Install `patina` into the cargo bin path (and `patina-elevate` on Windows).
install:
    cargo install --path patina-cli --locked --force
    {{ if os() == "windows" { "cargo install --path patina-elevate --features windows --locked --force" } else { "echo 'install: skipping patina-elevate (Windows-only Developer Mode UAC helper)'" } }}
