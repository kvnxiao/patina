---
paths: **/*.{rs,toml}
description: "Rust performance; Cow, capacity pre-allocation, FxHash, arena indices, compact collections, byte-string IO, and Cargo build profiles."
---

# Performance Considerations

## Use `Cow` for Conditional Cloning

```rust
use std::borrow::Cow;

fn process(input: &str) -> Cow<str> {
    if input.contains("special") {
        Cow::Owned(input.replace("special", "SPECIAL"))
    } else {
        Cow::Borrowed(input)
    }
}
```

## Allocations and Capacity

```rust
// Bad: Multiple reallocations
let mut vec = Vec::new();
for i in 0..1000 {
    vec.push(i);
}

// Good: Pre-allocate when size is known
let mut vec = Vec::with_capacity(1000);
for i in 0..1000 {
    vec.push(i);
}
```

## Avoid Unnecessary Copies

```rust
// Use references in iterations
for item in &collection {  // Not: for item in collection
    process(item);
}

// Use drain() when consuming is needed
for item in collection.drain(..) {
    consume(item);
}
```

## Fast hashing for internal maps

The std `HashMap` uses SipHash for HashDoS resistance. That protection only matters for maps keyed by untrusted input. For internal maps, `FxHash` is much faster.

```rust
use rustc_hash::FxHasher;
use std::{collections::HashMap, hash::BuildHasherDefault};

// Alias once; use FxHashMap for internal maps, std HashMap for attacker-facing ones.
type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
```

## Arena indices instead of pointer graphs

```rust
// Bad: `Rc<RefCell<Node>>` graphs: an allocation per node, runtime borrow checks.

// Good: newtype Copy indices into a flat arena. NonZeroU32 gives `Option<NodeId>`
// the same size as `NodeId` via the 0 niche, so optionality is free.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(std::num::NonZeroU32);

let nodes: IndexVec<NodeId, Node> = IndexVec::new();
```

## Compact collections on hot paths

```rust
// Small strings inline, no heap allocation up to ~24 bytes:
pub struct Name(compact_str::CompactString);

// One-word Vec for the common short-list case:
use thin_vec::ThinVec;

// Stack up to N elements before spilling to the heap. Justify N with a comment.
// Most lines have zero or one assertion, so optimize for a single element.
type Assertions = smallvec::SmallVec<[Assertion; 1]>;
```

## Shrink before caching; box immutable payloads

`collect`, `extend`, reservation, and `remove` can leave a collection over-allocated. Drop the spare capacity before stashing it in a long-lived cache.

```rust
vec.shrink_to_fit();

// Immutable payloads: convert to a boxed slice / str, with no capacity field
// (one fewer word), and it can never grow.
let payload: Box<[u8]> = data.into_boxed_slice();
let name: Box<str> = s.into_boxed_str();
```

## Byte strings for hot IO

On hot IO paths, work in bytes (`&[u8]`, `bstr`) and defer UTF-8 validation until you actually need text. Name your buffer capacities.

```rust
use bstr::ByteSlice;

const DEFAULT_BUFFER_CAPACITY: usize = 64 * (1 << 10); // 64 KB
```

## Cargo build profiles

```toml
# Keep line-table debug info in normal release builds so profilers and
# backtraces stay useful; the speed cost is negligible.
[profile.release]
debug = 1

# A dedicated max-optimization profile for shipping binaries.
[profile.release-lto]
inherits = "release"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
debug-assertions = false
overflow-checks = false

# Spend extra compile time only on the crates that dominate runtime.
[profile.release.package.parser]
codegen-units = 1

# Profiling: release speed, but keep symbols and full debug info.
[profile.profiling]
inherits = "release"
debug = "full"
strip = false
lto = false
```
