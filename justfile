# Patina developer task runner. `just` or `just --list` shows all recipes.
#
# `just lint` runs every linting / quality gate the CI workflow enforces
# (.github/workflows/ci.yml), in the same order, with the same commands, so a
# green `just lint` locally means those CI jobs will pass:
#
#   just recipe     CI job
#   ------------    --------------------
#   lint-fmt        Format (nightly)
#   lint-clippy     Clippy
#   lint-docs       Docs
#   lint-deny       Cargo deny
#
# The cross-platform `cargo test` matrix is `just test`, kept separate
# because it exercises behaviour rather than lints. `just check` runs both
# (`lint` then `test`) — the full local gate, and what the pre-push hook runs.
#
# CI-only gates `just` cannot reproduce on one dev box: the Windows/macOS
# test matrix, the MSRV (Rust 1.95) build, and coverage. A green `just check`
# is necessary, not sufficient — watch the PR checks after pushing.
#
# One-time tooling the lints assume:
#   rustup toolchain install nightly --component rustfmt   # lint-fmt
#   cargo install cargo-deny                               # lint-deny

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

# Clippy with all warnings denied (CI "Clippy").
lint-clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

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
