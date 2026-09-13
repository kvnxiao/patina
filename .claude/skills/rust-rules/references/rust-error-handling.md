---
paths: **/*.{rs,toml}
description: "Rust error handling; anyhow for apps versus thiserror for libraries, opaque error types, from and source attributes, context, and keeping the happy path hot."
---

# Error Handling

## `anyhow` for applications, `thiserror` for libraries (Default)

The default error style depends on whether the caller branches on the failure.

- **Applications** that propagate failures toward a human default to `anyhow`, `?`, and `.context()`
  breadcrumbs.
- **Libraries** whose callers react differently to distinct failures default to an owned error type,
  usually derived with `thiserror`.

The derive crate behind a public error type is an implementation detail, so switching between a
hand-written `Error` implementation and `thiserror` need not change the API. Changing a public
function's declared return type remains an API change, including a change from `anyhow::Error` to a
typed error.

## Typed errors: opaque wrapper over a private repr (Default)

A public enum exposes its variants in the SemVer surface. Default to an opaque public wrapper over a
private enum when the failure set is expected to evolve. A small variant set can remain public when
the variants are genuinely stable and useful for exhaustive matching.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
#[error(transparent)]
pub struct ParseError(#[from] ErrorRepr);

impl ParseError {
    pub fn is_eof(&self) -> bool {
        matches!(self.0, ErrorRepr::UnexpectedEof)
    }
}

#[derive(Debug, Error)]
enum ErrorRepr {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("invalid token at byte {offset}")]
    InvalidToken { offset: usize },
}
```

## `#[from]`, `#[source]`, and `'static` (Default)

```rust
#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o failed")]
    Io(#[from] std::io::Error),

    #[error("parse failed at byte {offset}")]
    Parse { source: ParseError, offset: usize },
}
```

A source must be `'static` — `std::error::Error::source` returns `&(dyn Error + 'static)`, so a
source field carrying a borrowed lifetime will not compile.

## One error type per crate (Conditional)

When a large API needs one small and stable error surface, use a crate-wide opaque `Error` whose
variants stay private. Callers classify through non-exhaustive `is_*` predicates. A pointer-sized
wrapper can make cloning cheap by storing the payload behind `Arc`. When callers benefit from a
private but concrete variant set, use the opaque-wrapper enum instead.

```rust
use std::sync::Arc;

#[derive(Clone)]
pub struct Error {
    inner: Option<Arc<ErrorInner>>,
}

struct ErrorInner {
    kind: ErrorKind,
    source: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

enum ErrorKind { NotFound }

impl Error {
    /// Return whether the operation failed because a resource was absent.
    pub fn is_not_found(&self) -> bool {
        matches!(self.inner.as_deref().map(|i| &i.kind), Some(ErrorKind::NotFound))
    }
}
```

## `Result` alias with a defaulted error param (Default)

Default to a crate-level alias when most fallible APIs share one error type. Keep explicit
`Result<T, E>` spelling when several error types are equally common.

```rust
pub type Result<T, E = Error> = core::result::Result<T, E>;
```

## Add context: eager vs lazy (Default)

`.context(v)` evaluates its argument eagerly, on every call including the success path.
`.with_context(|| ...)` defers it until an error occurs. The message construction cost determines
the choice.

```rust
use anyhow::{Context, Result};

fn load(path: &Utf8Path) -> Result<Config> {
    let text = fs_err::read_to_string(path).context(format!("reading {path}"))?;

    let text = fs_err::read_to_string(path).with_context(|| format!("reading {path}"))?;

    toml::from_str(&text).context("parsing config")
}
```

A crate-wide error type can offer the same `.context()` chaining on its own type; a `thiserror` enum
instead carries context through `#[source]` fields.

## Inspect an error: walk the chain, downcast (Default)

When an application must classify an underlying failure, default to walking the source chain and
downcasting each cause. Inspect only the outer error when wrappers are part of the intended
classification boundary.

```rust
use anyhow::Error;

fn io_error_kind(err: &Error) -> Option<std::io::ErrorKind> {
    for cause in err.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
    }
    None
}
```

## Keep the happy path hot (Conditional)

When profiling or hot-path evidence identifies error construction or layout as material, mark
constructors `#[cold]`. Add `#[inline(never)]` only when benchmarks show that forced non-inlining
improves the relevant path.

```rust
impl Error {
    #[cold]
    #[inline(never)]
    fn new(kind: ErrorKind) -> Error {
        Error {
            inner: Some(Arc::new(ErrorInner { kind, source: None })),
        }
    }
}
```
