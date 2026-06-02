//! Layered variable resolution with the reserved `patina.*` namespace
//! (REQ-007).
//!
//! The engine resolves a variable name by walking six layers in priority
//! order from highest to lowest:
//!
//! 1. **CLI overrides** — `-v key=value` repeated on the command line.
//! 2. **Per-machine** — variables persisted under the per-machine state
//!    directory.
//! 3. **Per-profile** — variables declared in the active profile's TOML.
//! 4. **Per-module** — the module's `patina.toml` `[variables]` table.
//! 5. **Repo-shared** — the root `patina.toml` `[variables]` table.
//! 6. **Built-ins** — the `patina.*` namespace resolved at process start (plus
//!    the dynamic `patina.env.*` map evaluated on every lookup).
//!
//! The first five layers carry only user-provided keys; any key starting
//! with `patina.` in any of those layers is rejected as
//! [`VariableError::ReservedKey`]. The sixth layer ([`Builtins`]) owns the
//! `patina.*` namespace exclusively.
//!
//! `[variables]` tables declared inside a `patina.toml` are validated for
//! the reserved-key rule at parse time via [`reject_reserved_keys`]; the
//! same helper is reused by the resolver when CLI / per-machine /
//! per-profile / per-module / repo-shared layers are pushed in.
//!
//! Strict-undefined semantics for missing keys *inside templates* belong
//! to `MiniJinja` (REQ-009 / T-008); this module returns `None` from
//! [`Resolver::get`] when a name is unset.

pub mod builtins;

pub use builtins::Builtins;
use std::collections::BTreeMap;
use thiserror::Error;

/// Reserved prefix for built-in variable names. Any user-set key starting
/// with this prefix at any non-built-in layer is rejected.
pub const RESERVED_PREFIX: &str = "patina.";

/// Failure modes returned by the variable subsystem.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VariableError {
    /// A user-set key collides with the reserved `patina.*` namespace.
    /// The Display contains the offending key and the substring
    /// `reserved` (REQ-007 done-when).
    #[error("variable key `{key}` is reserved: the `patina.*` namespace is owned by built-ins")]
    ReservedKey {
        /// The offending key.
        key: String,
    },

    /// A CLI override `-v key=value` was missing the `=` separator.
    #[error("invalid CLI override `{raw}`: expected `key=value`")]
    MalformedCliOverride {
        /// The raw `-v` argument that failed to parse.
        raw: String,
    },
}

/// Validate that every key in `layer` is outside the reserved `patina.*`
/// namespace.
///
/// Used by the TOML parser when ingesting a `[variables]` table and by
/// the resolver when accepting CLI / per-machine / per-profile layers.
///
/// # Errors
///
/// Returns [`VariableError::ReservedKey`] naming the first offending key
/// in iteration order.
pub fn reject_reserved_keys<'a, I>(layer: I) -> Result<(), VariableError>
where
    I: IntoIterator<Item = &'a str>,
{
    for key in layer {
        if key.starts_with(RESERVED_PREFIX) {
            return Err(VariableError::ReservedKey {
                key: key.to_owned(),
            });
        }
    }
    Ok(())
}

/// Parse a single `-v key=value` CLI override string into its parts.
///
/// The split happens on the first `=`; values may themselves contain
/// further `=` characters. The key is validated against the reserved
/// namespace.
///
/// # Errors
///
/// Returns [`VariableError::MalformedCliOverride`] when the input lacks
/// an `=`. Returns [`VariableError::ReservedKey`] when the key starts
/// with `patina.`.
pub fn parse_cli_override(raw: &str) -> Result<(String, String), VariableError> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| VariableError::MalformedCliOverride {
            raw: raw.to_owned(),
        })?;
    if key.starts_with(RESERVED_PREFIX) {
        return Err(VariableError::ReservedKey {
            key: key.to_owned(),
        });
    }
    Ok((key.to_owned(), value.to_owned()))
}

/// A flat string→string variable layer.
///
/// `[variables]` tables are TOML, so values may technically be any TOML
/// scalar. v1 narrows the contract to strings — the template engine
/// (T-008) coerces what it renders, and richer value types can be added
/// later without changing this resolver's shape.
type Layer = BTreeMap<String, String>;

