//! Active-profile resolution.
//!
//! The engine resolves a single profile name by composing four sources
//! in priority order from highest to lowest:
//!
//! 1. **`PATINA_PROFILE` environment variable.** A non-empty value is accepted
//!    verbatim; an unset or empty value falls through.
//! 2. **Persisted choice.** The first non-empty trimmed line of
//!    `<state>/patina/profile`, when that file exists and is readable. The
//!    per-machine state directory is resolved by [`state_dir`].
//! 3. **`[[auto_match]]` rules** declared in the root `patina.toml`, evaluated
//!    in declaration order against the built-in variable context. The first
//!    rule whose `when` predicate matches wins; its `profile` field is the
//!    resolved profile.
//! 4. **No-profile fallback.** An empty string. Profile-scoped variables and
//!    modules contribute nothing.
//!
//! There is no `--profile` CLI flag and no plan to add one. Profile
//! choice is intentionally environment-driven so the same dotfiles
//! source produces the same profile on the same machine regardless of
//! who invokes `patina apply`.
//!
//! # Predicate evaluation
//!
//! `[[auto_match]]` `when` predicates are evaluated by the shared
//! `MiniJinja` [`Engine`] — the same evaluator
//! `[[file]]` / `[[directory]]` / `[[hook]]` `when` predicates and
//! `*.tmpl` bodies use. One grammar, one
//! strict-undefined behavior, one place to reason about predicates. The
//! full `MiniJinja` expression grammar is available: equality, inequality
//! (`!=`), `and` / `or`, and expressions over the `patina.*` built-in
//! surface (`patina.os` / `patina.arch` / `patina.hostname` /
//! `patina.user` / `patina.home` / `patina.env.*`).
//!
//! Profile resolution runs *before* the user variable layers (CLI,
//! per-machine, per-profile, per-module, repo-shared) are assembled, so
//! the predicate is evaluated against a built-ins-only
//! [`Resolver`]. A predicate that
//! accesses a variable absent from that context — including a
//! user-defined variable, or `patina.profile` (which is precisely what
//! this resolution computes and so cannot be read here) — is a typed
//! [`ProfileError::Predicate`] naming the offending variable,
//! not a silent non-match. The undefined-access error and the
//! short-circuit carve-out (a variable on a not-taken `and` / `or`
//! branch is never accessed and does not error) fall out of routing
//! through the shared engine; this module adds no evaluator of its own.
//!
//! [`state_dir`]: crate::state_dir

use crate::template::Engine;
use crate::variables::Builtins;
use crate::variables::Resolver;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use thiserror::Error;

/// Filename under the per-machine state directory holding the persisted
/// profile choice. Owned by this module per the state-directory layout
/// note in `state_dir`.
pub const PERSISTED_PROFILE_FILE: &str = "profile";

/// Which source supplied the resolved profile.
///
/// Returned alongside the resolved name so callers (apply / status /
/// JSON output) can log or render which layer the value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    /// `PATINA_PROFILE` environment variable.
    Env,
    /// First non-empty trimmed line of `<state>/patina/profile`.
    Persisted,
    /// A matching `[[auto_match]]` rule in the root `patina.toml`.
    AutoMatch,
    /// No source matched; profile name is the empty string.
    Fallback,
}

/// Resolved active profile.
///
/// `name` is the empty string iff `source == ProfileSource::Fallback`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "the resolved profile is consumed by the variable resolver"]
pub struct Resolution {
    /// Resolved profile name, or the empty string for the no-profile
    /// fallback.
    pub name: String,
    /// Source layer that supplied [`Self::name`].
    pub source: ProfileSource,
}

/// One `[[auto_match]]` rule parsed from the root `patina.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct AutoMatchRule {
    /// Predicate evaluated against the built-in variable context. See
    /// the module-level docs for the supported shape.
    pub when: String,
    /// Profile name selected when [`Self::when`] evaluates true.
    pub profile: String,
}

