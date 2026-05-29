---
paths: **/*.rs
---

# Let Types Carry Invariants

The unifying rule: **do not write code whose only job is to re-check
something the type or a prior operation already guarantees.** Redundant
layers and infallible-but-fallible-looking plumbing read as if a failure
mode exists where none does. They mislead the next reader and violate the
project's "Simplicity first / clear intent over clever code" mandate
(AGENTS.md, CLAUDE.md). The patterns below are real review blockers seen on
this codebase; each is mechanical to avoid at write time.

## No redundant interior-mutability layers

A `Mutex<T>` already grants exclusive `&mut T` through its guard. Wrapping
the inner value in `RefCell` (or `Cell`) adds a second interior-mutability
layer that does nothing but obscure intent and add a borrow to read
through. `RwLock`, `Mutex`, and the atomics are each sufficient on their
own.

```rust
// Bad: Mutex already gives &mut; the RefCell is dead weight.
struct Tracker {
    names: Mutex<RefCell<BTreeSet<String>>>,
}
fn record(t: &Mutex<RefCell<BTreeSet<String>>>, name: &str) {
    if let Ok(guard) = t.lock() {
        guard.borrow_mut().insert(name.to_owned());
    }
}

// Good: one lock, mutate the guard directly.
struct Tracker {
    names: Mutex<BTreeSet<String>>,
}
fn record(t: &Mutex<BTreeSet<String>>, name: &str) {
    if let Ok(mut guard) = t.lock() {
        guard.insert(name.to_owned());
    }
}
```

Reach for `Mutex<RefCell<T>>` / `Mutex<Cell<T>>` only when you genuinely
need to hand out the inner `RefCell`/`Cell` independently of the lock —
which is rare. If you can't name that reason, collapse to the single layer.

## Don't express infallible steps as fallible

`Option`/`Result`-returning adapters (`filter_map`, `.get(..)`,
`try_from(..).ok()`, `?`) tell the reader "this can fail and be silently
discarded." When the surrounding code statically guarantees success — most
often after `chunks_exact(N)`, a known-length array, or a bounds check you
just performed — use the infallible form so the control flow tells the
truth. A `filter_map` that never yields `None` is a lie about the data.

```rust
// Bad: chunks_exact(5) guarantees 5 bytes, so .get(..4) is always Some
// and try_from on a 4-byte slice is always Ok. Two dead failure arms.
bytes
    .chunks_exact(5)
    .filter(|r| r.get(4) == Some(&MARKER))
    .filter_map(|r| r.get(..4))
    .filter_map(|idx| <[u8; 4]>::try_from(idx).ok())
    .map(u32::from_le_bytes)
    .collect()

// Good: destructure the statically-known length; the only None arm left
// is the genuine runtime decision (marker mismatch).
bytes
    .chunks_exact(5)
    .filter_map(|r| match r {
        [b0, b1, b2, b3, MARKER] => Some(u32::from_le_bytes([*b0, *b1, *b2, *b3])),
        _ => None,
    })
    .collect()
```

Note the Good form keeps `filter_map` because there *is* a real filter (the
marker test); what changed is that the byte extraction no longer pretends to
be fallible. Do not reach for `.expect()` to "prove" infallibility in
production — that is a panic path (forbidden outside tests); use an
irrefutable pattern (`let [a, b, c, d] = arr;`, a slice pattern on a
`chunks_exact` item) that the compiler accepts because the length is known.

## Reuse the existing helper; grep before you write one

Before adding a small filesystem, path, or byte-twiddling helper, search the
crate for an existing one — sibling modules frequently already have it. A
parallel copy drifts, and the copy is often written with a worse signature
than the original (e.g. taking `&Utf8PathBuf` where the canonical version
correctly takes `&Utf8Path` per the parameter-type hierarchy in
`rust-code-quality.md`). If a sibling's private helper is what you need,
promote it to `pub(super)` / `pub(crate)` and call it rather than
duplicating.

**Do this at write time, not after review flags it.** Before you write the
body of a free function or inherent method, grep the crate for an existing
one. The shapes that have actually shipped as duplicates and been bounced
back on this codebase:

- **Recursive directory walks** — a `fn` that recurses into a dir and
  collects/strips-prefix relative entries. If two submodules each need one,
  hoist a single `pub(super)` helper into the parent `mod.rs` and call it
  from both; do not write the walk twice.
- **Enum → `&'static str` labels** — a method or free fn mapping enum
  variants to a fixed word. If the word already appears anywhere in public
  output (e.g. inside a `Display` impl or an error message), the mapping is
  part of the type's surface: define it once on the enum and promote its
  visibility rather than re-spelling the `match` at the call site.
- **Path / byte-twiddling helpers** — see the `&Utf8Path` signature note
  above.

The check is mechanical: `rg 'fn <name>'` (and a scan for the same `match`
arms or `walk`/`recurse` shape) across the crate **before** typing the body.
If a private sibling already has it, promote and call. This is a write-time
step, not a principle to recall — running it costs seconds and removes the
most common style-review bounce on this repo.

```rust
// Bad: recovery.rs reinvents a helper mod.rs already has, with a worse param type.
//   mod.rs:      fn remove_if_present(path: &Utf8Path)    -> Result<()>
//   recovery.rs: fn remove_if_present(path: &Utf8PathBuf) -> Result<()>  // duplicate

// Good: promote the original and call it.
//   mod.rs:      pub(super) fn remove_if_present(path: &Utf8Path) -> Result<()>
//   recovery.rs: super::remove_if_present(&plan_path)?;
```