/// Composes the six variable layers and answers per-key lookups.
///
/// Construct via [`Resolver::new`], populate the user layers with the
/// `with_*` builders, and resolve with [`Resolver::get`]. Layer order is
/// fixed and matches REQ-007: CLI > per-machine > per-profile >
/// per-module > repo-shared > built-ins.
///
/// Built-ins are owned by the [`Builtins`] field; user keys collide-check
/// against the reserved namespace at ingest, so a successfully built
/// `Resolver` has no `patina.*` keys outside the built-in layer.
#[derive(Debug, Clone)]
#[must_use = "construct then call `.get(...)` to resolve variables"]
pub struct Resolver {
    cli: Layer,
    per_machine: Layer,
    per_profile: Layer,
    per_module: Layer,
    repo_shared: Layer,
    builtins: Builtins,
}

impl Resolver {
    /// Build a resolver whose user layers are empty and whose built-ins
    /// reflect the current process environment.
    pub fn new(builtins: Builtins) -> Self {
        Self {
            cli: Layer::new(),
            per_machine: Layer::new(),
            per_profile: Layer::new(),
            per_module: Layer::new(),
            repo_shared: Layer::new(),
            builtins,
        }
    }

    /// Push the CLI override layer. Keys must not collide with the
    /// reserved `patina.*` namespace.
    ///
    /// # Errors
    ///
    /// Returns [`VariableError::ReservedKey`] when any key starts with
    /// `patina.`.
    pub fn with_cli_overrides<I, K, V>(mut self, overrides: I) -> Result<Self, VariableError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        push_user_layer(&mut self.cli, overrides)?;
        Ok(self)
    }

    /// Push the per-machine variable layer.
    ///
    /// # Errors
    ///
    /// Returns [`VariableError::ReservedKey`] when any key starts with
    /// `patina.`.
    pub fn with_per_machine<I, K, V>(mut self, layer: I) -> Result<Self, VariableError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        push_user_layer(&mut self.per_machine, layer)?;
        Ok(self)
    }

    /// Push the per-profile variable layer.
    ///
    /// # Errors
    ///
    /// Returns [`VariableError::ReservedKey`] when any key starts with
    /// `patina.`.
    pub fn with_per_profile<I, K, V>(mut self, layer: I) -> Result<Self, VariableError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        push_user_layer(&mut self.per_profile, layer)?;
        Ok(self)
    }

    /// Push the per-module variable layer.
    ///
    /// # Errors
    ///
    /// Returns [`VariableError::ReservedKey`] when any key starts with
    /// `patina.`.
    pub fn with_per_module<I, K, V>(mut self, layer: I) -> Result<Self, VariableError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        push_user_layer(&mut self.per_module, layer)?;
        Ok(self)
    }

    /// Push the repo-shared variable layer.
    ///
    /// # Errors
    ///
    /// Returns [`VariableError::ReservedKey`] when any key starts with
    /// `patina.`.
    pub fn with_repo_shared<I, K, V>(mut self, layer: I) -> Result<Self, VariableError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        push_user_layer(&mut self.repo_shared, layer)?;
        Ok(self)
    }

    /// Inject the resolved active profile name into the built-in layer,
    /// marking `patina.profile` as resolved (the no-profile fallback is
    /// the empty string, still resolved).
    ///
    /// Wired this way so the resolver does not depend on profile
    /// resolution being complete before construction; until this is
    /// called, `patina.profile` is undefined (REQ-004 / DEC-010).
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.builtins.profile = Some(profile.into());
        self
    }

    /// Resolve a variable name against the six layers.
    ///
    /// Returns `None` when the name is unset at every layer. Templates
    /// turn that into a strict-undefined error (REQ-009 / T-008); this
    /// resolver is intentionally schema-free at the lookup boundary.
    #[must_use = "ignoring the resolved value defeats the lookup"]
    pub fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.cli.get(key) {
            return Some(v.clone());
        }
        if let Some(v) = self.per_machine.get(key) {
            return Some(v.clone());
        }
        if let Some(v) = self.per_profile.get(key) {
            return Some(v.clone());
        }
        if let Some(v) = self.per_module.get(key) {
            return Some(v.clone());
        }
        if let Some(v) = self.repo_shared.get(key) {
            return Some(v.clone());
        }
        self.builtins.get(key)
    }
}

