---
paths: **/*.{rs,toml}
description: "Rustdoc requirements; public-API and module docs, the module skeleton, crate-root cookbook, include_str rationale, and the docs.rs cfg knob."
---

# Documentation Requirements

## Public API Documentation (Default)

Default public library items to useful documentation that states the purpose and any invariants,
failure paths, panics, or side effects the signature cannot express. Examples are appropriate when
usage is not apparent from the type and name; redundant `# Arguments` sections and fixed paragraph
structures are omitted.

```rust
/// Normalize an account name for storage.
///
/// # Errors
///
/// Returns [`NameError::Empty`] if `input` is empty after trimming whitespace.
pub fn normalize_name(input: &str) -> Result<String, NameError> {
    let name = input.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    Ok(name.to_owned())
}
```

## Module Documentation (Default)

Default public modules to a concise model and primary entry points. An example is appropriate when
it clarifies how the items compose.

````rust
//! Load and validate application configuration.
//!
//! # Examples
//!
//! ```
//! use my_library::config::Config;
//!
//! let config = Config::from_file("config.toml")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
````

## Substantial public module guide (Conditional)

When a public module introduces a substantial concept or several related types, document its model,
primary entry points, and selection guidance. Use headings that fit the subject; the module does not
need a fixed skeleton.

````rust
//! Provide facilities for civil (time-zone-less) datetimes.
//!
//! # Overview
//!
//! - [`Date`]: a calendar date.
//! - [`Time`]: a wall-clock time.
//!
//! ```
//! use my_crate::civil::Date;
//! use my_crate::civil::date;
//! const DATE: Date = date(2024, 3, 14);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What is "civil" time?
//!
//! Civil time represents local calendar values without a time-zone offset.
//!
//! # When should I use civil time?
//!
//! Use civil time for calendar input before a time zone is known.
````

## Crate Root as Cookbook and Spec (Conditional)

For a published library, the crate root:

- Lists what the crate supports and does not support, with each unsupported feature linked to a
  tracking issue.
- States the [runtime panic policy](rust-api-design.md#use-fallible-runtime-constructors-required)
  and documents any explicitly approved exceptions.
- Includes a short cookbook of runnable, task-oriented examples.

When the README and crate documentation serve the same audience, share the content with `#![doc =
include_str!("../README.md")]`; eligible Rust blocks then run as doctests. Keep separate documents
when repository setup and API usage need different material. Run the
[shared documentation check](rust-lints-and-formatting.md#share-local-fix-lint-and-ci-tasks-required)
to catch broken intra-doc links and malformed markup.

## Long-Form Rationale via `include_str!` (Conditional)

When long-form design rationale exists, keep it in top-level Markdown and render it into rustdoc
through a hidden documentation module.

```rust
pub mod _documentation {
    #[doc = include_str!("../DESIGN.md")]
    pub mod design {}
    #[doc = include_str!("../PLATFORM.md")]
    pub mod platform {}
}
```

## Own Your `docs.rs` cfg Knob (Conditional)

When a crate uses nightly-only documentation attributes, use a crate-specific cfg name instead of
the shared `docsrs` name. Another crate can otherwise enable the shared cfg unexpectedly.

Register the name with `check-cfg` in the existing Rust lint table. For a workspace, put the lint
entry in `[workspace.lints.rust]` so members inherit it. Keep the docs.rs metadata in the package
manifest.

```toml
[lints.rust]
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(docsrs_mycrate)'] }

[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs_mycrate"]
```

```rust
#![cfg_attr(docsrs_mycrate, feature(doc_cfg))]
```

See
[Cargo's custom cfg registration](https://doc.rust-lang.org/rustc/check-cfg/cargo-specifics.html).
