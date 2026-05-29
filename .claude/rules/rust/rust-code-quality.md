---
paths: **/*.{rs,toml}
---

# Code Quality Standards

## Rust Edition and Features

Use the latest stable Rust edition:

```toml
[package]
edition = "2024"
rust-version = "1.95"  # Specify MSRV
```

## Code Organization

```rust
// Module structure should be clear and logical
// src/lib.rs or src/main.rs
mod config;
mod error;
mod models;
mod api;
mod utils;

pub use error::{Error, Result};
pub use config::Config;
```

## Prefer Enums Over Booleans

```rust
// Bad
fn process(data: &str, is_verbose: bool, is_strict: bool) { }

// Good
#[derive(Debug, Clone, Copy)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

#[derive(Debug, Clone, Copy)]
pub enum ValidationMode {
    Lenient,
    Strict,
}

fn process(data: &str, verbosity: Verbosity, validation: ValidationMode) { }
```

## Avoid Stringly-Typed Code

```rust
// Bad
fn get_user_by_type(user_type: &str) -> Result<User> {
    match user_type {
        "admin" => { /* ... */ },
        "regular" => { /* ... */ },
        _ => Err(Error::InvalidUserType),
    }
}

// Good
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserType {
    Admin,
    Regular,
    Guest,
}

fn get_user_by_type(user_type: UserType) -> Result<User> {
    match user_type {
        UserType::Admin => { /* ... */ },
        UserType::Regular => { /* ... */ },
        UserType::Guest => { /* ... */ },
    }
}
```

## Use `#[must_use]` Strategically

**Core principle:** Use `#[must_use]` when ignoring a value would likely be a mistake or indicate a logic error.

**ALWAYS include a custom message** to provide context to the caller about why the value matters.

### When to Use `#[must_use]`

**1. Results and Error Types**

```rust
#[must_use = "errors must be handled, not silently ignored"]
pub fn validate_config(config: &Config) -> Result<(), ValidationError> {
    // ...
}
```

**2. Builder Patterns**

```rust
#[must_use = "builders must be used to construct the final value"]
pub struct QueryBuilder {
    filters: Vec<Filter>,
}

impl QueryBuilder {
    #[must_use = "this returns a new builder with the filter added"]
    pub fn filter(mut self, f: Filter) -> Self {
        self.filters.push(f);
        self
    }

    #[must_use = "call .execute() to run the query"]
    pub fn build(self) -> Query {
        Query { filters: self.filters }
    }
}
```

**3. Expensive Computations**

```rust
#[must_use = "computing the hash is expensive; use the result"]
pub fn compute_hash(data: &[u8]) -> Hash {
    // CPU-intensive hashing...
}
```

**4. Values Representing State Changes**

```rust
#[must_use = "the guard must be held to maintain the lock"]
pub fn acquire_lock(&self) -> LockGuard<'_> {
    // ...
}

#[must_use = "the previous value may need to be processed"]
pub fn swap(&mut self, new_value: T) -> T {
    std::mem::replace(&mut self.value, new_value)
}
```

### When NOT to Use `#[must_use]`

```rust
// Don't use for side-effect functions where the return is optional info
pub fn log_event(event: &Event) -> usize {
    // Returns bytes written, but logging happened regardless
}

// Don't use for simple getters
pub fn len(&self) -> usize {
    self.items.len()
}
```

## Choosing Function Parameter Types

### Decision Hierarchy

**1. Prefer borrowed types when you don't need ownership:**

```rust
// Accept &str when you only need to read
pub fn validate_name(name: &str) -> bool {
    !name.is_empty() && name.len() < 100
}
```

**2. Use `impl AsRef<T>` for maximum flexibility without ownership:**

> **Path types in patina:** use `camino::Utf8Path` / `Utf8PathBuf` instead of `std::path::Path` / `PathBuf`. Patina mandates UTF-8 paths everywhere except at OS-API boundaries; see AGENTS.md "Code conventions". Filesystem reads/writes go through `fs-err`, not `std::fs`. The example below illustrates the parameter-typing pattern; substitute these crates in real code.

```rust
use camino::Utf8Path;

pub fn read_config(path: impl AsRef<Utf8Path>) -> Result<Config> {
    let path = path.as_ref();
    let content = fs_err::read_to_string(path)?;
    // ...
}
```

**3. Use `impl Into<T>` when you need ownership:**

```rust
impl User {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
        }
    }
}

// Clean call site - no .to_string() needed
let user = User::new("Alice", "alice@example.com");
```

### Summary

| Scenario                         | Recommended Type            | Example                              |
| -------------------------------- | --------------------------- | ------------------------------------ |
| Read-only access                 | `&str`, `&Utf8Path`, `&[T]` | `fn print(msg: &str)`                |
| Read-only, flexible input        | `impl AsRef<T>`             | `fn read(p: impl AsRef<Utf8Path>)`   |
| Need ownership, want flexibility | `impl Into<T>`              | `fn new(name: impl Into<String>)`    |
| Need exact type                  | Concrete type               | `fn process(data: Vec<u8>)`          |
