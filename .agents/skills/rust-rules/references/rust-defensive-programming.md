---
paths: **/*.{rs,toml}
description: "Defensive Rust; validate inputs at boundaries, builder pattern, newtypes for type safety, and checked arithmetic."
---

# Defensive Programming

Validate untrusted inputs at API boundaries, represent constrained values with types, return
recoverable errors, and state arithmetic policy explicitly.

## Input Validation (Required)

Validate external input before constructing trusted domain values. Reject missing, oversized,
malformed, and out-of-range data with an actionable error.

```rust
pub fn process_user_input(input: &str) -> Result<ProcessedData> {
    if input.is_empty() {
        return Err(Error::validation("input", "must not be empty"));
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err(Error::validation(
            "input",
            format!("must not exceed {MAX_INPUT_BYTES} bytes"),
        ));
    }
    Ok(ProcessedData::new(input))
}
```

## Builder Pattern for Complex Types (Default)

When a configuration has optional fields or cross-field constraints, consider a builder. Follow the
[deferred-validation contract](rust-api-design.md#deferred-validation-builder-required) and
construct errors through the
[opaque error type](rust-error-handling.md#typed-errors-opaque-wrapper-over-a-private-repr-default).

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
            host: self.host.ok_or_else(|| Error::validation("host", "must be specified"))?,
            port: self.port.ok_or_else(|| Error::validation("port", "must be specified"))?,
            timeout: self.timeout.unwrap_or(Duration::from_secs(30)),
        })
    }
}
```

## Newtype Pattern for Type Safety (Default)

When the same primitive represents distinct domain identities, default to newtypes that prevent
transposition at the call site. Keep the primitive when no domain distinction exists.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductId(u64);

impl UserId {
    pub fn new(id: u64) -> Self { Self(id) }
    pub fn as_u64(self) -> u64 { self.0 }
}

let user = get_user(UserId::new(42))?;
```

Implement `Hash` when identifiers serve as hash keys and ordering traits when the domain defines an
ordering. Keep their behavior consistent with equality.

## Safe Arithmetic (Required)

Use checked arithmetic for untrusted or unbounded values unless the domain specifies saturation or
wrapping. When overflow checks are enabled, ordinary integer overflow panics. When they are
disabled, signed and unsigned integers wrap with two's-complement semantics. Integer overflow is
never undefined behavior. See
[Behavior not considered unsafe](https://doc.rust-lang.org/reference/behavior-not-considered-unsafe.html#integer-overflow).

```rust
let checked = a.checked_add(b)
    .ok_or_else(|| Error::validation("sum", "result would overflow"))?;

let capped = a.saturating_add(b);
let wrapped = a.wrapping_add(b);
```
