---
paths: **/*.{rs,toml}
description: "Public API design for libraries; ergonomic, semver-evolvable interfaces via options structs, sealed traits, non_exhaustive, features and no_std, unsafe and macro hygiene."
---

# API Design

These patterns keep public interfaces ergonomic for callers and compatible with later evolution.

For async interfaces, apply the
[public future contracts](rust-async.md#establish-public-future-contracts-required) to runtime
requirements, auto-trait bounds, and supported callers.

## Options struct + `impl Into` for overload-like ergonomics (Conditional)

When a public API benefits from overload-like call ergonomics, accept `impl Into<Options>` and
provide a small family of `From` implementations. The simple call can pass a bare value, while
richer calls pass the full struct with named fields.

```rust
/// Configure the unit and step used when rounding a span.
pub struct RoundOptions {
    /// Select the smallest retained unit.
    pub smallest: Unit,
    /// Select the number of units in each rounding step.
    pub increment: i64,
}

impl From<Unit> for RoundOptions {
    fn from(smallest: Unit) -> Self { Self { smallest, increment: 1 } }
}

impl Span {
    pub fn round<R: Into<RoundOptions>>(self, options: R) -> Result<Span> {
        let options = options.into();
        self.round_to(options.smallest, options.increment)
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

When implementing `Hash`, ensure equal values hash equally. When implementing ordering traits, keep
`PartialOrd`, `Ord`, and equality consistent. Derive these traits only when fieldwise behavior
matches all implemented contracts.

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

## Use fallible runtime constructors (Required)

Use fallible constructors for runtime validation, including author-controlled literals. Permit
invalid-literal rejection during forced compile-time evaluation. A `const fn` can also run at
runtime; use a const item or an explicit `const {}` block to force evaluation. Obtain the user's
approval before introducing a deliberate runtime panic API, then document its panic conditions and
provide a fallible alternative.

```rust
let literal = Date::new(2024, 2, 29)?;
let parsed = Date::new(year, month, day)?;

const NEW_YEAR: Date = date(2025, 1, 1);
let anniversary = const { date(2025, 3, 14) };
```

## Extension traits for literal ergonomics (Conditional)

When repeated unit conversions benefit from an extension trait, use fallible methods for values that
can exceed the representable range. Apply the runtime constructor policy to literal syntax as well.

```rust
use jiff::ToSpan;

let duration = n.try_hours()?;
```

## Sealed traits (Conditional)

When a public trait must gain methods without a major-version bump, bound it on a private `Sealed`
supertrait. Downstream code can call the trait but cannot implement it.

```rust
pub trait Context<T>: private::Sealed {
    fn context<C: Display + Send + Sync + 'static>(self, cx: C) -> Result<T>;
}

mod private {
    #[expect(unnameable_types, reason = "only this crate may implement the public trait")]
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

## Private modules, curated re-exports (Default)

Default modules to private and export curated items unless the module path is part of the intended
public API. This separates file layout from public paths.

```rust
mod error;
mod span;
pub mod civil;

pub use crate::error::Error;
pub use crate::span::Span;
pub use crate::span::SpanRound;
pub use crate::span::Unit;
```

## Use `From` for lossless conversions and `TryFrom` for checked conversions (Required)

Use `From` for lossless conversions and `TryFrom` when the conversion must reject out-of-range or
unrepresentable values. Never hide truncation behind an infallible `From`. For intentionally
approximate integer-to-float conversions, use an explicit cast and follow the numerical contract;
require an exactness check only when the domain needs exact representation.

```rust
let widened = i64::from(seconds);
let narrowed = i32::try_from(seconds)?;

impl TryFrom<std::time::Duration> for SignedDuration {
    type Error = Error;
    fn try_from(d: std::time::Duration) -> Result<Self> {
        let secs = i64::try_from(d.as_secs())?;
        Ok(Self { secs, nanos: d.subsec_nanos() })
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
  `println!("cargo:rustc-check-cfg=cfg(...)")`. Scope the printing expectation to the function
  emitting Cargo directives under the
  [output exception](rust-lints-and-formatting.md#limit-exceptions-to-their-approved-scope-required).

## `unsafe` soundness obligations (Required)

Each `unsafe` block must state its safety invariant, and each `unsafe fn` must document caller
obligations in a `# Safety` section. Enable `unsafe_op_in_unsafe_fn`, keep unsafe blocks minimal,
and wrap raw unsafe operations behind safe public abstractions.

For an approved FFI boundary with `unsafe_code = "deny"`, state the caller's obligations for the
entire operation:

```rust
/// Read a length supplied by an FFI caller.
///
/// # Safety
///
/// `ptr` must be non-null, aligned, and valid for reading one initialized `u32`.
/// The allocation must remain live and its contents must not change during this call.
#[expect(unsafe_code, reason = "FFI callers provide storage under the documented contract")]
pub unsafe fn read_length(ptr: *const u32) -> u32 {
    // SAFETY: The caller guarantees live, aligned, initialized storage without concurrent mutation.
    unsafe { ptr.read() }
}
```

When an operation creates a reference from a raw pointer, require valid storage and compliance with
aliasing rules for the reference's full lifetime. See
[pointer-to-reference requirements](https://doc.rust-lang.org/std/ptr/index.html#pointer-to-reference-conversion).

## `unsafe` project policy (Required)

Set `unsafe_code = "forbid"` by default. Obtain the user's approval before changing the applicable
crate or workspace policy to `deny` for unsafe implementation. Limit `#[expect(unsafe_code, reason =
"...")]` to the approved implementation and keep all other baseline lints active. A local
expectation cannot override `forbid`; follow the
[lint inheritance and exception policy](rust-lints-and-formatting.md#limit-exceptions-to-their-approved-scope-required).

Prefer a maintained safe wrapper when one covers the required API:

| Domain      | Unsafe Bindings  | Safe Wrapper           |
| ----------- | ---------------- | ---------------------- |
| Windows API | `windows-sys`    | `winsafe`              |
| POSIX/Unix  | `libc`           | `nix`, `rustix`        |
| SQLite      | `libsqlite3-sys` | `rusqlite`             |
| OpenSSL     | `openssl-sys`    | `openssl`              |
| Memory      | raw pointers     | `bytemuck`, `zerocopy` |

For approved unsafe implementation, select verification for the safety contract, including
ownership, aliasing, and drop behavior. When Miri can exercise the implementation, propose it as an
additional project-specific nightly check; keep the baseline build, tests, and Clippy on stable.

For a public library, default to `assert_send::<T>()`-style tests for the intended auto-trait
surface. When code uses by-value ownership tricks, add drop-count tests.

## Macro-author hygiene (Required)

Generated code runs in the caller's namespace, so it must be self-contained.

```rust
quote! {
    #[automatically_derived]
    impl #generics ::core::fmt::Display for #ty {
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            ::core::fmt::Display::fmt(&self.value, f)
        }
    }
}
```

- Fully-qualify every path (`::core::`, `::std::`, `::your_crate::`) so it works regardless of the
  caller's `use`s.
- Emit `#[automatically_derived]` on generated impls.
- Test generated output under the lint baseline. When generated code needs a suppression outside the
  predefined exceptions, obtain approval and scope it to the affected generated item.
