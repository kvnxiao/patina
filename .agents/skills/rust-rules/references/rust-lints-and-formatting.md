---
paths: **/*.{rs,toml}
description: "Clippy and rustfmt config; pedantic with justified allows, promoted restriction lints, clippy.toml, -D warnings in CI not source, and rustfmt.toml."
---

# Lints and Formatting

## Enable `pedantic` group-wide, then allow back with a reason (Default)

Default the full `clippy::pedantic` group to a low-priority warning, then allow rejected lints with
one-line reasons. A project with a fixed lint baseline can select individual pedantic lints instead.
`priority = -2` makes individual lint settings override the group regardless of order.

```toml
[workspace.lints.clippy]
pedantic = { level = "warn", priority = -2 }

match_same_arms = "allow"
module_name_repetitions = "allow"
needless_continue = "allow" # an explicit continue can read better than an empty else
```

## Promote restriction lints to warnings (Default)

For library and tool projects, default the following `clippy::restriction` lints to warnings. Omit a
lint when its prohibited operation is part of the project contract.

```toml
print_stdout = "warn"
print_stderr = "warn"
dbg_macro = "warn"
exit = "warn"
get_unwrap = "warn"
rc_mutex = "warn"
iter_over_hash_type = "warn"
```

`iter_over_hash_type` flags iteration whose result can depend on randomized hash order.

## Configure risky std calls in `clippy.toml` (Default)

For projects that require injectable environment and filesystem access, disallow direct `std::env`
and `std::fs` calls with per-entry reasons. The reason appears in the lint message. Use
`doc-valid-idents` to exempt domain words from `doc_markdown`.

```toml
disallowed-methods = [
    { path = "std::env::var", reason = "use System::env_var so tests can inject env" },
    { path = "std::fs::read_to_string", reason = "use System::read_to_string" },
]

doc-valid-idents = ["NumPy", "PyCharm", "SQLAlchemy"]
```

## Justify every `allow` (Default)

Default lint suppressions to groups organized by reason, with each entry justified. Prefer
`#[expect(...)]` where the supported toolchain provides it; when the lint stops firing, the stale
expectation emits a warning.

```rust
#![allow(
    // Clippy issue: https://github.com/rust-lang/rust-clippy/issues/5704
    clippy::unnested_or_patterns,
    // Integer serialization and deserialization require these casts.
    clippy::cast_possible_truncation,
)]
```

## Keep `-D warnings` in CI, not in source (Default)

Default strict lint enforcement to CI. A new compiler or Clippy lint then fails the project build
without imposing source-level warning policy on downstream users.

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Keep `#![deny(warnings)]` out of source because a future toolchain that adds a lint would break
consumers building the crate.

## Commit a minimal `rustfmt.toml` (Default)

Default the repository to a minimal `rustfmt.toml` that pins formatting across contributors and
toolchains. Update `edition` and `style_edition` with the crate edition.

```toml
edition = "2024"
style_edition = "2024"
```
