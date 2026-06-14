#![expect(
    clippy::expect_used,
    reason = "integration tests use .expect() on fixture setup; the lint's allow-expect-in-tests covers #[cfg(test)] modules but not the helper functions in tests/*.rs integration crates."
)]

//! Integration coverage for active-profile resolution.
//!
//! The end-to-end scenarios are described in terms of
//! `patina apply --yes --json` JSON output, but the clap-derived CLI
//! surface and the `apply` plan computation don't land yet. The engine's
//! contract is the resolution
//! function and the auto-match parse path — these tests exercise that
//! contract directly so the per-source priority order, the shared-engine
//! `when` evaluation, and the root-`patina.toml` parser cannot
//! regress before the CLI is wired in.

use camino::Utf8PathBuf;
use patina_core::profile::PERSISTED_PROFILE_FILE;
use patina_core::profile::ProfileSource;
use patina_core::profile::load_auto_match_rules;
use patina_core::profile::resolve;
use patina_core::template::Engine;
use patina_core::variables::Builtins;
use tempfile::TempDir;

fn persisted_path(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().join(PERSISTED_PROFILE_FILE))
        .expect("tempdir path is utf-8")
}

fn root_manifest_path(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().join("patina.toml")).expect("tempdir path is utf-8")
}

/// With `PATINA_PROFILE=work`, no `[[auto_match]]` rules, and
/// no persisted choice, the resolved profile is `work`.
#[test]
fn env_var_resolves_to_work_when_no_other_sources_match() {
    let dir = TempDir::new().expect("tempdir");
    let persisted = persisted_path(&dir);
    let root = root_manifest_path(&dir);

    let rules = load_auto_match_rules(&root).expect("no root manifest");
    assert!(rules.is_empty(), "no auto-match rules expected");

    let builtins = Builtins::for_tests();
    let resolution = resolve(
        Some("work".to_owned()),
        &persisted,
        &rules,
        &builtins,
        &Engine::new(),
    )
    .expect("env-var resolution succeeds");

    assert_eq!(resolution.name, "work");
    assert_eq!(resolution.source, ProfileSource::Env);
}

/// With `PATINA_PROFILE` unset, no persisted choice, and a
/// root `patina.toml` declaring
/// `[[auto_match]] when = "patina.hostname == 'CHK-host'"
/// profile = "desktop"` against a host whose hostname is `CHK-host`,
/// the resolved profile is `desktop`.
///
/// The harness pins `Builtins::hostname` to `CHK-host` directly (the
/// public field is mutable; `Builtins::current`
/// reads `$HOSTNAME` from the process environment, which is not
/// reliably exported on Unix). This isolates the test from the host's
/// real hostname.
#[test]
fn auto_match_on_hostname_resolves_to_desktop() {
    let dir = TempDir::new().expect("tempdir");
    let persisted = persisted_path(&dir);
    let root = root_manifest_path(&dir);

    fs_err::write(
        root.as_std_path(),
        "[[auto_match]]\nwhen = \"patina.hostname == 'CHK-host'\"\nprofile = \"desktop\"\n",
    )
    .expect("write root patina.toml");

    let rules = load_auto_match_rules(&root).expect("parse rules");
    assert_eq!(rules.len(), 1);

    let mut builtins = Builtins::for_tests();
    builtins.hostname = "CHK-host".to_owned();

    let resolution = resolve(None, &persisted, &rules, &builtins, &Engine::new())
        .expect("auto-match resolution succeeds");
    assert_eq!(resolution.name, "desktop");
    assert_eq!(resolution.source, ProfileSource::AutoMatch);
}

/// Done-when (b): with `PATINA_PROFILE` unset, no auto-match rules,
/// and a persisted choice of `home`, the resolved profile is `home`.
#[test]
fn persisted_choice_resolves_to_home_when_env_unset() {
    let dir = TempDir::new().expect("tempdir");
    let persisted = persisted_path(&dir);
    fs_err::write(persisted.as_std_path(), "home\n").expect("write persisted profile");
    let builtins = Builtins::for_tests();

    let resolution = resolve(None, &persisted, &[], &builtins, &Engine::new())
        .expect("persisted resolution succeeds");
    assert_eq!(resolution.name, "home");
    assert_eq!(resolution.source, ProfileSource::Persisted);
}

/// Done-when (d): with all three higher sources absent or non-matching,
/// the resolved profile is the empty string and the source is the
/// no-profile fallback.
#[test]
fn fallback_resolves_to_empty_when_nothing_matches() {
    let dir = TempDir::new().expect("tempdir");
    let persisted = persisted_path(&dir);
    let root = root_manifest_path(&dir);

    // Root manifest has a rule but it does not match the test host.
    fs_err::write(
        root.as_std_path(),
        "[[auto_match]]\nwhen = \"patina.hostname == 'nope-host'\"\nprofile = \"never\"\n",
    )
    .expect("write root patina.toml");

    let rules = load_auto_match_rules(&root).expect("parse rules");
    let builtins = Builtins::for_tests();

    let resolution =
        resolve(None, &persisted, &rules, &builtins, &Engine::new()).expect("fallback succeeds");
    assert_eq!(resolution.name, "");
    assert_eq!(resolution.source, ProfileSource::Fallback);
}
