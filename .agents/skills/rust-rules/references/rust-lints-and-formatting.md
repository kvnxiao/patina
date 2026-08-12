---
paths: **/*.{rs,toml}
description: "Clippy and rustfmt config; pedantic with justified allows, promoted restriction lints, clippy.toml, -D warnings in CI not source, and rustfmt.toml."
---

# Lints and Formatting

## Enable `pedantic` group-wide, then allow back with a reason

Turn on the whole `clippy::pedantic` group at a low priority, then `allow` the handful you reject, each with a one-line reason. `priority = -2` makes the group lose to individual lint lines, so your overrides win regardless of order.

```toml
[workspace.lints.clippy]
pedantic = { level = "warn", priority = -2 }

# Allowed pedantic lints, each justified.
match_same_arms = "allow"
module_name_repetitions = "allow"
needless_continue = "allow" # an explicit continue can read better than an empty else
```

## Promote restriction lints to warnings

Several `clippy::restriction` lints catch real mistakes in library and tool code. Promote them:

```toml
print_stdout = "warn"
print_stderr = "warn"
dbg_macro = "warn"
exit = "warn"
get_unwrap = "warn"
rc_mutex = "warn"
iter_over_hash_type = "warn" # forces deterministic iteration order
```

`iter_over_hash_type` forces deterministic iteration order: iterating a `HashMap` in hash order is a nondeterminism bug waiting to happen.

## Funnel risky std calls through `clippy.toml`

`disallowed-methods` with a per-entry `reason` routes `std::fs`/`std::env` through an injectable abstraction (so tests can fake the filesystem). The reason shows up in the lint message. `doc-valid-idents` silences `doc_markdown` on domain words.

```toml
# clippy.toml
disallowed-methods = [
    { path = "std::env::var", reason = "use System::env_var so tests can inject env" },
    { path = "std::fs::read_to_string", reason = "use System::read_to_string" },
]

doc-valid-idents = ["NumPy", "PyCharm", "SQLAlchemy"]
```

## Justify every `allow`

Group `#![allow(...)]` by reason and annotate each entry. Prefer `#[expect(...)]` over `#[allow(...)]` where the toolchain supports it: an `expect` that stops firing is itself a warning, so stale suppressions surface instead of lingering.

```rust
#![allow(
    // clippy bug: https://github.com/rust-lang/rust-clippy/issues/5704
    clippy::unnested_or_patterns,
    // integer ser/de legitimately needs these casts
    clippy::cast_possible_truncation,
)]
```

## Keep `-D warnings` in CI, not in source

Enforce the strict bar in CI so a new compiler or clippy lint fails *your* build, not every downstream that compiles your published crate.

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Do **not** put `#![deny(warnings)]` in source: a future toolchain that adds a lint would break consumers building your crate through no fault of theirs.

## Commit a minimal `rustfmt.toml`

Pin the formatting so it doesn't drift across contributors and toolchains. Bump `edition`/`style_edition` alongside the crate edition.

```toml
edition = "2024"
style_edition = "2024"
# Optional width tuning, e.g.:
# max_width = 79
# use_small_heuristics = "max"
```
