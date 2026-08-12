---
paths: **/*.{rs,toml}
description: "Rust testing; insta snapshots, table and file-driven tests, invariant-checking helpers, trybuild, no_std verification, compile-time assertions, and nextest."
---

# Testing

## Centralize snapshot settings in one macro

Wrap every `insta` snapshot assertion in a single project macro so settings (redactions, `omit_expression`, filters) are applied consistently and set once.

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

Use per-test `filters` to normalize volatile substrings (absolute paths, timestamps) before comparison, so snapshots are stable across machines.

## Table-driven tests with `#[test_case]`

Turn a fixture table into one parameterized test instead of copy-pasted functions.

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

## File-driven tests with `datatest-stable`

For large corpora, make each fixture file on disk its own test case. Opt out of the default libtest harness.

```toml
[[test]]
name = "mdtest"
harness = false
```

```rust
datatest_stable::harness!(run_test, "resources/mdtest", r"^.*\.md$");
```

## Make the shared test helper assert invariants

A helper that every test funnels through should check invariants on each call, not just diff a snapshot. Then a bug in any code path fails the nearest test. Good invariants: applying a fix **converges** (a second application is a no-op), and a transformation introduces **no new syntax errors**.

## Compile-fail UI tests with `trybuild`

Pin the exact error message your macro or API produces for bad input. Commit the `.stderr` files. Messages vary by toolchain, so gate on nightly and skip under miri.

```rust
#[rustversion::attr(not(nightly), ignore = "requires nightly")]
#[cfg_attr(miri, ignore = "incompatible with miri")]
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
```

## Verify `no_std` with a real `no_std` crate

A `#[cfg]` alone won't catch an accidental `std::` path. Add a separate crate that is genuinely `#![no_std]` and depends on yours with `default-features = false`.

```toml
[dependencies]
my-crate = { path = "../..", default-features = false }
```

```rust
#![no_std]
use my_crate::Error; // fails to build if the crate leaks a std dependency
```

## Compile-time size and trait assertions

Lock the size and trait surface of hot types so a careless change fails at compile time.

```rust
use static_assertions::{assert_eq_size, assert_impl_all};

assert_eq_size!(NodeId, Option<NodeId>); // NonZeroU32 niche: Option is free
assert_impl_all!(NodeId: Ord, Send, Sync);
```

## Auto-trait and drop-count tests

Guard your public auto-trait surface: a stray `Rc` or raw pointer silently removing `Send`/`Sync` is a breaking change.

```rust
#[test]
fn auto_traits() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<Error>();
    assert_sync::<Error>();
}
```

For by-value ownership tricks (custom `Drop`, manual `unsafe` memory management), assert values are dropped exactly once with a drop-counting helper.

```rust
#[test]
fn drops_source_once() {
    let (err, dropped) = make_chain();
    assert!(dropped.none());
    drop(err);
    assert!(dropped.all());
}
```

## A release-profile test profile

Debug-only checks (e.g. bounds checks gated on `debug_assertions`) never run under `cargo test`. Add a profile that turns them off so CI exercises the real release paths.

```toml
[profile.testrelease]
inherits = "test"
debug-assertions = false
```

## `nextest`: serialize and bound flaky tests

Put race-prone tests in a serial group and cap deadlock-prone ones with a hard timeout.

```toml
# .config/nextest.toml
[test-groups]
serial = { max-threads = 1 }

[[profile.default.overrides]]
filter = 'binary(file_watching)'
test-group = 'serial'
slow-timeout = { period = "1s", terminate-after = 60 } # terminate on deadlock
```

## CI-enforce generated-code freshness

If you check in generated code, fail CI when regenerating it would produce a diff. Otherwise the checked-in copy silently rots.

```sh
cargo run -p my-cli -- generate
git diff --exit-code
```
