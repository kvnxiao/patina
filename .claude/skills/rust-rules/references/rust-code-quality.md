---
paths: **/*.{rs,toml}
description: "General Rust code-quality patterns; edition and MSRV, enums over booleans and strings, #[must_use], and choosing function parameter types."
---

# Code Quality Standards

## Edition and MSRV

Track the latest stable edition and pin the MSRV.

```toml
[package]
edition = "2024"
rust-version = "1.95"
```

## Prefer Enums Over Booleans

A `bool` parameter is opaque at the call site, and adjacent flags invite transposition. Name the states.

```rust
// Bad: process(data, true, false) passes two anonymous flags.
fn process(data: &str, is_verbose: bool, is_strict: bool) {}

// Good
#[derive(Debug, Clone, Copy)]
pub enum Verbosity { Quiet, Normal, Verbose }

#[derive(Debug, Clone, Copy)]
pub enum ValidationMode { Lenient, Strict }

fn process(data: &str, verbosity: Verbosity, validation: ValidationMode) {}
```

## Avoid Stringly-Typed Code

A `&str` discriminant accepts any string and forces a fallible catch-all arm. An enum makes the match exhaustive and the invalid case unrepresentable.

```rust
// Bad
fn get_user_by_type(user_type: &str) -> Result<User> {
    match user_type {
        "admin" => { /* ... */ }
        "regular" => { /* ... */ }
        _ => Err(Error::InvalidUserType),
    }
}

// Good
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserType { Admin, Regular, Guest }

fn get_user_by_type(user_type: UserType) -> Result<User> {
    match user_type {
        UserType::Admin => { /* ... */ }
        UserType::Regular => { /* ... */ }
        UserType::Guest => { /* ... */ }
    }
}
```

## Use `#[must_use]` Strategically

Add `#[must_use]` when ignoring a value is likely a bug, and always give a custom message explaining why it matters.

```rust
// Results and error-returning validators
#[must_use = "errors must be handled, not silently ignored"]
pub fn validate_config(config: &Config) -> Result<(), ValidationError> { /* ... */ }

// Builder types and their chained methods
#[must_use = "builders must be used to construct the final value"]
pub struct QueryBuilder { /* ... */ }

impl QueryBuilder {
    #[must_use = "this returns a new builder with the filter added"]
    pub fn filter(mut self, f: Filter) -> Self { /* ... */ self }
}

// Expensive computations
#[must_use = "computing the hash is expensive; use the result"]
pub fn compute_hash(data: &[u8]) -> Hash { /* ... */ }

// Values representing a state change
#[must_use = "the guard must be held to maintain the lock"]
pub fn acquire_lock(&self) -> LockGuard<'_> { /* ... */ }

#[must_use = "the previous value may need to be processed"]
pub fn swap(&mut self, new_value: T) -> T {
    std::mem::replace(&mut self.value, new_value)
}
```

Skip it for side-effecting functions whose return is incidental, and for simple getters.

```rust
pub fn log_event(event: &Event) -> usize { /* bytes written; logging happened anyway */ }
pub fn len(&self) -> usize { self.items.len() }
```

## Choosing Function Parameter Types

Pick the least demanding type that does the job, from most to least flexible.

**1. Borrow when you only read:**

```rust
pub fn validate_name(name: &str) -> bool {
    !name.is_empty() && name.len() < 100
}
```

**2. `impl AsRef<T>` for flexible read-only input:**

```rust
use camino::Utf8Path;

pub fn read_config(path: impl AsRef<Utf8Path>) -> Result<Config> {
    let content = fs_err::read_to_string(path.as_ref())?;
    // ...
}
```

**3. `impl Into<T>` when you need to own the value:**

```rust
impl User {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }
}

let user = User::new("Alice", "alice@example.com"); // no .to_string() at the call site
```

| Scenario                         | Recommended Type            | Example                              |
| -------------------------------- | --------------------------- | ------------------------------------ |
| Read-only access                 | `&str`, `&Utf8Path`, `&[T]` | `fn print(msg: &str)`                |
| Read-only, flexible input        | `impl AsRef<T>`             | `fn read(p: impl AsRef<Utf8Path>)`   |
| Need ownership, want flexibility | `impl Into<T>`              | `fn new(name: impl Into<String>)`    |
| Need exact type                  | Concrete type               | `fn process(data: Vec<u8>)`          |