/// Failure modes returned by [`resolve`] and [`load_auto_match_rules`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfileError {
    /// Reading the persisted-profile file failed for a reason other
    /// than `NotFound` (which is treated as "no persisted choice" and
    /// falls through to the next source).
    #[error("failed to read persisted profile file {path}: {source}")]
    PersistedRead {
        /// Path that failed to read.
        path: Utf8PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// Reading the root `patina.toml` failed.
    #[error("failed to read root patina.toml at {path}: {source}")]
    RootRead {
        /// Path that failed to read.
        path: Utf8PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// The root `patina.toml` did not deserialize.
    #[error("failed to parse root patina.toml at {path}: {source}")]
    RootParse {
        /// Path whose TOML failed to parse.
        path: Utf8PathBuf,
        /// Underlying TOML error.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// Evaluating an `[[auto_match]]` rule's `when` predicate through the
    /// shared `MiniJinja` engine failed — a syntax error, a type error,
    /// or (most commonly) a reference to a variable undefined in the
    /// built-ins-only context profile resolution runs against.
    /// The wrapped [`TemplateError`](crate::template::TemplateError)
    /// names the offending variable.
    #[error("[[auto_match]] when predicate `{predicate}` failed to evaluate")]
    Predicate {
        /// The offending `when` text.
        predicate: String,
        /// Underlying engine evaluation error (names the undefined
        /// variable for an undefined-access failure).
        #[source]
        source: crate::template::TemplateError,
    },
}

/// Resolve the active profile by walking the four sources.
///
/// Each source is consulted in priority order; the first that produces
/// a non-empty profile name wins. Sources are wired through arguments
/// rather than re-resolved internally so the function is trivially
/// testable: the integration in `apply` / `status` / `rollback` passes
/// `std::env::var("PATINA_PROFILE").ok()`, the resolved state-directory
/// path, the parsed auto-match rules, and [`Builtins::current`].
///
/// # Arguments
///
/// * `env_value` — `Some(value)` when `PATINA_PROFILE` is set in the process
///   environment, `None` when unset. An empty `value` is treated the same as
///   `None`.
/// * `persisted_path` — absolute path to `<state>/patina/profile`. `NotFound`
///   is silent fall-through; other IO errors surface as
///   [`ProfileError::PersistedRead`].
/// * `auto_match_rules` — rules parsed from the root `patina.toml`, in
///   declaration order. The first whose `when` matches wins.
/// * `builtins` — built-in variable context (`patina.os`, `patina.hostname`, …)
///   the predicate evaluates against. Profile resolution runs before the user
///   variable layers are assembled, so the predicate sees a built-ins-only
///   [`Resolver`].
/// * `engine` — the shared `MiniJinja` [`Engine`] that evaluates every `when`
///   site.
///
/// # Errors
///
/// Returns [`ProfileError::PersistedRead`] when the persisted-profile
/// file exists but cannot be read, and [`ProfileError::Predicate`] when
/// an `[[auto_match]]` rule's `when` expression fails to evaluate —
/// notably when it accesses a variable undefined in the built-ins-only
/// context (a misspelled built-in, a user-defined variable, or
/// `patina.profile`), which is a typed error naming the variable rather
/// than a silent non-match. When predicates evaluate cleanly
/// to `false`, the function continues to the next rule and ultimately to
/// the fallback.
///
/// # Examples
///
/// ```
/// use patina_core::profile::{resolve, ProfileSource};
/// use patina_core::variables::Builtins;
/// use patina_core::template::Engine;
/// use camino::Utf8PathBuf;
///
/// let builtins = Builtins::for_tests();
/// let engine = Engine::new();
/// let resolution = resolve(
///     Some("work".to_owned()),
///     &Utf8PathBuf::from("/nonexistent/profile"),
///     &[],
///     &builtins,
///     &engine,
/// )?;
/// assert_eq!(resolution.name, "work");
/// assert_eq!(resolution.source, ProfileSource::Env);
/// # Ok::<(), patina_core::profile::ProfileError>(())
/// ```
pub fn resolve(
    env_value: Option<String>,
    persisted_path: &Utf8Path,
    auto_match_rules: &[AutoMatchRule],
    builtins: &Builtins,
    engine: &Engine,
) -> Result<Resolution, ProfileError> {
    // 1. Environment variable.
    if let Some(value) = env_value
        && !value.is_empty()
    {
        return Ok(Resolution {
            name: value,
            source: ProfileSource::Env,
        });
    }

    // 2. Persisted choice.
    match fs_err::read_to_string(persisted_path.as_std_path()) {
        Ok(text) => {
            if let Some(line) = first_non_empty_line(&text) {
                return Ok(Resolution {
                    name: line.to_owned(),
                    source: ProfileSource::Persisted,
                });
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // No persisted choice; fall through.
        }
        Err(source) => {
            return Err(ProfileError::PersistedRead {
                path: persisted_path.to_path_buf(),
                source,
            });
        }
    }

    // 3. Auto-match rules. Each `when` is evaluated through the shared
    // engine against a built-ins-only resolver: profile resolution runs
    // before the user variable layers are assembled, so no active-profile
    // and no user layers are in scope. A `Resolver` with no
    // layers pushed exposes exactly the built-ins.
    let resolver = Resolver::new(builtins.clone());
    for rule in auto_match_rules {
        let matched =
            engine
                .eval_when(&rule.when, &resolver)
                .map_err(|source| ProfileError::Predicate {
                    predicate: rule.when.clone(),
                    source,
                })?;
        if matched {
            return Ok(Resolution {
                name: rule.profile.clone(),
                source: ProfileSource::AutoMatch,
            });
        }
    }

    // 4. Fallback.
    Ok(Resolution {
        name: String::new(),
        source: ProfileSource::Fallback,
    })
}

/// Parse the `[[auto_match]]` table-array from the root `patina.toml`
/// at `path`. A missing file or a missing/empty `[[auto_match]]`
/// section returns an empty vector.
///
/// Only the root manifest carries `[[auto_match]]`; module manifests
/// (parsed by [`crate::config::parse_module_config`]) ignore the
/// section.
///
/// # Errors
///
/// Returns [`ProfileError::RootRead`] on IO failure (other than
/// `NotFound`, which is silent), and [`ProfileError::RootParse`] when
/// the TOML document fails to deserialize.
pub fn load_auto_match_rules(path: &Utf8Path) -> Result<Vec<AutoMatchRule>, ProfileError> {
    let text = match fs_err::read_to_string(path.as_std_path()) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ProfileError::RootRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse_auto_match_rules_str(&text).map_err(|source| ProfileError::RootParse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// Parse the `[[auto_match]]` table-array from an in-memory string.
fn parse_auto_match_rules_str(text: &str) -> Result<Vec<AutoMatchRule>, toml::de::Error> {
    #[derive(Deserialize)]
    struct RawRoot {
        #[serde(default)]
        auto_match: Vec<AutoMatchRule>,
    }
    let raw: RawRoot = toml::from_str(text)?;
    Ok(raw.auto_match)
}

fn first_non_empty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_persisted(dir: &TempDir, contents: &str) -> Utf8PathBuf {
        let path = Utf8PathBuf::from_path_buf(dir.path().join(PERSISTED_PROFILE_FILE))
            .expect("tempdir path is utf-8");
        fs_err::write(path.as_std_path(), contents).expect("write persisted profile");
        path
    }

    fn engine() -> Engine {
        Engine::new()
    }

    #[test]
    fn env_var_wins_when_set() {
        let dir = TempDir::new().expect("tempdir");
        let persisted = write_persisted(&dir, "home\n");
        let builtins = Builtins::for_tests();

        let resolution = resolve(
            Some("work".to_owned()),
            &persisted,
            &[AutoMatchRule {
                when: "patina.hostname == 'test-host'".to_owned(),
                profile: "desktop".to_owned(),
            }],
            &builtins,
            &engine(),
        )
        .expect("env wins");

        assert_eq!(resolution.name, "work");
        assert_eq!(resolution.source, ProfileSource::Env);
    }

    #[test]
    fn empty_env_value_falls_through() {
        let dir = TempDir::new().expect("tempdir");
        let persisted = write_persisted(&dir, "home\n");
        let builtins = Builtins::for_tests();

        let resolution = resolve(Some(String::new()), &persisted, &[], &builtins, &engine())
            .expect("falls through");

        assert_eq!(resolution.name, "home");
        assert_eq!(resolution.source, ProfileSource::Persisted);
    }

    #[test]
    fn persisted_file_resolves_when_env_unset() {
        let dir = TempDir::new().expect("tempdir");
        let persisted = write_persisted(&dir, "  home  \n");
        let builtins = Builtins::for_tests();

        let resolution =
            resolve(None, &persisted, &[], &builtins, &engine()).expect("persisted wins");

        assert_eq!(resolution.name, "home");
        assert_eq!(resolution.source, ProfileSource::Persisted);
    }

    #[test]
    fn missing_persisted_file_silently_falls_through() {
        let dir = TempDir::new().expect("tempdir");
        let persisted =
            Utf8PathBuf::from_path_buf(dir.path().join("profile")).expect("tempdir path is utf-8");
        let builtins = Builtins::for_tests();

        let resolution =
            resolve(None, &persisted, &[], &builtins, &engine()).expect("falls through");

        assert_eq!(resolution.name, "");
        assert_eq!(resolution.source, ProfileSource::Fallback);
    }

    #[test]
    fn empty_persisted_file_falls_through() {
        let dir = TempDir::new().expect("tempdir");
        let persisted = write_persisted(&dir, "   \n\n");
        let builtins = Builtins::for_tests();

        let resolution =
            resolve(None, &persisted, &[], &builtins, &engine()).expect("falls through");

        assert_eq!(resolution.source, ProfileSource::Fallback);
        assert_eq!(resolution.name, "");
    }

    #[test]
    fn auto_match_first_rule_wins() {
        let dir = TempDir::new().expect("tempdir");
        let persisted =
            Utf8PathBuf::from_path_buf(dir.path().join("profile")).expect("tempdir path is utf-8");
        let mut builtins = Builtins::for_tests();
        builtins.hostname = "tower".to_owned();

        let rules = vec![
            AutoMatchRule {
                when: "patina.hostname == 'laptop'".to_owned(),
                profile: "mobile".to_owned(),
            },
            AutoMatchRule {
                when: "patina.hostname == 'tower'".to_owned(),
                profile: "desktop".to_owned(),
            },
            AutoMatchRule {
                when: "patina.hostname == 'tower'".to_owned(),
                profile: "should-not-win".to_owned(),
            },
        ];

        let resolution =
            resolve(None, &persisted, &rules, &builtins, &engine()).expect("auto-match");
        assert_eq!(resolution.name, "desktop");
        assert_eq!(resolution.source, ProfileSource::AutoMatch);
    }

    #[test]
    fn auto_match_no_rules_match_falls_through_to_fallback() {
        let dir = TempDir::new().expect("tempdir");
        let persisted =
            Utf8PathBuf::from_path_buf(dir.path().join("profile")).expect("tempdir path is utf-8");
        let builtins = Builtins::for_tests();

        let rules = vec![AutoMatchRule {
            when: "patina.hostname == 'nope'".to_owned(),
            profile: "desktop".to_owned(),
        }];

        let resolution = resolve(None, &persisted, &rules, &builtins, &engine()).expect("fallback");
        assert_eq!(resolution.name, "");
        assert_eq!(resolution.source, ProfileSource::Fallback);
    }

    /// The shared engine accepts the wider grammar the removed narrow
    /// evaluator rejected: an `!=` predicate over a defined built-in
    /// selects its profile rather than erroring (parity-plus-wider-grammar;
    /// replaces `predicate_rejects_unsupported_shape`).
    #[test]
    fn auto_match_inequality_predicate_now_selects() {
        let dir = TempDir::new().expect("tempdir");
        let persisted =
            Utf8PathBuf::from_path_buf(dir.path().join("profile")).expect("tempdir path is utf-8");
        let mut builtins = Builtins::for_tests();
        builtins.hostname = "tower".to_owned();

        let rules = vec![AutoMatchRule {
            when: "patina.hostname != 'laptop'".to_owned(),
            profile: "desktop".to_owned(),
        }];

        let resolution =
            resolve(None, &persisted, &rules, &builtins, &engine()).expect("!= now evaluates");
        assert_eq!(resolution.name, "desktop");
        assert_eq!(resolution.source, ProfileSource::AutoMatch);
    }

    /// An `or` of two equalities — previously rejected by the narrow
    /// evaluator — now evaluates and selects when either disjunct holds.
    #[test]
    fn auto_match_or_predicate_now_selects() {
        let dir = TempDir::new().expect("tempdir");
        let persisted =
            Utf8PathBuf::from_path_buf(dir.path().join("profile")).expect("tempdir path is utf-8");
        let mut builtins = Builtins::for_tests();
        builtins.hostname = "tower".to_owned();

        let rules = vec![AutoMatchRule {
            when: "patina.hostname == 'laptop' or patina.hostname == 'tower'".to_owned(),
            profile: "desktop".to_owned(),
        }];

        let resolution =
            resolve(None, &persisted, &rules, &builtins, &engine()).expect("or now evaluates");
        assert_eq!(resolution.name, "desktop");
        assert_eq!(resolution.source, ProfileSource::AutoMatch);
    }

    /// A `when` that accesses `patina.profile` — unresolved during profile
    /// resolution because it is precisely what this pass computes — is a
    /// typed [`ProfileError::Predicate`] naming the variable, not a silent
    /// non-match (replaces the narrow evaluator's reject tests
    /// and `missing_builtin_compares_as_empty_string`).
    #[test]
    fn auto_match_referencing_patina_profile_errors() {
        let dir = TempDir::new().expect("tempdir");
        let persisted =
            Utf8PathBuf::from_path_buf(dir.path().join("profile")).expect("tempdir path is utf-8");
        let builtins = Builtins::for_tests();

        let rules = vec![AutoMatchRule {
            when: "patina.profile == 'work'".to_owned(),
            profile: "desktop".to_owned(),
        }];

        let err = resolve(None, &persisted, &rules, &builtins, &engine())
            .expect_err("patina.profile is undefined during profile resolution");
        // It is a typed predicate failure over the offending rule, and the
        // wrapped engine error names the undefined variable so the user can
        // fix the source.
        assert!(
            matches!(&err, ProfileError::Predicate { predicate, .. } if predicate == "patina.profile == 'work'"),
            "expected ProfileError::Predicate over the offending rule, got {err:?}"
        );
        let ProfileError::Predicate { source, .. } = &err else {
            return;
        };
        assert!(
            source.to_string().contains("patina.profile"),
            "predicate error must name the undefined variable, got: {source}"
        );
    }

    #[test]
    fn parse_auto_match_rules_empty_doc_is_empty_vec() {
        let rules = parse_auto_match_rules_str("").expect("parse");
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_auto_match_rules_section() {
        let text = r#"
[[auto_match]]
when = "patina.hostname == 'tower'"
profile = "desktop"

[[auto_match]]
when = "patina.os == 'macos'"
profile = "laptop"
"#;
        let rules = parse_auto_match_rules_str(text).expect("parse");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules.first().expect("first rule").profile, "desktop");
        assert_eq!(rules.get(1).expect("second rule").profile, "laptop");
    }

    #[test]
    fn load_auto_match_rules_missing_file_is_empty() {
        let dir = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(dir.path().join("patina.toml"))
            .expect("tempdir path is utf-8");
        let rules = load_auto_match_rules(&root).expect("load");
        assert!(rules.is_empty());
    }

    #[test]
    fn load_auto_match_rules_reads_file() {
        let dir = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(dir.path().join("patina.toml"))
            .expect("tempdir path is utf-8");
        fs_err::write(
            root.as_std_path(),
            "[[auto_match]]\nwhen = \"patina.os == 'linux'\"\nprofile = \"server\"\n",
        )
        .expect("write");
        let rules = load_auto_match_rules(&root).expect("load");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules.first().expect("rule present").profile, "server");
    }
}
