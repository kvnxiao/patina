---
paths: **/*.{rs,toml}
description: "Public API design for libraries; ergonomic, semver-evolvable interfaces via options structs, sealed traits, non_exhaustive, features and no_std, unsafe and macro hygiene."
---

# API Design

These patterns keep public interfaces ergonomic for callers and compatible with later evolution.

## Options struct + `impl Into` for overload-like ergonomics (Conditional)

When a public API benefits from overload-like call ergonomics, accept `impl Into<Options>` and
provide a small family of `From` implementations. The simple call can pass a bare value, while
richer calls pass a tuple or the full struct.

```rust
pub struct RoundOptions { smallest: Unit, increment: i64 }

impl From<Unit> for RoundOptions {
    fn from(smallest: Unit) -> Self { Self { smallest, increment: 1 } }
}
impl From<(Unit, i64)> for RoundOptions {
    fn from((smallest, increment): (Unit, i64)) -> Self { Self { smallest, increment } }
}

impl Span {
    pub fn round<R: Into<RoundOptions>>(self, options: R) -> Result<Span> {
        let options = options.into();
        todo!()
    }
}
```

## Deferred-validation builder (Required)

When builder fields interact, setters must store values and `build()` must validate the complete
state. Per-setter validation can reject a valid final state because an intermediate state is
incomplete.

An order-sensitive builder can reject this call before the month changes:

```rust
date.with().day(29).month(2).build()
```

Deferred validation accepts any setter order and validates once:

```rust
date.with()
    .month(2)
    .day(29)
    .build()?
```

## Derive equality and ordering from semantics (Default)

A derived `PartialEq` compares field by field. Default to a semantic comparison or omit equality
when values can be equivalent despite different representations. Derive equality when structural
equality is the intended contract.

```rust
impl PartialEq for Zoned {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp() == other.timestamp()
    }
}

#[repr(transparent)]
pub struct SpanFieldwise(pub Span);
```

## `#[non_exhaustive]` on config enums expected to grow (Conditional)

When a public configuration enum is expected to gain variants, mark it `#[non_exhaustive]` to permit
additions in compatible releases.

```rust
/// Select how an ambiguous local time is resolved.
#[non_exhaustive]
pub enum Disambiguation {
    Compatible,
    Earlier,
    Later,
    Reject,
}
```

## Paired panicking / fallible constructors (Default)

Default author-controlled literals to a terse panicking constructor and untrusted input to a
fallible constructor. A `const {}` block moves a literal panic to compile time.

```rust
let literal = date(2024, 2, 29);
let parsed = Date::new(year, month, day)?;

const NEW_YEAR: Date = const { date(2025, 1, 1) };
```

## Extension traits for literal ergonomics (Conditional)

When an API has many author-controlled literals, an extension trait can add literal syntax to
primitives. Document the methods as literals-only and panicking, and pair them with `try_*` methods
for user input.

```rust
use jiff::ToSpan;

let literal = 2.hours().minutes(30);
let parsed = n.try_hours()?;
```

## Sealed traits (Conditional)

When a public trait must gain methods without a major-version bump, bound it on a private `Sealed`
supertrait. Downstream code can call the trait but cannot implement it.

```rust
pub trait Context<T>: private::Sealed {
    fn context<C: Display + Send + Sync + 'static>(self, cx: C) -> Result<T>;
}

mod private {
    pub trait Sealed {}
    impl<T, E: std::error::Error> Sealed for Result<T, E> {}
}
```

## Hide macro glue behind `#[doc(hidden)]` (Default)

Generated macro code can need public items for expansion. Default those items to a `#[doc(hidden)]
pub mod __private` unless callers are expected to use them directly.

The attribute does not make an item private: downstream code can still name and call it. Rust
convention treats hidden items as unsupported, and `cargo-semver-checks` excludes them from the
SemVer surface by default. See
[Checking semver for doc(hidden) items](https://predr.ag/blog/checking-semver-for-doc-hidden-items/).

```rust
#[doc(hidden)]
pub mod __private {
    pub use core::result::Result;
}
```

## Private modules, one curated `pub use` (Default)

Default modules to private and export a curated `pub use` block unless the module path is part of
the intended public API. This separates file layout from public paths.

```rust
mod error;
mod span;
pub mod civil;

pub use crate::{
    error::Error,
    span::{Span, SpanRound, Unit},
};
```

## Lossless → `From`, lossy → `TryFrom` (Required)

A conversion that can overflow or lose data must be fallible. Never hide truncation behind an
infallible `From`.

```rust
let widened = i64::from(seconds);
let narrowed = i32::try_from(seconds)?;

impl TryFrom<std::time::Duration> for SignedDuration {
    type Error = Error;
    fn try_from(d: std::time::Duration) -> Result<Self> {
        let secs = i64::try_from(d.as_secs())?;
        todo!()
    }
}
```

## Features must remain additive (Required)

Published library features must only add behavior. A feature must not change existing behavior
because downstream crates share feature resolution.

## Library features and `no_std` conventions (Default)

When publishing a library, default to explicit capability tiers and document what changes when a
feature is disabled.

```rust
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;
```

```toml
[features]
default = ["std"]
std = ["alloc"]        # tier features: std ⊃ alloc ⊃ core
alloc = []
derive = ["dep:my_derive"] # optional proc-macro, off by default

# Removed feature retained as a no-op until the next major version.
backtrace = []

# Does not preserve identity; enable only when that behavior is acceptable.
rc = []
```

- Feature documentation states what degrades when a feature is off and what semantic trade-offs an
  opt-in feature adds.
- Target-sensitive features remain under the final binary's control unless the library contract
  itself requires them.
- Conditional type selection stays in one module unless local `#[cfg]` attributes are clearer.
- When a stable API cannot express a capability check, `build.rs` probes it and emits
  `println!("cargo:rustc-check-cfg=cfg(...)")`.

## `unsafe` soundness obligations (Required)

Each `unsafe` block must state its safety invariant, and each `unsafe fn` must document caller
obligations in a `# Safety` section. Enable `unsafe_op_in_unsafe_fn`, keep unsafe blocks minimal,
and wrap raw unsafe operations behind safe public abstractions.

```rust
#![deny(unsafe_op_in_unsafe_fn)]

// Safety: `ptr` is non-null and points to an initialized `T`.
let value = unsafe { &*ptr };
```

## `unsafe` project policy (Default)

Default `unsafe_code` to `forbid`. When a crate needs unsafe implementation code, `unsafe_code =
"warn"` permits reviewed exceptions.

Prefer a maintained safe wrapper when one covers the required API:

| Domain      | Unsafe Bindings  | Safe Wrapper           |
| ----------- | ---------------- | ---------------------- |
| Windows API | `windows-sys`    | `winsafe`              |
| POSIX/Unix  | `libc`           | `nix`, `rustix`        |
| SQLite      | `libsqlite3-sys` | `rusqlite`             |
| OpenSSL     | `openssl-sys`    | `openssl`              |
| Memory      | raw pointers     | `bytemuck`, `zerocopy` |

Crates containing `unsafe` default to `cargo +nightly miri test`; projects without nightly test
support can omit this job.

For a public library, default to `assert_send::<T>()`-style tests for the intended auto-trait
surface. When code uses by-value ownership tricks, add drop-count tests.

## Macro-author hygiene (Required)

Generated code runs in the caller's namespace, so it must be self-contained.

```rust
quote! {
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl #generics ::core::fmt::Display for #ty {
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            todo!()
        }
    }
}
```

- Fully-qualify every path (`::core::`, `::std::`, `::your_crate::`) so it works regardless of the
  caller's `use`s.
- Emit `#[automatically_derived]` on generated impls.
- Add targeted `#[allow(...)]` for lints your codegen cannot avoid, and test the output under
  `#![deny(...)]`.
