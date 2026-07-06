---
paths: **/*.{rs,toml}
description: "Public API design for libraries; ergonomic, semver-evolvable interfaces via options structs, sealed traits, non_exhaustive, features and no_std, unsafe and macro hygiene."
---

# API Design

Patterns for public interfaces that stay ergonomic for callers and evolvable for you.

## Options struct + `impl Into` for overload-like ergonomics

Rust has no function overloading. Accept `impl Into<Options>` and provide a family of `From` impls: the simple call passes a bare value, richer calls pass a tuple or the full struct. No builder ceremony for the common case, and one obvious home for optional params.

```rust
pub struct RoundOptions { smallest: Unit, increment: i64 }

impl From<Unit> for RoundOptions {
    fn from(smallest: Unit) -> Self { Self { smallest, increment: 1 } }
}
impl From<(Unit, i64)> for RoundOptions {
    fn from((smallest, increment): (Unit, i64)) -> Self { Self { smallest, increment } }
}

impl Span {
    // span.round(Unit::Hour) | span.round((Unit::Minute, 15)) | span.round(RoundOptions { .. })
    pub fn round<R: Into<RoundOptions>>(self, options: R) -> Result<Span> {
        let options = options.into();
        // ...
    }
}
```

## Deferred-validation builder

Validating inside each setter makes some valid end states unreachable through valid intermediate states.

```rust
// Bad: `day(29)` validates against the current month. Setting the day before
// the month rejects Feb 29 even when you're about to switch to a leap year.
date.with().day(29).month(2).build()  // spurious error

// Good: setters only store; `build()` validates the whole thing once.
date.with()
    .month(2)
    .day(29)   // order-independent, not checked yet
    .build()?  // single validation point
```

## Don't derive `Eq`/`Ord`/`PartialEq` reflexively

A derived `PartialEq` compares field-by-field. That is often wrong: two values can be semantically equal with different representations.

```rust
// Bad: derived PartialEq makes 2.hours() != 120.minutes() despite equal duration.

// Good, option 1 — hand-write over the meaningful field:
impl PartialEq for Zoned {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp() == other.timestamp() // compare the instant, ignore the zone
    }
}

// Good, option 2 — withhold equality when "equal" is ambiguous, and expose an
// explicit opt-in newtype for the field-wise comparison:
#[repr(transparent)]
pub struct SpanFieldwise(pub Span); // via `span.fieldwise()`, only when asked for
```

## `#[non_exhaustive]` on config enums expected to grow

```rust
/// Non-exhaustive so new strategies can be added in a semver-compatible release.
#[non_exhaustive]
pub enum Disambiguation {
    Compatible,
    Earlier,
    Later,
    Reject,
}
```

## Paired panicking / fallible constructors

Give literals a terse panicking constructor and untrusted input a fallible one. A `const {}` block moves the literal's panic to compile time.

```rust
let d = date(2024, 2, 29);          // panicking: for author-known-good literals
let d = Date::new(year, month, day)?; // fallible: for runtime / user input

const NEW_YEAR: Date = const { date(2025, 1, 1) }; // invalid literal fails to compile
```

## Extension traits for literal ergonomics

An extension trait can add literal syntax to primitives. Document it as literals-only and panicking, and pair it with `try_*` methods for user input.

```rust
use jiff::ToSpan;

let span = 2.hours().minutes(30); // ergonomic literals; panics if out of range
let span = n.try_hours()?;        // for untrusted input
```

## Sealed traits

A trait bounded on a `pub(crate)` `Sealed` supertrait is public to *call* but closed to *implement*. You can add methods later without a major bump, and no downstream type can implement it.

```rust
pub trait Context<T>: private::Sealed {
    fn context<C: Display + Send + Sync + 'static>(self, cx: C) -> Result<T>;
}

mod private {
    pub trait Sealed {}
    impl<T, E: std::error::Error> Sealed for Result<T, E> {}
}
```

## Hide macro glue behind `#[doc(hidden)]`

Code your macros generate needs public items to call, but humans should not. Put them in a `#[doc(hidden)] pub mod __private`; they are not part of your semver surface.

```rust
#[doc(hidden)]
pub mod __private {
    pub use core::result::Result;
    // re-exports the generated code depends on ...
}
```

## Private modules, one curated `pub use`

Decouple your file layout from your public path: keep modules private and export a single curated block. Rename or move files without touching the public API.

```rust
mod error;
mod span;
pub mod civil; // only genuinely-public modules are `pub`

pub use crate::{
    error::Error,
    span::{Span, SpanRound, Unit},
};
```

## Lossless → `From`, lossy → `TryFrom`

A conversion that can overflow or lose data must be fallible. Never hide truncation behind an infallible `From`.

```rust
impl From<i32> for Duration { /* widening: always succeeds */ }

impl TryFrom<std::time::Duration> for SignedDuration {
    type Error = Error;
    fn try_from(d: std::time::Duration) -> Result<Self> {
        let secs = i64::try_from(d.as_secs())?; // may not fit
        // ...
    }
}
```

## Library authoring: features and `no_std`

Applies when you publish a library.

```rust
// Toggle `no_std` behind a feature; pull in `alloc` where available.
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

# Removed feature kept as a documented no-op so `features = ["backtrace"]` still builds.
backtrace = []

# Footgun documented inline: does not preserve identity — enable only if intended.
rc = []
```

- **Features must be additive.** Enabling one only adds behavior, never changes it. Downstream crates share your feature resolution.
- **Document what degrades** when a feature is off, and what a footgun feature does.
- **Don't forward target-sensitive features** from a library. Let the final binary opt in (e.g. a `js` feature that only makes sense on `wasm32-unknown-unknown`).
- **Centralize `#[cfg]`** in one module as type aliases + macros, so the rest of the crate stays cfg-free.
- **Probe unstable APIs in `build.rs`** and emit `println!("cargo:rustc-check-cfg=cfg(...)")`, rather than leaking a nightly feature to users.

## `unsafe` discipline

Forbid `unsafe` by default. If a crate must relax that baseline, keep the discipline; `unsafe_code = "warn"` is a middle ground.

```rust
#![deny(unsafe_op_in_unsafe_fn)]

// Every unsafe block states the invariant it relies on.
// Safety: `ptr` is non-null and points to an initialized `T` (checked above).
let value = unsafe { &*ptr };
```

Lock the public auto-trait surface with `assert_send::<T>()`-style tests, and add drop-count tests for by-value ownership tricks.

## Macro-author hygiene

Generated code runs in the caller's namespace, so it must be self-contained.

```rust
// Inside a derive macro's `quote!`:
quote! {
    #[automatically_derived]
    #[allow(unused_qualifications)]
    impl #generics ::core::fmt::Display for #ty { /* ... */ }
}
```

- Fully-qualify every path (`::core::`, `::std::`, `::your_crate::`) so it works regardless of the caller's `use`s.
- Emit `#[automatically_derived]` on generated impls.
- Add targeted `#[allow(...)]` for lints your codegen cannot avoid, and test the output under `#![deny(...)]`.
