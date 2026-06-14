//! The strict-undefined variable context bridging the six-layer
//! [`Resolver`](crate::variables::Resolver) into a single
//! [`minijinja::Value`].
//!
//! Under the engine's `SemiStrict` undefined behaviour, a reference to a
//! missing variable becomes an
//! [`ErrorKind::UndefinedError`](minijinja::ErrorKind) (on emit/coercion)
//! or an undefined *value* (as a `when`-expression result). Either way the
//! `MiniJinja` error carries only the generic message `"undefined value"`
//! — it does **not** name the variable that was missing. The typed
//! engine error's `Display` must name the offending variable
//! (`user_email`, `missing_var`, …).
//!
//! To recover the name we make the template context a dynamic
//! [`Object`]. Every top-level variable access routes through
//! `PatinaContext::get_value`; every `patina.*` access routes through
//! `PatinaBuiltins::get_value`; every `patina.env.*` access routes through
//! `PatinaEnv::get_value`. When a lookup resolves to nothing we record the
//! requested key in an interior-mutable set *before* returning [`None`]
//! (which `MiniJinja` turns into undefined). After a render fails or a
//! `when` evaluation yields undefined, the caller reads the recorded names
//! back out to build a typed error that names them.

use crate::variables::Resolver;
use minijinja::Value;
use minijinja::value::Enumerator;
use minijinja::value::Object;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;

/// Reserved namespace key exposed at the top level of the template
/// context. Every built-in lives under `patina.*`.
const PATINA_NAMESPACE: &str = "patina";

/// Sub-key under [`PATINA_NAMESPACE`] holding the dynamic process
/// environment map (`patina.env.FOO`).
const ENV_SUBKEY: &str = "env";

/// The top-level template context.
///
/// Resolves a bare name first against the six-layer user/built-in
/// [`Resolver`] (via [`Resolver::get`]), then against the special
/// `patina` namespace object. A name that resolves nowhere is recorded
/// in the shared [`UndefinedTracker`] and reported as [`None`].
#[derive(Debug)]
struct PatinaContext {
    resolver: Resolver,
    /// Pre-built `patina` namespace value, shared across every render
    /// and `when` evaluation in an apply.
    patina: Value,
    /// Names accessed at the top level that resolved to nothing. Shared
    /// with the `patina` sub-object so `patina.*` misses land here too.
    /// Interior-mutable because [`Object::get_value`] takes `&self`.
    undefined: Arc<Mutex<UndefinedTracker>>,
}

/// The `patina` namespace object: the static built-ins plus the dynamic
/// `env` sub-map.
#[derive(Debug)]
struct PatinaBuiltins {
    resolver: Resolver,
    env: Value,
    undefined: Arc<Mutex<UndefinedTracker>>,
}

/// The dynamic `patina.env` map. Each `patina.env.FOO` lookup reads the
/// process environment at access time; a miss is tracked as an
/// undefined named `patina.env.FOO`.
#[derive(Debug)]
struct PatinaEnv {
    undefined: Arc<Mutex<UndefinedTracker>>,
}

/// Records the names of variables that resolved to undefined during a
/// single render or `when` evaluation.
///
/// Always accessed behind a [`Mutex`], which both satisfies the
/// `Send + Sync` bound `MiniJinja` requires of an [`Object`] and grants
/// the exclusive `&mut` access the [`Object::get_value`] implementors (which
/// receive `&self`) need to push names.
#[derive(Debug, Default)]
pub(crate) struct UndefinedTracker {
    names: BTreeSet<String>,
}

impl UndefinedTracker {
    fn record(&mut self, name: &str) {
        self.names.insert(name.to_owned());
    }

    /// The recorded undefined names in sorted order. Sorted so the
    /// reported message is deterministic regardless of access order.
    pub(crate) fn names(&self) -> Vec<String> {
        self.names.iter().cloned().collect()
    }
}

/// Record `name` as undefined in `tracker` and return [`None`].
///
/// Shared by every [`Object::get_value`] miss path so a missing variable
/// is both surfaced to `MiniJinja` as undefined and remembered for the
/// typed error. A poisoned tracker lock degrades to "do not record" — the
/// lookup still reports undefined, so strict semantics hold even if the
/// name is lost.
fn record_undefined(tracker: &Mutex<UndefinedTracker>, name: &str) -> Option<Value> {
    if let Ok(mut tracker) = tracker.lock() {
        tracker.record(name);
    }
    None
}

/// Build the shared template context [`Value`] for one apply invocation.
///
/// Returns the context alongside the tracker handle the caller reads
/// after a strict-undefined failure to recover the offending names.
pub(crate) fn build_context(resolver: &Resolver) -> (Value, Arc<Mutex<UndefinedTracker>>) {
    let undefined = Arc::new(Mutex::new(UndefinedTracker::default()));

    let env = Value::from_object(PatinaEnv {
        undefined: Arc::clone(&undefined),
    });
    let patina = Value::from_object(PatinaBuiltins {
        resolver: resolver.clone(),
        env,
        undefined: Arc::clone(&undefined),
    });
    let context = Value::from_object(PatinaContext {
        resolver: resolver.clone(),
        patina,
        undefined: Arc::clone(&undefined),
    });

    (context, undefined)
}

impl Object for PatinaContext {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let key = key.as_str()?;
        if key == PATINA_NAMESPACE {
            return Some(self.patina.clone());
        }
        match self.resolver.get(key) {
            Some(value) => Some(Value::from(value)),
            None => record_undefined(&self.undefined, key),
        }
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        // Strict-undefined drives error reporting through `get_value`,
        // not enumeration. Advertising no keys keeps `{% for %}` over the
        // bare context a deliberate no-op rather than leaking the layer
        // internals.
        Enumerator::Empty
    }
}

impl Object for PatinaBuiltins {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let key = key.as_str()?;
        if key == ENV_SUBKEY {
            return Some(self.env.clone());
        }
        let qualified = format!("{PATINA_NAMESPACE}.{key}");
        match self.resolver.get(&qualified) {
            Some(value) => Some(Value::from(value)),
            None => record_undefined(&self.undefined, &qualified),
        }
    }
}

impl Object for PatinaEnv {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let key = key.as_str()?;
        match std::env::var(key) {
            Ok(value) => Some(Value::from(value)),
            Err(_) => record_undefined(
                &self.undefined,
                &format!("{PATINA_NAMESPACE}.{ENV_SUBKEY}.{key}"),
            ),
        }
    }
}