fn push_user_layer<I, K, V>(target: &mut Layer, layer: I) -> Result<(), VariableError>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    for (key, value) in layer {
        let key = key.into();
        if key.starts_with(RESERVED_PREFIX) {
            return Err(VariableError::ReservedKey { key });
        }
        target.insert(key, value.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_override_shadows_lower_layers() {
        let resolver = Resolver::new(Builtins::for_tests())
            .with_repo_shared([("email", "root@example.com")])
            .expect("repo layer accepted")
            .with_per_module([("email", "module@example.com")])
            .expect("module layer accepted")
            .with_cli_overrides([("email", "cli@example.com")])
            .expect("cli layer accepted");

        assert_eq!(resolver.get("email").as_deref(), Some("cli@example.com"));
    }

    #[test]
    fn per_module_beats_repo_shared() {
        let resolver = Resolver::new(Builtins::for_tests())
            .with_repo_shared([("email", "root@example.com")])
            .expect("repo layer accepted")
            .with_per_module([("email", "module@example.com")])
            .expect("module layer accepted");

        assert_eq!(resolver.get("email").as_deref(), Some("module@example.com"));
    }

    #[test]
    fn missing_keys_return_none() {
        let resolver = Resolver::new(Builtins::for_tests());
        assert!(resolver.get("nonexistent").is_none());
    }

    #[test]
    fn reserved_keys_are_rejected_at_every_user_layer() {
        let builtins = Builtins::for_tests();

        let err = Resolver::new(builtins.clone())
            .with_cli_overrides([("patina.os", "foo")])
            .expect_err("cli override of patina.os must be rejected");
        assert!(matches!(err, VariableError::ReservedKey { ref key } if key == "patina.os"));
        assert!(err.to_string().contains("patina.os"));
        assert!(err.to_string().contains("reserved"));

        Resolver::new(builtins.clone())
            .with_per_machine([("patina.x", "y")])
            .expect_err("per-machine layer must reject patina.*");
        Resolver::new(builtins.clone())
            .with_per_profile([("patina.x", "y")])
            .expect_err("per-profile layer must reject patina.*");
        Resolver::new(builtins.clone())
            .with_per_module([("patina.x", "y")])
            .expect_err("per-module layer must reject patina.*");
        Resolver::new(builtins)
            .with_repo_shared([("patina.x", "y")])
            .expect_err("repo-shared layer must reject patina.*");
    }

    #[test]
    fn reject_reserved_keys_accepts_clean_iterators() {
        reject_reserved_keys(["email", "editor", "shell"])
            .expect("clean key set passes validation");
    }

    #[test]
    fn reject_reserved_keys_flags_offender() {
        let err = reject_reserved_keys(["email", "patina.foo", "shell"])
            .expect_err("reserved key must be rejected");
        assert!(matches!(err, VariableError::ReservedKey { ref key } if key == "patina.foo"));
        let display = err.to_string();
        assert!(display.contains("patina.foo"));
        assert!(display.contains("reserved"));
    }

    #[test]
    fn parse_cli_override_splits_on_first_equals() {
        let (key, value) = parse_cli_override("greeting=hello=world").expect("override parses");
        assert_eq!(key, "greeting");
        assert_eq!(value, "hello=world");
    }

    #[test]
    fn parse_cli_override_rejects_missing_equals() {
        let err = parse_cli_override("just-a-key").expect_err("must require =");
        assert!(
            matches!(err, VariableError::MalformedCliOverride { ref raw } if raw == "just-a-key")
        );
    }

    #[test]
    fn parse_cli_override_rejects_reserved_key() {
        let err = parse_cli_override("patina.os=foo").expect_err("reserved key must be rejected");
        assert!(matches!(err, VariableError::ReservedKey { ref key } if key == "patina.os"));
    }

    #[test]
    fn profile_injection_is_lazy() {
        let resolver = Resolver::new(Builtins::for_tests()).with_profile("work");
        assert_eq!(resolver.get("patina.profile").as_deref(), Some("work"));
    }
}
