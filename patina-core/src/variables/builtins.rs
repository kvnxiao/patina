//! Built-in `patina.*` variables resolved at process start.
//!
//! The static built-ins (`patina.os`, `patina.arch`, `patina.hostname`,
//! `patina.user`, `patina.home`) are snapshotted once when [`Builtins`]
//! is constructed. The `patina.profile` field is left **unresolved**
//! (`None`) by [`Builtins::current`] and filled in by
//! [`crate::variables::Resolver::with_profile`] once the active profile
//! is resolved — `Some(name)`, where the no-profile fallback is
//! `Some("")`. While it is `None`, a `patina.profile` lookup is
//! undefined: profile resolution itself evaluates `[[auto_match]]`
//! predicates *before* the profile exists, so a rule referencing
//! `patina.profile` accesses an undefined variable and errors rather
//! than silently matching the empty string.
//!
//! The dynamic `patina.env.*` map is **not** snapshotted: each
//! `patina.env.FOO` lookup reads `std::env::var("FOO")` at lookup time
//! so apply-time environment changes are observable.

/// Snapshot of the `patina.*` built-in variable layer.
#[derive(Debug, Clone)]
pub struct Builtins {
    /// Host operating system family: `"macos"`, `"linux"`, or
    /// `"windows"`. Falls back to the raw value of
    /// `std::env::consts::OS` for other Unixes (e.g. `"freebsd"`).
    pub os: String,
    /// Host CPU architecture from `std::env::consts::ARCH` (e.g.
    /// `"x86_64"`, `"aarch64"`).
    pub arch: String,
    /// Host name from the operating system (via `whoami`, a `gethostname` /
    /// `GetComputerNameExW` syscall). Empty when the OS query fails.
    pub hostname: String,
    /// Current user name from the operating system (via `whoami`, a
    /// `getpwuid` / `GetUserNameW` syscall). Empty when the OS query fails.
    pub user: String,
    /// Current user's home directory. Empty when neither `HOME` (unix)
    /// nor `USERPROFILE` (Windows) is set.
    pub home: String,
    /// Active profile name once resolved: `Some(name)`, with the
    /// no-profile fallback represented as `Some("")`. `None` means the
    /// profile is not yet resolved, so a `patina.profile` lookup is
    /// undefined (see the module docs). Filled in by
    /// [`crate::variables::Resolver::with_profile`].
    pub profile: Option<String>,
}

impl Builtins {
    /// Capture the built-ins from the current process environment.
    #[must_use = "the captured snapshot feeds the variable resolver"]
    pub fn current() -> Self {
        Self {
            os: normalized_os(std::env::consts::OS),
            arch: std::env::consts::ARCH.to_owned(),
            hostname: current_hostname(),
            user: current_user(),
            home: current_home(),
            profile: None,
        }
    }

    /// Construct a deterministic snapshot for tests. Values are stable
    /// strings, independent of the host environment.
    #[doc(hidden)]
    #[must_use = "the captured snapshot feeds the variable resolver"]
    pub fn for_tests() -> Self {
        Self {
            os: normalized_os(std::env::consts::OS),
            arch: std::env::consts::ARCH.to_owned(),
            hostname: String::from("test-host"),
            user: String::from("test-user"),
            home: String::from("/home/test-user"),
            profile: None,
        }
    }

    /// Resolve a fully-qualified built-in name (`patina.os`,
    /// `patina.env.FOO`, …). Returns `None` when the name is not in
    /// the built-in namespace or when a `patina.env.*` lookup references
    /// an unset variable.
    pub(crate) fn get(&self, key: &str) -> Option<String> {
        match key {
            "patina.os" => Some(self.os.clone()),
            "patina.arch" => Some(self.arch.clone()),
            "patina.hostname" => Some(self.hostname.clone()),
            "patina.user" => Some(self.user.clone()),
            "patina.home" => Some(self.home.clone()),
            "patina.profile" => self.profile.clone(),
            _ => {
                if let Some(env_key) = key.strip_prefix("patina.env.") {
                    std::env::var(env_key).ok()
                } else {
                    None
                }
            }
        }
    }
}

fn normalized_os(raw: &str) -> String {
    match raw {
        "macos" | "linux" | "windows" => raw.to_owned(),
        other => other.to_owned(),
    }
}

/// Host name from the OS, via `whoami` (a `gethostname` /
/// `GetComputerNameExW` syscall) rather than the `$HOSTNAME` env var, which
/// is a non-exported bash shell variable on Unix and is absent under the
/// systemd/launchd watcher services. Empty when the query fails.
fn current_hostname() -> String {
    whoami::hostname().unwrap_or_default()
}

/// Current user name from the OS, via `whoami` (a `getpwuid` /
/// `GetUserNameW` syscall) rather than `$USER` / `$USERNAME`. Empty when the
/// query fails.
fn current_user() -> String {
    whoami::username().unwrap_or_default()
}

#[cfg(windows)]
fn current_home() -> String {
    std::env::var("USERPROFILE").unwrap_or_default()
}

#[cfg(not(windows))]
fn current_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_os_matches_one_of_the_three_v1_families() {
        let builtins = Builtins::current();
        // The host running the test must be one of macOS / Linux /
        // Windows in CI; allow other Unix variants for local dev.
        assert!(!builtins.os.is_empty());
        if matches!(std::env::consts::OS, "macos" | "linux" | "windows") {
            assert_eq!(builtins.os, std::env::consts::OS);
        }
    }

    #[test]
    fn arch_is_populated() {
        let builtins = Builtins::current();
        assert_eq!(builtins.arch, std::env::consts::ARCH);
    }

    #[test]
    fn unknown_keys_outside_namespace_return_none() {
        let builtins = Builtins::for_tests();
        assert!(builtins.get("email").is_none());
        assert!(builtins.get("patina.unknown").is_none());
    }

    #[test]
    fn env_lookup_reads_process_environment() {
        // `PATH` is one of the few env vars guaranteed to be set on
        // every host that runs the tests. Reading it through the
        // `patina.env.*` map must return the same value `std::env`
        // sees directly.
        let builtins = Builtins::for_tests();
        let direct = std::env::var("PATH").ok();
        let via_builtins = builtins.get("patina.env.PATH");
        assert_eq!(via_builtins, direct);
    }

    #[test]
    fn env_lookup_returns_none_for_unset_variable() {
        let builtins = Builtins::for_tests();
        assert!(
            builtins
                .get("patina.env.PATINA_DEFINITELY_UNSET_VAR_FOR_T006_TEST")
                .is_none()
        );
    }

    #[test]
    fn profile_starts_unresolved() {
        let builtins = Builtins::current();
        // Unresolved until `Resolver::with_profile` sets it, so a
        // `patina.profile` lookup is undefined (returns `None`) rather
        // than a defined empty string.
        assert!(builtins.profile.is_none());
        assert!(builtins.get("patina.profile").is_none());
    }
}
