---
paths: **/*.{rs,toml}
description: "Rustdoc requirements; public-API and module docs, the module skeleton, crate-root cookbook, include_str rationale, and the docs.rs cfg knob."
---

# Documentation Requirements

## Public API Documentation

Every public item must have documentation. Clippy enforces `# Errors`
and `# Panics` sections (`missing_errors_doc` / `missing_panics_doc`);
this rule covers the rest: the prose summary, `# Arguments`, and
`# Examples`.

```rust
/// Processes the input data and returns a processed result.
///
/// # Arguments
///
/// * `input` - The input string to process
/// * `options` - Processing options
///
/// # Examples
///
/// ```
/// use my_library::{process, Options};
///
/// let result = process("hello", Options::default())?;
/// assert_eq!(result.value(), "HELLO");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn process(input: &str, options: Options) -> Result<ProcessedData> {
    // Implementation
}
```

## Module Documentation

```rust
//! Configuration management for the application.
//!
//! This module provides types and functions for loading, validating,
//! and managing application configuration.
//!
//! # Examples
//!
//! ```
//! use my_library::config::Config;
//!
//! let config = Config::from_file("config.toml")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
```

## Module `//!` Skeleton

For a substantial module, follow a three-part skeleton: an **Overview** (bulleted list of the types, plus runnable examples), a **"What is X?"** concept section, then a **"When should I use X?"** decision guide that points at the alternatives.

```rust
//! Facilities for civil (time-zone-less) datetimes.
//!
//! # Overview
//!
//! - [`Date`]: a calendar date.
//! - [`Time`]: a wall-clock time.
//!
//! ```
//! use my_crate::civil::date;
//! let d = date(2024, 3, 14);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What is "civil" time?
//!
//! (the concept, in prose)
//!
//! # When should I use civil time?
//!
//! (a decision guide contrasting the alternatives)
```

## Crate Root as Cookbook and Spec

New users land at the crate root. Make it useful:

- List what the crate supports, and explicitly what it does **not**, linking each gap to a tracking issue.
- State the panic policy ("APIs that panic by design are few and clearly documented as such").
- Embed a short cookbook of runnable, task-oriented examples.

## Long-Form Rationale via `include_str!`

Keep design rationale in top-level Markdown (PR-reviewable, one source of truth) and render it into the docs through a hidden documentation module.

```rust
/// Longer-form documentation.
pub mod _documentation {
    #[doc = include_str!("../DESIGN.md")]
    pub mod design {}
    #[doc = include_str!("../PLATFORM.md")]
    pub mod platform {}
}
```

## Own Your `docs.rs` cfg Knob

Use a crate-specific cfg name, not the shared `docsrs`, so another crate's use of `docsrs` can't accidentally toggle your nightly-only doc attributes.

```toml
[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs_mycrate"]
```

```rust
#![cfg_attr(docsrs_mycrate, feature(doc_cfg))]
```
