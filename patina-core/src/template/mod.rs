//! Single strict-undefined `MiniJinja` environment for `.tmpl` rendering
//! and `when` predicate evaluation (REQ-009).
//!
//! One [`minijinja::Environment`], configured with
//! [`UndefinedBehavior::SemiStrict`](minijinja::UndefinedBehavior) (see
//! the "Why `SemiStrict`" section below), serves both callers:
//!
//! - `*.tmpl` file rendering (the executor in T-014 calls [`Engine::render`]).
//! - `when` expression evaluation on `[[file]]` and `[[hook]]` entries (the
//!   plan/pipeline in T-014 / T-015 calls [`Engine::eval_when`]).
//!
//! Both methods take the same [`minijinja::Value`] context built once per
//! apply from the six-layer [`Resolver`] (see
//! `strict::build_context`). The environment instance is held behind an
//! [`Arc`] so the engine constructor wiring can share exactly one copy
//! across every template and predicate in an apply.
//!
//! # Strict-undefined contract
//!
//! A reference to a variable that is unset at every resolution layer
//! produces an [`EngineError::Template`](crate::EngineError) whose
//! `Display` names the missing variable, rather than `MiniJinja`'s
//! silent empty-string substitution. The Jinja2-inherited carve-out
//! holds: an undefined value reached only through the unevaluated branch
//! of an `{% if %}`/`{% else %}` block does not fire, so
//! `{% if defined %}{{ x }}{% else %}fallback{% endif %}` renders
//! `fallback` when `defined` is unset.
//!
//! # Why `SemiStrict`, not `Strict`
//!
//! `MiniJinja`'s [`UndefinedBehavior::Strict`] errors the moment an
//! undefined value is *tested* — including the condition of an
//! `{% if defined %}` block — which would break the REQ-009 `{% else %}`
//! carve-out. [`UndefinedBehavior::SemiStrict`] is the exact behaviour
//! the SPEC's assumption documents: an undefined value still errors when
//! it is *emitted* (`{{ user_email }}`) or coerced into a concrete type,
//! but a bare `{% if missing %}` test treats the undefined as falsy and
//! falls through to `{% else %}`. For `when` predicates,
//! `compile_expression().eval()` returns an undefined *value* rather than
//! an error when the expression resolves to undefined (e.g. the
//! short-circuit result of `true and missing_var`); [`Engine::eval_when`]
//! converts that undefined result into the same typed error so a `when`
//! that touches an undefined variable is reported, not silently treated
//! as false.
//!
//! # Examples
//!
//! ```
//! use patina_core::Builtins;
//! use patina_core::Resolver;
//! use patina_core::template::Engine;
//!
//! let resolver = Resolver::new(Builtins::for_tests())
//!     .with_repo_shared([("name", "patina")])?;
//! let engine = Engine::new();
//! let rendered = engine.render("hello {{ name }}", &resolver)?;
//! assert_eq!(rendered, "hello patina");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod strict;

use crate::variables::Resolver;
use minijinja::Environment;
use minijinja::ErrorKind;
use minijinja::UndefinedBehavior;
use minijinja::Value;
use std::sync::Arc;
use thiserror::Error;

/// Synthetic template name used when rendering an in-memory `.tmpl`
/// body. `MiniJinja` requires a name for `render_named_str`; the name is
/// internal and never user-visible.
const TEMPLATE_NAME: &str = "<patina-template>";

/// Failure modes returned by the template subsystem (REQ-009).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TemplateError {
    /// A template body or `when` predicate referenced a variable that is
    /// undefined at every resolution layer. The `Display` names the
    /// offending variable(s) so the user can fix the source (REQ-009
    /// done-when).
    #[error("undefined variable(s) referenced: {names}")]
    UndefinedVariable {
        /// Comma-separated list of the undefined variable names, sorted
        /// for deterministic output.
        names: String,
    },

    /// The template body or `when` expression failed to compile or
    /// evaluate for a reason other than an undefined variable (syntax
    /// error, type error in an operation, …).
    #[error("template evaluation failed: {message}")]
    Render {
        /// The underlying `MiniJinja` error rendered to a string.
        message: String,
    },
}

/// Wraps the single shared strict-undefined `MiniJinja` environment.
///
/// Construct once per apply via [`Engine::new`] and clone freely — the
/// inner [`Environment`] lives behind an [`Arc`], so every clone shares
/// the same instance (the property CHK verifies for REQ-009).
#[derive(Debug, Clone)]
#[must_use = "construct the engine then call `render` / `eval_when`"]
pub struct Engine {
    env: Arc<Environment<'static>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// Build the engine with one [`UndefinedBehavior::SemiStrict`]
    /// environment (see the module docs for why `SemiStrict` rather than
    /// `Strict`).
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::SemiStrict);
        Self { env: Arc::new(env) }
    }

    /// The shared environment handle. Two [`Engine`] clones return
    /// [`Arc`] pointers to the same allocation — used to prove the
    /// single-instance wiring property.
    #[must_use = "inspect the shared environment handle"]
    pub fn shared_environment(&self) -> Arc<Environment<'static>> {
        Arc::clone(&self.env)
    }

    /// Render a `.tmpl` body against the resolved variable context.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UndefinedVariable`] when the body
    /// references a variable undefined at every resolution layer (under
    /// strict-undefined semantics), naming the offending variable.
    /// Returns [`TemplateError::Render`] for syntax or evaluation errors
    /// unrelated to undefined variables.
    pub fn render(&self, body: &str, resolver: &Resolver) -> Result<String, TemplateError> {
        let (context, tracker) = strict::build_context(resolver);
        match self.env.render_named_str(TEMPLATE_NAME, body, context) {
            Ok(rendered) => Ok(rendered),
            Err(err) => Err(classify(&err, &tracker)),
        }
    }

    /// Evaluate a `when` predicate against the resolved variable context
    /// and return its truthiness.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UndefinedVariable`] when the predicate
    /// references a variable undefined at every resolution layer,
    /// naming the offending variable. Returns [`TemplateError::Render`]
    /// for syntax or evaluation errors unrelated to undefined variables.
    pub fn eval_when(&self, expr: &str, resolver: &Resolver) -> Result<bool, TemplateError> {
        let (context, tracker) = strict::build_context(resolver);
        let compiled = self
            .env
            .compile_expression(expr)
            .map_err(|err| classify(&err, &tracker))?;
        match compiled.eval(context) {
            Ok(value) => coerce_when_result(&value, &tracker),
            Err(err) => Err(classify(&err, &tracker)),
        }
    }
}

