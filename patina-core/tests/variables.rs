//! Integration scenarios for REQ-007 / T-006: layered variable
//! resolution and the reserved `patina.*` namespace.
//!
//! These tests exercise the public surface of `patina_core::variables`
//! and the parse-time reservation hook in `patina_core::config`.

use patina_core::Builtins;
use patina_core::Resolver;
use patina_core::VariableError;
use patina_core::config::ConfigParseError;
use patina_core::config::parse_module_config_str;

/// CHK-015 — CLI override shadows per-module and repo-shared layers.
#[test]
fn cli_override_shadows_module_and_repo_for_email_lookup() {
    let resolver = Resolver::new(Builtins::for_tests())
        .with_repo_shared([("email", "root@example.com")])
        .expect("repo layer accepted")
        .with_per_module([("email", "module@example.com")])
        .expect("module layer accepted")
        .with_cli_overrides([("email", "cli@example.com")])
        .expect("cli layer accepted");

    assert_eq!(resolver.get("email").as_deref(), Some("cli@example.com"));
}

/// CHK-016 — `[variables]` declaring `patina.foo = "bar"` fails parse
/// with a typed error whose Display names the key and contains the
/// substring `reserved`.
#[test]
fn parsing_a_patina_toml_with_reserved_variable_key_fails() {
    let manifest = r#"
[variables]
"patina.foo" = "bar"
"#;
    let err = parse_module_config_str(manifest).expect_err("parse must reject reserved key");
    assert!(
        matches!(
            err,
            ConfigParseError::Variable(VariableError::ReservedKey { ref key })
                if key == "patina.foo"
        ),
        "expected ConfigParseError::Variable(ReservedKey {{ key: \"patina.foo\" }}), got {err:?}"
    );
    let display = err.to_string();
    assert!(
        display.contains("patina.foo"),
        "display `{display}` missing key"
    );
    assert!(
        display.contains("reserved"),
        "display `{display}` missing substring `reserved`"
    );
}

/// REQ-007 done-when — CLI `-v patina.os=foo` is rejected.
#[test]
fn cli_override_of_reserved_key_is_rejected() {
    let err = Resolver::new(Builtins::for_tests())
        .with_cli_overrides([("patina.os", "foo")])
        .expect_err("must reject CLI override of patina.os");
    let display = err.to_string();
    assert!(matches!(err, VariableError::ReservedKey { ref key } if key == "patina.os"));
    assert!(display.contains("patina.os"));
    assert!(display.contains("reserved"));
}

/// CHK-040 / REQ-007 done-when — `patina.env.FOO` resolves to the value
/// of `FOO` in the current process environment. Tests forbid mutating
/// `std::env` under `unsafe_code = "forbid"`, so we exercise the path
/// via `PATH`, which is reliably set on every host that runs the
/// workspace tests.
#[test]
fn patina_env_lookup_reads_process_environment() {
    let resolver = Resolver::new(Builtins::current());
    let direct = std::env::var("PATH").ok();
    let via_resolver = resolver.get("patina.env.PATH");
    assert_eq!(
        via_resolver, direct,
        "patina.env.PATH must mirror the process environment exactly"
    );
}

/// `patina.env.FOO` returns `None` when `FOO` is unset; downstream
/// strict-undefined handling lives in `MiniJinja` (T-008 / REQ-009).
#[test]
fn patina_env_unset_variable_resolves_to_none() {
    let resolver = Resolver::new(Builtins::current());
    assert!(
        resolver
            .get("patina.env.PATINA_DEFINITELY_UNSET_INTEGRATION_T006")
            .is_none()
    );
}

/// REQ-007 done-when — `patina.os` resolves to one of the three v1
/// platform strings on the supported hosts.
#[test]
fn patina_os_is_one_of_the_v1_platform_strings() {
    let resolver = Resolver::new(Builtins::current());
    let os = resolver
        .get("patina.os")
        .expect("patina.os must always resolve");
    if matches!(std::env::consts::OS, "macos" | "linux" | "windows") {
        assert!(
            matches!(os.as_str(), "macos" | "linux" | "windows"),
            "patina.os = `{os}` outside the v1 set"
        );
        assert_eq!(os, std::env::consts::OS);
    } else {
        // Non-{macOS, Linux, Windows} hosts (BSDs) report through
        // unchanged. Just assert it is non-empty.
        assert!(!os.is_empty());
    }
}

/// Profile injection is lazy: T-007 wires the resolved profile in via
/// [`Resolver::with_profile`] without forcing this task to depend on
/// T-007's wiring being complete.
#[test]
fn profile_injection_round_trips() {
    let resolver = Resolver::new(Builtins::for_tests()).with_profile("desktop");
    assert_eq!(resolver.get("patina.profile").as_deref(), Some("desktop"));
}

/// Lower-precedence layers still surface when no higher layer sets the
/// key — exercises the per-module → repo-shared fall-through.
#[test]
fn lookup_falls_through_to_lower_layers() {
    let resolver = Resolver::new(Builtins::for_tests())
        .with_repo_shared([("shell", "zsh")])
        .expect("repo layer accepted");
    assert_eq!(resolver.get("shell").as_deref(), Some("zsh"));
}
