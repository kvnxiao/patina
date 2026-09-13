---
paths: **/*.{rs,toml}
description: "General Rust code-quality patterns; edition and MSRV, derived metadata, enums over booleans and strings, #[must_use], and choosing function parameter types."
---

# Code Quality Standards

## Edition and MSRV (Default)

Default the project to a deliberate edition and a declared minimum supported Rust version. A
compatibility policy can require an older edition or compiler.

```toml
[package]
edition = "2024"
rust-version = "1.95"
```

## Derive collection and package metadata (Default)

Derive collection lengths and catalog membership from their data. Read package identity from Cargo
metadata instead of maintaining a separate version or package-name string. Keep deliberate limits,
protocol constants, and supported-version policies explicit; they define choices that the data
cannot determine.

For tests over a catalog or fixture directory, discover members and derive counts from the fixtures.
Keep expected parser results, serialized bytes, and golden outputs independent of the implementation
under test.

## Prefer Enums Over Booleans (Default)

A `bool` parameter is opaque at the call site, and adjacent flags invite transposition.

```rust
fn process(data: &str, is_verbose: bool, is_strict: bool) {}
```

Default to enums when callers choose among named states. Keep booleans for self-evident predicates
and setters.

```rust
#[derive(Debug, Clone, Copy)]
pub enum Verbosity { Quiet, Normal, Verbose }

#[derive(Debug, Clone, Copy)]
pub enum ValidationMode { Lenient, Strict }

fn process(data: &str, verbosity: Verbosity, validation: ValidationMode) {}
```

## Prefer domain types after string boundaries (Default)

A `&str` discriminant accepts any string and forces a fallible catch-all arm:

```rust
fn get_user_by_type(user_type: &str) -> Result<User> {
    match user_type {
        "admin" => load_admin(),
        "regular" => load_regular(),
        _ => Err(Error::InvalidUserType),
    }
}
```

Default to an enum after parsing at genuine string boundaries such as CLIs and wire formats.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserType { Admin, Regular, Guest }

fn get_user_by_type(user_type: UserType) -> Result<User> {
    match user_type {
        UserType::Admin => load_admin(),
        UserType::Regular => load_regular(),
        UserType::Guest => load_guest(),
    }
}
```

## Use `#[must_use]` strategically (Default)

When discarding an annotated type or return value likely indicates a bug, default it to
`#[must_use]`. A message is appropriate only when it gives the caller a non-obvious corrective
action. Types such as `Result` already carry the attribute, so a function returning them usually
needs no additional annotation.

```rust
#[must_use]
pub struct QueryBuilder { filters: Vec<Filter> }

impl QueryBuilder {
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }
}

impl Lock {
    #[must_use = "hold the guard for as long as the lock must remain acquired"]
    pub fn acquire(&self) -> LockGuard<'_> { todo!() }
}
```

Side-effecting functions whose return is incidental, simple getters, and expensive computations with
no discard bug do not meet this criterion. Work cost alone does not justify the attribute.

```rust
pub fn log_event(event: &Event) -> usize { todo!() }
pub fn len(&self) -> usize { self.items.len() }
```

See the Rust Reference for
[`must_use`](https://doc.rust-lang.org/reference/attributes/diagnostics.html#the-must_use-attribute)
semantics and the rustc
[`unused_must_use`](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#unused-must-use)
lint.

## Choosing Function Parameter Types (Default)

Default to concrete borrowed parameters such as `&str`, `&Utf8Path`, and `&[T]`. Use `impl AsRef<T>`
for a read-only API or `impl Into<T>` for an owning API only when accepting several common caller
types materially improves ergonomics.

Borrow concrete types when callers already have the expected representation:

```rust
pub fn validate_name(name: &str) -> bool {
    !name.is_empty() && name.len() < 100
}
```

An `impl AsRef<T>` parameter is appropriate when callers commonly hold multiple borrowed or owned
representations:

```rust
use camino::Utf8Path;

pub fn read_config(path: impl AsRef<Utf8Path>) -> Result<Config> {
    let content = fs_err::read_to_string(path.as_ref())?;
    todo!()
}
```

An `impl Into<T>` parameter is appropriate when the function needs ownership and conversion at the
boundary avoids repeated caller boilerplate:

```rust
impl User {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }
}

let user = User::new("Alice", "alice@example.com");
```

| Scenario                         | Recommended Type            | Example                            |
| -------------------------------- | --------------------------- | ---------------------------------- |
| Read-only access                 | `&str`, `&Utf8Path`, `&[T]` | `fn print(msg: &str)`              |
| Read-only, flexible input        | `impl AsRef<T>`             | `fn read(p: impl AsRef<Utf8Path>)` |
| Need ownership, want flexibility | `impl Into<T>`              | `fn new(name: impl Into<String>)`  |
| Need exact type                  | Concrete type               | `fn process(data: Vec<u8>)`        |
