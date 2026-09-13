---
paths: **/*.{rs,toml}
description: "Rust performance guidance activated by profiling; borrowing, allocation capacity, hashing, arenas, compact collections, byte-oriented IO, and measured build profiles."
---

# Performance Considerations

The default representation is a readable standard-library type. Profiling or benchmarks activate
specialized data structures, storage formats, and build profiles.

## Use `Cow` for conditional cloning (Conditional)

When profiling identifies cloning as material and most inputs can remain borrowed, return `Cow`.
Keep an owned return type when the lifetime parameter and two representations would complicate the
API without a meaningful gain.

```rust
use std::borrow::Cow;

fn process(input: &str) -> Cow<'_, str> {
    if input.contains("special") {
        Cow::Owned(input.replace("special", "SPECIAL"))
    } else {
        Cow::Borrowed(input)
    }
}
```

## Allocations and capacity (Default)

When the output size is known or has a reliable upper bound, reserve that capacity before repeated
insertion.

```rust
let mut values = Vec::with_capacity(1000);
for value in 0..1000 {
    values.push(value);
}
```

## Borrow or consume collections intentionally (Default)

Borrow a collection when it remains in use, and consume it with `into_iter()` when ownership can
move to the loop.

```rust
for item in &collection {
    process(item);
}

for item in collection.into_iter() {
    consume(item);
}
```

Use `drain(..)` when the empty allocation will be reused or when only a subrange must be removed.
Default full-range consumption to `into_iter()`; `clippy::iter_with_drain` identifies a full-range
drain used only for consumption.

## Fast hashing for internal maps (Conditional)

When profiling identifies hashing in a trusted internal map as a hot path, benchmark a faster hasher
such as `FxHash`. Maps keyed by untrusted input must retain a HashDoS-resistant hasher.

```rust
use rustc_hash::FxHasher;
use std::{collections::HashMap, hash::BuildHasherDefault};

type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
```

## Arena indices instead of pointer graphs (Conditional)

When allocation and pointer traversal dominate a graph workload, benchmark copyable indices into a
flat arena against the standard pointer representation.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(std::num::NonZeroU32);

let nodes: IndexVec<NodeId, Node> = IndexVec::new();
```

## Compact collections on hot paths (Conditional)

When measurements show that allocation count or representation size matters, benchmark inline
strings and small-vector representations using observed size distributions.

```rust
pub struct Name(compact_str::CompactString);

type Children<T> = thin_vec::ThinVec<T>;

type Assertions = smallvec::SmallVec<[Assertion; 1]>;
```

## Shrink before caching and box immutable payloads (Conditional)

When a long-lived cache has measured memory pressure, remove spare collection capacity before
insertion or store immutable payloads as boxed slices and strings.

```rust
vec.shrink_to_fit();

let payload: Box<[u8]> = data.into_boxed_slice();
let name: Box<str> = value.into_boxed_str();
```

## Byte strings for hot IO (Conditional)

When profiling identifies UTF-8 validation or text conversion on a hot IO path, operate on `&[u8]`
or `bstr` values and validate only at text boundaries. Name buffer capacities so benchmarks can vary
them deliberately.

```rust
use bstr::ByteSlice;

const DEFAULT_BUFFER_CAPACITY: usize = 64 * (1 << 10);
```

## Release debug information (Default)

Line-table debug information remains in release builds for profilers and backtraces unless artifact
size measurements require stripping it.

```toml
[profile.release]
debug = 1
```

## Specialized build profiles (Conditional)

When benchmarks justify longer builds or different runtime semantics, add LTO, per-package codegen
settings, profiling profiles, or custom release profiles. Compare the produced binary on
representative workloads before retaining the configuration.

```toml
[profile.release-lto]
inherits = "release"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
debug-assertions = false
overflow-checks = false

[profile.release.package.parser]
codegen-units = 1

[profile.profiling]
inherits = "release"
debug = "full"
strip = false
lto = false
```

With `overflow-checks = false`, integer overflow wraps with two's-complement semantics. It is not
undefined behavior, but code that requires overflow rejection must use checked arithmetic instead of
relying on the profile.
