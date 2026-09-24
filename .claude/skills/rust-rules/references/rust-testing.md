---
paths: **/*.{rs,toml}
description: "Rust testing; insta snapshots, table and file-driven tests, invariant-checking helpers, trybuild, no_std verification, compile-time assertions, and nextest."
---

# Testing

For async code, apply the
[deterministic async checks](rust-async.md#verify-async-contracts-deterministically-required) and
[Tokio virtual-time guidance](rust-async.md#use-virtual-time-for-timer-logic-default).

## Keep test failures diagnostic (Required)

Use unit-returning test functions and descriptive `expect` messages for fallible setup so a setup
failure reports its expectation and source location. Apply the
[test lint configuration](rust-lints-and-formatting.md#configure-clippy-and-test-allowances-required)
and keep other baseline restrictions active. Put test-only helpers that need the configured
allowances in `#[cfg(test)]` modules; keep other shared helpers fallible.

```rust
#[test]
fn parses_decimal_port() {
    let port = "8080".parse::<u16>().expect("fixture contains a valid port");
    assert_eq!(port, 8080);
}
```

When an error variant or payload is part of the contract, assert it explicitly. Use `is_ok()` or
`is_err()` when only success or failure matters, and include the result in the failure message.

## Group integration tests when binary overhead matters (Conditional)

When many integration-test binaries add material linking or execution overhead, group related tests
as modules under one integration-test entry point, such as `tests/integration/main.rs`. Keep
separate binaries when tests require different harnesses, target configuration, or process
isolation. See
[Cargo's integration-test layout guidance](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#integration-tests).

## Centralize snapshot settings in one macro (Default)

Default `insta` assertions to one project macro when snapshots share settings such as redactions,
`omit_expression`, or filters. Direct assertions remain appropriate when no project setting applies.

```rust
#[macro_export]
macro_rules! assert_diagnostics {
    ($value:expr) => {{
        insta::with_settings!({ omit_expression => true }, {
            insta::assert_snapshot!($crate::test::print_messages(&$value));
        });
    }};
}
```

For tests with volatile substrings such as absolute paths or timestamps, apply per-test filters
before comparison.

## Table-driven tests with `#[test_case]` (Default)

When cases share one assertion path, default to a parameterized test instead of copied test
functions. Keep separate tests when their setup or failure contracts differ.

```rust
#[cfg(test)]
mod tests {
    use test_case::test_case;

    #[test_case("80", 80)]
    #[test_case("443", 443)]
    fn parses_port(text: &str, expected: u16) {
        let port = text.parse::<u16>().expect("fixture contains a valid port");
        assert_eq!(port, expected);
    }
}
```

## Property tests for invariants (Conditional)

When parsers, serializers, or numeric operations have properties that hold over many inputs, use
`proptest` to exercise those properties and shrink failures. Assert an independent invariant, such
as a serialization round trip, rather than reproducing the implementation's algorithm. Keep explicit
examples for known boundaries and error contracts.

Commit the generated `proptest-regressions` files so discovered failures replay in later runs. See
[Proptest failure persistence](https://proptest-rs.github.io/proptest/proptest/failure-persistence.html).

## File-driven tests with `datatest-stable` (Conditional)

For large corpora, make each fixture file on disk its own test case. Opt out of the default libtest
harness.

```toml
[[test]]
name = "mdtest"
harness = false
```

```rust
datatest_stable::harness!(run_test, "resources/mdtest", r"^.*\.md$");
```

## Make the shared test helper assert invariants (Default)

Default a shared test helper to checking common invariants on each call instead of only diffing a
snapshot. Each invariant failure then identifies the calling test. Useful invariants include
**convergence** and preservation of syntax validity during a transformation.

## Compile-fail UI tests with `trybuild` (Conditional)

When a macro or API has misuse that must fail to compile with a useful diagnostic, use `trybuild`
and commit the `.stderr` files. Keep the `rust-src` component consistent across local development
and CI because its presence changes standard-library snippets in diagnostics.

```rust
#[cfg_attr(miri, ignore = "incompatible with miri")]
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
```

Add `rust-src` to the components in the
[committed toolchain file](rust-lints-and-formatting.md#commit-the-complete-nightly-rustfmt-configuration-required)
and to the stable installation command in CI.

When a project treats exact diagnostic text as a compatibility contract, pin a stable release for
that UI-test suite and run it in a separate shared local and CI task. Exclude that suite from the
floating-stable test task and keep other checks on the baseline toolchains. When expected
diagnostics change after a deliberate toolchain update, run the suite's test task with
`TRYBUILD=overwrite`, inspect the diff, and commit the accepted output. Trybuild does not require
nightly; see its
[workflow and troubleshooting guidance](https://github.com/dtolnay/trybuild#workflow).

## Verify `no_std` support on a target without `std` (Required)

When a library promises `no_std` support, check the library and its dependencies on a supported
target without the standard library. Disable default features and check each promised feature
combination separately. For a library supporting Cortex-M4 bare-metal targets:

```sh
rustup target add --toolchain stable thumbv7em-none-eabi
cargo +stable check -p my-crate --lib --no-default-features --target thumbv7em-none-eabi --locked
```

Choose the target from the library's supported platforms. If the library exposes an `alloc` tier,
also check it with `--features alloc`. Run the same target and feature checks locally and in CI. A
host-built `#![no_std]` consumer can still link `std` through dependencies; use the target check to
verify the portability claim. See the
[Rust Reference on `no_std`](https://doc.rust-lang.org/reference/names/preludes.html#the-no_std-attribute).

## Check released library APIs (Conditional)

When a released library promises compatibility, run `cargo-semver-checks` against the last release
in the supported release line. Select the baseline explicitly for reproducible checks; for example:

```sh
cargo +stable semver-checks --package my-crate --baseline-version 1.2.3
```

Use `--baseline-rev` for a release Git revision when a registry version is unavailable. Check the
features and targets covered by the compatibility promise. A passing scan does not establish full
compatibility; review behavioral contracts separately. Resolve reported incompatibilities or make
the required breaking release. See
[baseline selection and coverage limits](https://github.com/obi1kenobi/cargo-semver-checks#readme).

## Check supported feature configurations (Conditional)

When a crate exposes feature choices, use `cargo-hack` to check supported configurations without
development dependencies. For a crate whose features each work independently:

```sh
cargo +stable hack check --package my-crate --each-feature --no-dev-deps --locked
```

Include default and no-default-feature builds when supported. For interacting features, use
`--feature-powerset` or an explicit supported matrix; individual-feature checks do not cover every
combination. Exclude unsupported combinations explicitly and retain behavior tests for supported
combinations. Avoid concurrent manifest edits while `--no-dev-deps` temporarily removes and restores
development dependencies. See
[cargo-hack's feature checks](https://github.com/taiki-e/cargo-hack#--each-feature).

For both compatibility and feature checks, install the applicable tool with `cargo +stable install
--locked` and add a task shared by local development and CI. Follow the
[shared task policy](rust-lints-and-formatting.md#share-local-fix-lint-and-ci-tasks-required).

## Compile-time size and trait assertions (Conditional)

When a type is hot or ABI-critical, assert its required size and trait bounds at compile time. Use a
const assertion for size constraints:

```rust
use std::mem::size_of;

const _: () = assert!(size_of::<NodeId>() == size_of::<Option<NodeId>>());
```

Use generic bounds for positive trait assertions, as in the
[auto-trait tests](#auto-trait-and-drop-count-tests-default). Add an assertion crate only when the
required check needs capabilities these forms do not provide.

## Auto-trait and drop-count tests (Default)

For public libraries, default to compile-time tests for the intended auto-trait surface. A stray
`Rc` or raw pointer can remove `Send` or `Sync` and break callers.

```rust
#[test]
fn auto_traits() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<Error>();
    assert_sync::<Error>();
}
```

When a type uses custom `Drop` or manual unsafe ownership, add a drop-counting test that asserts
each value is dropped exactly once.

```rust
#[test]
fn drops_source_once() {
    let (err, dropped) = make_chain();
    assert!(dropped.none());
    drop(err);
    assert!(dropped.all());
}
```

## A release-profile test profile (Conditional)

When behavior changes under `debug_assertions`, add a test profile that disables them so CI
exercises the release path.

```toml
[profile.testrelease]
inherits = "test"
debug-assertions = false
```

## `nextest`: serialize and bound flaky tests (Conditional)

When tests contend for a shared resource or can deadlock, use nextest groups and timeouts to
serialize or bound them.

```toml
[test-groups]
serial = { max-threads = 1 }

[[profile.default.overrides]]
filter = 'binary(file_watching)'
test-group = 'serial'
slow-timeout = { period = "1s", terminate-after = 60 }
```

## CI-enforce generated-code freshness (Required)

If you check in generated code, fail CI when regenerating it would produce a diff; otherwise the
checked-in copy can become stale.

```sh
cargo run -p my-cli -- generate
git diff --exit-code
```