/// Coerce a `when` expression's evaluated [`Value`] into a boolean.
///
/// Under `SemiStrict`, an expression that resolves to an undefined value
/// (a bare `missing_var`, or the short-circuit result of
/// `true and missing_var`) returns `Ok(undefined)` rather than erroring.
/// REQ-009 requires such a `when` to be reported as an undefined-variable
/// error naming the offending variable — not silently treated as false.
/// A defined result is returned as its truthiness.
fn coerce_when_result(
    value: &Value,
    tracker: &std::sync::Mutex<strict::UndefinedTracker>,
) -> Result<bool, TemplateError> {
    if value.is_undefined() {
        let names = tracker.lock().map(|t| t.names()).unwrap_or_default();
        if !names.is_empty() {
            return Err(TemplateError::UndefinedVariable {
                names: names.join(", "),
            });
        }
        // Undefined with no recorded name should not happen (every miss
        // routes through the tracking context), but fail closed rather
        // than silently treating an undefined predicate as false.
        return Err(TemplateError::Render {
            message: "`when` expression evaluated to an undefined value".to_owned(),
        });
    }
    Ok(value.is_true())
}

/// Map a `MiniJinja` error to a [`TemplateError`].
///
/// An [`ErrorKind::UndefinedError`] is reported as
/// [`TemplateError::UndefinedVariable`] naming the variables the context
/// recorded as missing during this evaluation. Any other error falls
/// through to [`TemplateError::Render`].
fn classify(
    err: &minijinja::Error,
    tracker: &std::sync::Mutex<strict::UndefinedTracker>,
) -> TemplateError {
    if err.kind() == ErrorKind::UndefinedError {
        let names = tracker.lock().map(|t| t.names()).unwrap_or_default();
        if !names.is_empty() {
            return TemplateError::UndefinedVariable {
                names: names.join(", "),
            };
        }
    }
    TemplateError::Render {
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variables::Builtins;

    fn resolver() -> Resolver {
        Resolver::new(Builtins::for_tests())
    }

    #[test]
    fn render_substitutes_a_defined_variable() {
        let resolver = resolver()
            .with_repo_shared([("name", "patina")])
            .expect("layer accepted");
        let out = Engine::new()
            .render("hello {{ name }}", &resolver)
            .expect("render succeeds");
        assert_eq!(out, "hello patina");
    }

    #[test]
    fn render_undefined_variable_names_it() {
        let err = Engine::new()
            .render("email = {{ user_email }}", &resolver())
            .expect_err("strict undefined must fail");
        assert!(
            matches!(&err, TemplateError::UndefinedVariable { names } if names.contains("user_email")),
            "expected UndefinedVariable naming user_email, got {err:?}"
        );
    }

    #[test]
    fn eval_when_true_for_matching_builtin() {
        let resolver = resolver();
        let os = resolver.get("patina.os").expect("os resolves");
        let expr = format!("patina.os == '{os}'");
        let result = Engine::new()
            .eval_when(&expr, &resolver)
            .expect("eval succeeds");
        assert!(result);
    }

    #[test]
    fn eval_when_undefined_variable_names_it() {
        // CHK-020 short-circuits `missing_var` unless the left operand is
        // true. Build a left operand that is always true on the test host
        // (`patina.os == <this host's os>`) so the predicate genuinely
        // reaches the undefined operand regardless of where tests run.
        let resolver = resolver();
        let os = resolver.get("patina.os").expect("patina.os resolves");
        let expr = format!("patina.os == '{os}' and missing_var");
        let err = Engine::new()
            .eval_when(&expr, &resolver)
            .expect_err("strict undefined must fail");
        assert!(
            matches!(&err, TemplateError::UndefinedVariable { names } if names.contains("missing_var")),
            "expected UndefinedVariable naming missing_var, got {err:?}"
        );
    }

    #[test]
    fn else_block_carveout_renders_fallback() {
        let out = Engine::new()
            .render(
                "{% if defined %}{{ undefined_var }}{% else %}fallback{% endif %}",
                &resolver(),
            )
            .expect("else carve-out must not fire strict undefined");
        assert_eq!(out, "fallback");
    }

    #[test]
    fn clones_share_one_environment_instance() {
        let engine = Engine::new();
        let clone = engine.clone();
        assert!(Arc::ptr_eq(
            &engine.shared_environment(),
            &clone.shared_environment()
        ));
    }
}
