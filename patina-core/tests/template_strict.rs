//! Integration tests for template strict.

use patina_core::Builtins;
use patina_core::Resolver;
use patina_core::TemplateEngine;
use patina_core::TemplateError;

fn resolver() -> Resolver {
    Resolver::new(Builtins::for_tests())
}

#[test]
fn template_with_undefined_variable_fails_naming_it() {
    let body = "[user]\nemail = {{ user_email }}";
    let err = TemplateEngine::new()
        .render(body, &resolver())
        .expect_err("strict undefined must fail");
    assert!(
        matches!(&err, TemplateError::UndefinedVariable { names } if names.contains("user_email")),
        "expected UndefinedVariable naming user_email, got {err:?}"
    );
    assert!(
        err.to_string().contains("user_email"),
        "Display `{err}` must name the missing variable"
    );
}

#[test]
fn when_predicate_with_undefined_variable_fails_naming_it() {
    let resolver = resolver();
    let os = resolver.get("patina.os").expect("patina.os resolves");
    let expr = format!("patina.os == '{os}' and missing_var");
    let err = TemplateEngine::new()
        .eval_when(&expr, &resolver)
        .expect_err("strict undefined must fail");
    assert!(
        matches!(&err, TemplateError::UndefinedVariable { names } if names.contains("missing_var")),
        "expected UndefinedVariable naming missing_var, got {err:?}"
    );
    assert!(
        err.to_string().contains("missing_var"),
        "Display `{err}` must name the missing variable"
    );
}

#[test]
fn bare_undefined_when_predicate_is_an_error_not_false() {
    let err = TemplateEngine::new()
        .eval_when("undefined_flag", &resolver())
        .expect_err("a bare undefined predicate must not silently be false");
    assert!(
        matches!(&err, TemplateError::UndefinedVariable { names } if names.contains("undefined_flag")),
        "expected UndefinedVariable naming undefined_flag, got {err:?}"
    );
}

#[test]
fn else_block_undefined_renders_fallback_without_error() {
    let body = "{% if defined %}{{ undefined_var }}{% else %}fallback{% endif %}";
    let out = TemplateEngine::new()
        .render(body, &resolver())
        .expect("else carve-out must not fire strict undefined");
    assert_eq!(out, "fallback");
}

#[test]
fn render_and_when_share_one_environment_instance() {
    let engine = TemplateEngine::new();
    let for_render = engine.clone();
    let for_when = engine.clone();

    let rendered = for_render
        .render("{{ patina.user }}", &resolver())
        .expect("render path works");
    assert_eq!(rendered, "test-user");
    let taken = for_when
        .eval_when("patina.user == 'test-user'", &resolver())
        .expect("when path works");
    assert!(taken);

    assert!(
        std::sync::Arc::ptr_eq(
            &for_render.shared_environment(),
            &for_when.shared_environment()
        ),
        "render and when callers must share one MiniJinja environment"
    );
}

#[test]
fn defined_variable_renders_through() {
    let resolver = resolver()
        .with_repo_shared([("user_email", "kevin@example.com")])
        .expect("repo layer accepted");
    let out = TemplateEngine::new()
        .render("email = {{ user_email }}", &resolver)
        .expect("defined variable renders");
    assert_eq!(out, "email = kevin@example.com");
}

#[test]
fn when_predicate_over_defined_builtins_evaluates() {
    let resolver = resolver();
    let os = resolver.get("patina.os").expect("patina.os resolves");
    let matching = format!("patina.os == '{os}'");
    assert!(
        TemplateEngine::new()
            .eval_when(&matching, &resolver)
            .expect("matching predicate evaluates")
    );
    let non_matching = "patina.os == 'definitely-not-an-os'";
    assert!(
        !TemplateEngine::new()
            .eval_when(non_matching, &resolver)
            .expect("non-matching predicate evaluates to false")
    );
}
