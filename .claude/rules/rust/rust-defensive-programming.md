---
paths: **/*.{rs,toml}
---

# Defensive Programming

## Core Principles

1. **Validate all inputs** at API boundaries
2. **Use the type system** to enforce invariants
3. **Avoid panics** in library code
4. **Check preconditions** explicitly
5. **Handle all error cases** exhaustively

## Input Validation

```rust
pub fn process_user_input(input: &str) -> Result<ProcessedData> {
    // Validate input is not empty
    if input.is_empty() {
        return Err(MyLibraryError::ValidationError {
            field: "input".to_string(),
            constraint: "must not be empty".to_string(),
        });
    }

    // Validate length constraints
    if input.len() > MAX_INPUT_LENGTH {
        return Err(MyLibraryError::ValidationError {
            field: "input".to_string(),
            constraint: format!("must not exceed {} characters", MAX_INPUT_LENGTH),
        });
    }

    // Continue processing...
    Ok(ProcessedData::new(input))
}
```

## Use Builder Pattern for Complex Types

```rust
#[derive(Debug)]
pub struct Config {
    host: String,
    port: u16,
    timeout: Duration,
}

pub struct ConfigBuilder {
    host: Option<String>,
    port: Option<u16>,
    timeout: Option<Duration>,
}

impl ConfigBuilder {
    pub fn new() -> Self {
        Self {
            host: None,
            port: None,
            timeout: None,
        }
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

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

```rust
// Instead of using raw types that can be confused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductId(u64);

impl UserId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

// Now you can't accidentally mix up user IDs and product IDs
fn get_user(id: UserId) -> Result<User> {
    // Type system prevents passing ProductId here
    todo!()
}
```

## Safe Arithmetic

```rust
// Bad: Can overflow in debug, wraps in release
let result = a + b;

// Good: Handle overflow explicitly
let result = a.checked_add(b)
    .ok_or(MyLibraryError::ValidationError {
        field: "sum".to_string(),
        constraint: "result would overflow".to_string(),
    })?;

// Or use saturating/wrapping explicitly when appropriate
let result = a.saturating_add(b); // When overflow should cap
let result = a.wrapping_add(b);   // When overflow should wrap (make explicit)
```
