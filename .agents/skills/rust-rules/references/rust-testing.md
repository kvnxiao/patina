---
paths: **/*.{rs,toml}
description: "Rust testing; insta snapshots, table and file-driven tests, invariant-checking helpers, trybuild, no_std verification, compile-time assertions, and nextest."
---

# Testing

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
use test_case::test_case;

#[test_case(Rule::NoSlotsInStrSubclass, Path::new("SLOT000.py"))]
#[test_case(Rule::NoSlotsInTupleSubclass, Path::new("SLOT001.py"))]
fn rules(rule: Rule, path: &Path) -> Result<()> {
    let diagnostics = test_path(path, &settings::for_rule(rule))?;
    assert_diagnostics!(format!("{}_{}", rule.noqa_code(), path.display()), diagnostics);
    Ok(())
}
```

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

```toml
[toolchain]
components = ["rust-src"]
```

When expected diagnostics drift after a deliberate toolchain update, run `TRYBUILD=overwrite cargo
test`, inspect the diff, and commit the accepted output. When a project treats exact diagnostic text
as a compatibility contract, pin the Rust toolchain and update it through a reviewed maintenance
process. Trybuild does not require nightly; see its
[workflow and troubleshooting guidance](https://github.com/dtolnay/trybuild#workflow).

## Verify `no_std` with a real `no_std` crate (Required)

A `#[cfg]` alone won't catch an accidental `std::` path. Add a separate crate that is genuinely
`#![no_std]` and depends on yours with `default-features = false`.

```toml
[dependencies]
my-crate = { path = "../..", default-features = false }
```

```rust
#![no_std]
use my_crate::Error;
```

## Compile-time size and trait assertions (Conditional)

When a type is hot or ABI-critical, lock its size and required trait surface at compile time.

```rust
use static_assertions::{assert_eq_size, assert_impl_all};

assert_eq_size!(NodeId, Option<NodeId>);
assert_impl_all!(NodeId: Ord, Send, Sync);
```

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
