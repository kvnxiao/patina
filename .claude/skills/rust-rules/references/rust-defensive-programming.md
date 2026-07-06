---
paths: **/*.{rs,toml}
description: "Defensive Rust; validate inputs at boundaries, builder pattern, newtypes for type safety, and checked arithmetic."
---

# Defensive Programming

## Core Principles

- Validate all inputs at API boundaries.
- Use the type system to make invalid states unrepresentable.
- Avoid panics in library code; return `Result`.
- Check preconditions explicitly and handle every error case.

## Input Validation

```rust
pub fn process_user_input(input: &str) -> Result<ProcessedData> {
    if input.is_empty() {
        return Err(MyLibraryError::ValidationError {
            field: "input".to_string(),
            constraint: "must not be empty".to_string(),
        });
    }
    if input.len() > MAX_INPUT_LENGTH {
        return Err(MyLibraryError::ValidationError {
            field: "input".to_string(),
            constraint: format!("must not exceed {MAX_INPUT_LENGTH} characters"),
        });
    }
    Ok(ProcessedData::new(input))
}
```

## Builder Pattern for Complex Types

Store each field as `Option`; validate the whole config once in `build()`.

```rust
#[derive(Debug)]
pub struct Config {
    host: String,
    port: u16,
    timeout: Duration,
}

#[derive(Default)]
pub struct ConfigBuilder {
    host: Option<String>,
    port: Option<u16>,
    timeout: Option<Duration>,
}

impl ConfigBuilder {
    pub fn host(mut self, host: impl Into<String>) -> Self { self.host = Some(host.into()); self }
    pub fn port(mut self, port: u16) -> Self { self.port = Some(port); self }
    pub fn timeout(mut self, timeout: Duration) -> Self { self.timeout = Some(timeout); self }

    pub fn build(self) -> Result<Config> {
        Ok(Config {
            host: self.host.ok_or_else(|| MyLibraryError::ValidationError {
                field: "host".to_string(),
                constraint: "must be specified".to_string(),
            })?,
            port: self.port.ok_or_else(|| MyLibraryError::ValidationError {
                field: "port".to_string(),
                constraint: "must be specified".to_string(),
            })?,
            timeout: self.timeout.unwrap_or(Duration::from_secs(30)),
        })
    }
}
```

## Newtype Pattern for Type Safety

Wrap primitives so distinct IDs can't be transposed at a call site.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductId(u64);

impl UserId {
    pub fn new(id: u64) -> Self { Self(id) }
    pub fn as_u64(self) -> u64 { self.0 }
}

// The compiler rejects a ProductId where a UserId is expected.
fn get_user(id: UserId) -> Result<User> { /* ... */ }
```

## Safe Arithmetic

```rust
// Bad: overflow panics in debug, wraps in release.
let result = a + b;

// Good: handle overflow explicitly.
let result = a.checked_add(b).ok_or(MyLibraryError::ValidationError {
    field: "sum".to_string(),
    constraint: "result would overflow".to_string(),
})?;

// Or choose an explicit policy:
let result = a.saturating_add(b); // cap at the bound
let result = a.wrapping_add(b);   // wrap on overflow
```
