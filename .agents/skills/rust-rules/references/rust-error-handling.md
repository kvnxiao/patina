---
paths: **/*.{rs,toml}
description: "Rust error handling; anyhow for apps versus thiserror for libraries, opaque error types, from and source attributes, context, and keeping the happy path hot."
---

# Error Handling

For spawned work, apply the
[task outcome rules](rust-async.md#own-spawned-work-and-its-outcomes-required) when propagating join
failures and operation errors. Use the
[retry contract](rust-async.md#retry-only-repeatable-operations-conditional) when classifying
failures for another attempt.

## `anyhow` for applications, `thiserror` for libraries (Default)

The default error style depends on whether the caller branches on the failure.

- **Applications** that propagate failures toward a human default to `anyhow`, `?`, and `.context()`
  breadcrumbs.
- **Libraries** whose callers react differently to distinct failures default to an owned error type,
  usually derived with `thiserror`.

Apply the same caller-based choice to internal application crates. Use `anyhow` when their callers
only report failures; use typed errors when callers need to classify failures and recover.

The derive crate behind a public error type is an implementation detail, so switching between a
hand-written `Error` implementation and `thiserror` need not change the API. Changing a public
function's declared return type remains an API change, including a change from `anyhow::Error` to a
typed error.

## Write composable error messages (Default)

Write concise lowercase error messages without terminal punctuation, preserving the spelling of
identifiers and proper names. For example, use `invalid port` rather than `Invalid port.`.

When a wrapper exposes an underlying error through `source()`, describe only the wrapper's context
in `Display`. Let the reporting boundary format the source chain once. For example, use `reading
config.toml` as the context and retain the I/O error as its source. See the
[standard error conventions](https://doc.rust-lang.org/std/error/trait.Error.html).

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
    inner: Arc<ErrorInner>,
}

struct ErrorInner {
    kind: ErrorKind,
    source: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

enum ErrorKind { NotFound }

impl Error {
    /// Return whether the operation failed because a resource was absent.
    pub fn is_not_found(&self) -> bool {
        matches!(self.inner.kind, ErrorKind::NotFound)
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

Preserve the underlying error while propagating it: use `#[from]`, `#[source]`, or a context wrapper
so callers can inspect the source chain. Convert it to text at presentation or serialization
boundaries, or when an explicit boundary contract requires a textual representation. Keep the
original error available within the diagnostic path when the external representation omits it.

`.context(v)` evaluates its argument eagerly, on every call including the success path.
`.with_context(|| ...)` defers it until an error occurs. The message construction cost determines
the choice.

```rust
use anyhow::Context;
use anyhow::Result;

fn load(path: &Utf8Path) -> Result<Config> {
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
            inner: Arc::new(ErrorInner { kind, source: None }),
        }
    }
}
```
