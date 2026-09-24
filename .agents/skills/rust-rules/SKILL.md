---
name: rust-rules
description: "Use for Rust implementation, review, API design, async and concurrency, testing, error handling, diagnostics, dependencies, documentation, linting, formatting, performance, and workspace structure."
---

# Rust Rules

Use for Rust implementation, review, API design, async and concurrency, testing, error handling, diagnostics, dependencies, documentation, linting, formatting, performance, and workspace structure.

## Rule Strength

- **Required**: Follow this rule to preserve correctness, security, lifecycle, or compatibility.
- **Default**: Follow this project convention unless a stated exception applies.
- **Conditional**: Apply this rule only when its stated condition or measurement is present.

## Rule References

- [API design](references/rust-api-design.md): Read when adding or reviewing public APIs, constructors, traits, macros, unsafe code, features, or no_std behavior.
- [Async and concurrency](references/rust-async.md): Read when choosing async APIs or runtimes, spawning tasks, handling cancellation and shutdown, bounding work, sharing state, setting deadlines and retries, tracing operations, or testing async behavior.
- [Code quality](references/rust-code-quality.md): Read when implementing or reviewing Rust code for maintainability, idioms, module shape, naming, and readability, or when deriving collection and package metadata.
- [Defensive programming](references/rust-defensive-programming.md): Read when handling invariants, input validation, panics, assertions, boundaries, and failure modes.
- [Dependencies](references/rust-dependencies.md): Read when adding, updating, configuring, or evaluating Rust crate dependencies and feature flags.
- [Documentation](references/rust-documentation.md): Read when writing or reviewing docs, examples, crate-level docs, public API docs, and README guidance.
- [Error handling](references/rust-error-handling.md): Read when designing or reviewing Result types, error enums, context, propagation, panics, and diagnostics.
- [Lints and formatting](references/rust-lints-and-formatting.md): Read when bootstrapping a Rust project or configuring rustfmt, Clippy, lint exceptions, and shared local/CI tasks; install the complete required baseline.
- [Performance](references/rust-performance.md): Read when optimizing Rust code, reducing allocations, choosing data structures, or reviewing hot paths.
- [Testing](references/rust-testing.md): Read when adding, refactoring, or reviewing unit tests, integration tests, fixtures, property tests, and test ergonomics.
- [Workspaces](references/rust-workspaces.md): Read when changing Cargo workspace structure, package boundaries, crate metadata, features, or shared configuration.
