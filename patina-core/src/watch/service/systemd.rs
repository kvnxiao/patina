//! The Linux `systemd --user` service backend (SPEC-0003 REQ-001 / REQ-003 /
//! DEC-005 / DEC-010).
//!
//! `install` writes a per-user unit to
//! `~/.config/systemd/user/patina-watcher.service` (`Restart=on-failure`,
//! `WantedBy=default.target`, and `ExecStart` pointing at the canonical
//! `patina` binary invoked with `watch --foreground`), then enables and starts
//! it with `systemctl --user enable --now patina-watcher.service`. `start` /
//! `stop` drive `systemctl --user start` / `stop`; `restart` is
//! stop-then-start; `uninstall` stops, `systemctl --user disable`s, and removes
//! the unit file; `status` queries `systemctl --user` for liveness, last-fired,
//! and last-exit, and recovers the watcher's log counters from the rotated
//! structured log (DEC-012).
//!
//! Per DEC-005, neither `install` nor `uninstall` invokes
//! `loginctl enable-linger` / `disable-linger`, and there is no `--linger`
//! flag: a user who wants the watcher to survive logout runs
//! `sudo loginctl enable-linger $USER` themselves (documented in
//! `docs/USER_GUIDE.md`). None of these paths require admin or sudo: a
//! `systemd --user` unit is owned by the invoking user.
//!
//! On a host where `systemd --user` is unavailable (a non-systemd init, or a
//! systemd build without the user bus reachable), the [`super::current`]
//! factory returns the [`super::unsupported`] stub instead of this backend, so
//! the user is directed at `patina watch --foreground` under their own
//! supervisor (DEC-010).

use super::FOREGROUND_ARGS;
use super::LifecycleResult;
use super::ServiceBackend;
use super::ServiceError;
use super::ServiceStatus;
use super::canonical_binary_path;
use super::recover_log_counters;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::process::Command;

/// The systemd unit name for the watcher service.
pub const UNIT_NAME: &str = "patina-watcher.service";

/// The Linux `systemd --user` backend.
///
/// Bound to the resolved per-machine `state_dir` so `status` can recover the
/// watcher's log counters from `<state_dir>/logs/`.
#[derive(Debug, Clone)]
pub struct SystemdBackend {
    /// The resolved per-machine state root, for log-counter recovery (DEC-012).
    state_dir: Utf8PathBuf,
}

impl SystemdBackend {
    /// Construct a backend bound to the resolved per-machine state root.
    #[must_use = "construct the backend to perform a lifecycle action through it"]
    pub fn new(state_dir: Utf8PathBuf) -> Self {
        Self { state_dir }
    }

    /// Whether `systemd --user` is reachable on this host.
    ///
    /// The factory uses this to decide between the systemd backend and the
    /// [`super::unsupported`] fallback (DEC-010). A successful
    /// `systemctl --user is-system-running` *invocation* (any exit code, even
    /// the `degraded` / `offline` non-zero ones — the bus answered) proves the
    /// user bus is reachable; a spawn failure (no `systemctl` binary) or an
    /// explicit "Failed to connect to bus" message means there is no user
    /// manager to drive, so we fall back to the foreground escape hatch.
    #[must_use = "the availability decision selects the backend; ignoring it loses the dispatch"]
    pub fn is_available() -> bool {
        let Ok(output) = Command::new("systemctl")
            .args(["--user", "is-system-running"])
            .output()
        else {
            // No `systemctl` binary at all: not a systemd host.
            return false;
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        bus_reachable(&stderr)
    }

    /// The absolute path of the per-user unit file
    /// (`~/.config/systemd/user/patina-watcher.service`).
    fn unit_path() -> Result<Utf8PathBuf, ServiceError> {
        let dir = unit_dir()?;
        Ok(dir.join(UNIT_NAME))
    }

    /// Whether the unit is registered (the unit file exists).
    fn is_installed() -> bool {
        Self::unit_path().is_ok_and(|path| path.exists())
    }

    /// Run a `systemctl --user` subcommand, mapping a non-zero exit to
    /// [`ServiceError::Supervisor`] carrying the captured stderr.
    fn systemctl(args: &[&str]) -> Result<std::process::Output, ServiceError> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .map_err(|source| {
                ServiceError::Supervisor(format!("failed to run systemctl --user: {source}"))
            })?;
        if output.status.success() {
            Ok(output)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(ServiceError::Supervisor(format!(
                "systemctl --user {} failed: {}",
                args.join(" "),
                stderr.trim()
            )))
        }
    }
}

impl ServiceBackend for SystemdBackend {
    fn install(&self) -> Result<LifecycleResult, ServiceError> {
        if Self::is_installed() {
            return Err(ServiceError::AlreadyInstalled);
        }
        let binary = canonical_binary_path()?;
        let unit_path = Self::unit_path()?;
        write_unit(&unit_path, &binary)?;

        // `systemctl --user` reads its unit directory lazily; a daemon-reload
        // makes the freshly written unit visible before enable --now.
        let _reload = Self::systemctl(&["daemon-reload"]);
        Self::systemctl(&["enable", "--now", UNIT_NAME])?;
        Ok(LifecycleResult::Installed)
    }

    fn uninstall(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        let unit_path = Self::unit_path()?;

        // Stop and disable first (best-effort: a stopped / already-disabled
        // service is not an error), then remove the unit file and reload so the
        // manager forgets the unit.
        let _stop = Self::systemctl(&["stop", UNIT_NAME]);
        let _disable = Self::systemctl(&["disable", UNIT_NAME]);

        fs_err::remove_file(unit_path.as_std_path()).map_err(|source| {
            ServiceError::WriteDescriptor {
                path: unit_path,
                source,
            }
        })?;
        let _reload = Self::systemctl(&["daemon-reload"]);
        Ok(LifecycleResult::Uninstalled)
    }

    fn start(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        Self::systemctl(&["start", UNIT_NAME])?;
        Ok(LifecycleResult::Started)
    }

    fn stop(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        Self::systemctl(&["stop", UNIT_NAME])?;
        Ok(LifecycleResult::Stopped)
    }

    fn restart(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        self.stop()?;
        self.start()?;
        Ok(LifecycleResult::Restarted)
    }

    fn status(&self) -> Result<ServiceStatus, ServiceError> {
        let installed = Self::is_installed();
        let (subscriptions_count, re_applies_since_start) = recover_log_counters(&self.state_dir);

        if !installed {
            return Ok(ServiceStatus {
                installed: false,
                running: false,
                last_fired_at: None,
                last_exit_code: None,
                subscriptions_count,
                re_applies_since_start,
            });
        }

        // `systemctl --user show` emits `Key=Value` lines and exits 0 even for
        // an inactive unit, so it is the read-only liveness query: ActiveState
        // gives running, ExecMainExitTimestamp gives last-fired, and
        // ExecMainStatus gives the last exit code.
        let show = Command::new("systemctl")
            .args([
                "--user",
                "show",
                UNIT_NAME,
                "--property=ActiveState,ExecMainStatus,ExecMainExitTimestamp",
            ])
            .output()
            .map_err(|source| {
                ServiceError::Supervisor(format!("failed to run systemctl --user show: {source}"))
            })?;
        let text = String::from_utf8_lossy(&show.stdout);
        let parsed = parse_systemctl_show(&text);

        Ok(ServiceStatus {
            installed: true,
            running: parsed.running,
            last_fired_at: parsed.last_fired_at,
            last_exit_code: parsed.last_exit_code,
            subscriptions_count,
            re_applies_since_start,
        })
    }
}

/// Decide whether the `systemd --user` bus is reachable from a
/// `systemctl --user is-system-running` invocation's captured stderr.
///
/// The user bus is unreachable only when systemctl reports it cannot connect;
/// that surfaces on stderr regardless of exit code (`is-system-running` exits
/// non-zero for `degraded` / `offline` even when the bus *did* answer). Any
/// other stderr — empty, or a benign state word — means the user manager
/// answered, so the host is a systemd host (DEC-010 routing decision).
fn bus_reachable(stderr: &str) -> bool {
    !stderr.contains("Failed to connect to bus")
}

/// The per-user systemd unit directory
/// (`~/.config/systemd/user/`), honouring `XDG_CONFIG_HOME` when set.
fn unit_dir() -> Result<Utf8PathBuf, ServiceError> {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    resolve_unit_dir(xdg.as_deref(), home.as_deref())
}

/// Resolve the per-user systemd unit directory from the relevant env values.
///
/// `$XDG_CONFIG_HOME/systemd/user` when `XDG_CONFIG_HOME` is set and non-empty,
/// otherwise `$HOME/.config/systemd/user`. Taking the env values as arguments
/// keeps the precedence logic pure and testable without mutating process env
/// (the workspace forbids the `unsafe` `set_var`).
fn resolve_unit_dir(
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Result<Utf8PathBuf, ServiceError> {
    let base = match xdg_config_home {
        Some(xdg) if !xdg.is_empty() => Utf8PathBuf::from(xdg),
        _ => {
            let home = home.ok_or_else(|| ServiceError::Supervisor("HOME is unset".to_owned()))?;
            Utf8PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("systemd").join("user"))
}

/// Write the systemd unit for `binary` to `path`, creating
/// `~/.config/systemd/user/` if it does not exist.
fn write_unit(path: &Utf8Path, binary: &Utf8Path) -> Result<(), ServiceError> {
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent.as_std_path()).map_err(|source| {
            ServiceError::WriteDescriptor {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    fs_err::write(path.as_std_path(), render_unit(binary).as_bytes()).map_err(|source| {
        ServiceError::WriteDescriptor {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(())
}

/// Render the systemd unit for the watcher service: `Restart=on-failure`,
/// `WantedBy=default.target`, and `ExecStart` of the canonical `binary` plus
/// `watch --foreground` (REQ-001).
///
/// Each `ExecStart` token is quoted and escaped through [`systemd_exec_quote`]
/// so a binary path containing whitespace, a `%` specifier, or a newline lands
/// as a single literal argument rather than being word-split, specifier-
/// expanded, or injected as a fresh unit directive — mirroring how the launchd
/// sibling XML-escapes the same path for its descriptor format.
fn render_unit(binary: &Utf8Path) -> String {
    let exec_start = std::iter::once(binary.as_str())
        .chain(FOREGROUND_ARGS)
        .map(systemd_exec_quote)
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "[Unit]\n\
Description=Patina dotfile watcher\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={exec_start}\n\
Restart=on-failure\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

/// Quote and escape one `ExecStart` token per systemd's command-line rules.
///
/// systemd's `ExecStart=` is line-oriented: it word-splits on whitespace,
/// expands `%` specifiers (`%h`, `%t`, …), and treats a newline as the end of
/// the directive. An unescaped path is therefore both fragile (a legitimate
/// space-containing install prefix is split into spurious args) and a
/// descriptor-injection vector (a `\n` would start a fresh directive). We
/// neutralize all three by emitting the token as a single double-quoted word:
///
/// - the whole token is wrapped in `"…"`, so embedded whitespace stays part of
///   one argument;
/// - inside the quotes, `\` and `"` are backslash-escaped (systemd's
///   double-quote escapes), and a literal newline / carriage return / tab is
///   rendered as its C-style escape (`\n` / `\r` / `\t`) so it can never
///   terminate the line;
/// - `%` is doubled to `%%` so systemd takes it literally instead of expanding
///   it as a specifier (`%` expansion happens regardless of quoting).
fn systemd_exec_quote(token: &str) -> String {
    let mut out = String::with_capacity(token.len() + 2);
    out.push('"');
    for ch in token.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '%' => out.push_str("%%"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The supervisor-derived fields parsed out of `systemctl --user show` output.
struct SystemctlShow {
    /// Whether the unit's `ActiveState` is `active` (the unit is running).
    running: bool,
    /// The unit's last-fired timestamp (`ExecMainExitTimestamp`), if recorded.
    last_fired_at: Option<String>,
    /// The unit's last recorded exit code (`ExecMainStatus`), if any.
    last_exit_code: Option<i64>,
}

/// Parse the liveness state, last-fired timestamp, and last-exit code out of
/// `systemctl --user show` output. The command emits `Key=Value` lines; this
/// reads `ActiveState=`, `ExecMainExitTimestamp=`, and `ExecMainStatus=`,
/// tolerating absent or empty fields (each yields `None` / `false`).
fn parse_systemctl_show(text: &str) -> SystemctlShow {
    let mut running = false;
    let mut last_fired_at = None;
    let mut last_exit_code = None;

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("ActiveState=") {
            running = value.trim() == "active";
        } else if let Some(value) = line.strip_prefix("ExecMainExitTimestamp=") {
            let trimmed = value.trim();
            // systemd renders an unset timestamp as an empty value.
            if !trimmed.is_empty() {
                last_fired_at = Some(trimmed.to_owned());
            }
        } else if let Some(value) = line.strip_prefix("ExecMainStatus=") {
            last_exit_code = value.trim().parse::<i64>().ok();
        }
    }

    SystemctlShow {
        running,
        last_fired_at,
        last_exit_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_unit_contains_the_required_directives_and_exec_start() {
        let binary = Utf8Path::new("/usr/local/bin/patina");
        let unit = render_unit(binary);

        // REQ-001: the unit must declare on-failure restart, the
        // default.target install hook, and an ExecStart pointing at the
        // canonical binary invoked with `watch --foreground`.
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        // Each ExecStart token is quoted (systemd word-splits unquoted values);
        // a space-free canonical path is one quoted word per token.
        assert!(
            unit.contains("ExecStart=\"/usr/local/bin/patina\" \"watch\" \"--foreground\""),
            "ExecStart must name the canonical binary plus the foreground tokens, got: {unit}"
        );
    }

    #[test]
    fn render_unit_neutralizes_space_percent_and_newline_in_the_binary_path() {
        // A hostile / awkward install path: a space (word-split risk), a `%`
        // (specifier-expansion risk), and a newline (directive-injection risk).
        // All three must land inside a single quoted ExecStart word so systemd
        // takes the path literally and the unit stays a one-line ExecStart.
        let binary = Utf8Path::new("/opt/my apps/pat%ina\nExecStartPost=/evil");
        let unit = render_unit(binary);

        // The injected `\n` must be escaped, not emitted raw: there must be no
        // line that begins a second ExecStart-family directive.
        assert!(
            !unit.contains("\nExecStartPost="),
            "a newline in the path must not inject a fresh directive, got: {unit}"
        );
        // The whole token is one quoted word: the space stays inside the quotes,
        // `%` is doubled, and the newline is the C-style `\n` escape.
        assert!(
            unit.contains(
                "ExecStart=\"/opt/my apps/pat%%ina\\nExecStartPost=/evil\" \"watch\" \"--foreground\""
            ),
            "ExecStart must quote-and-escape the path as a single literal word, got: {unit}"
        );
    }

    #[test]
    fn systemd_exec_quote_escapes_quotes_and_backslashes() {
        // Inside systemd's double-quote string, `"` and `\` must be
        // backslash-escaped so the quoting cannot be broken out of.
        assert_eq!(systemd_exec_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        // A plain token is just wrapped in quotes.
        assert_eq!(systemd_exec_quote("watch"), "\"watch\"");
    }

    #[test]
    fn bus_reachable_is_false_only_when_the_bus_connect_fails() {
        // The connect-failure message means there is no user manager to drive:
        // route to the unsupported `--foreground` fallback (DEC-010).
        assert!(!bus_reachable(
            "Failed to connect to bus: No such file or directory"
        ));
        // Every other outcome means the user bus answered, so the host is a
        // systemd host: a degraded/running state word, or empty stderr.
        assert!(bus_reachable(""));
        assert!(bus_reachable("degraded"));
        assert!(bus_reachable("running"));
    }

    #[test]
    fn render_unit_has_the_three_canonical_sections() {
        let unit = render_unit(Utf8Path::new("/bin/patina"));
        // A valid systemd service unit needs the [Unit], [Service], and
        // [Install] sections; the [Install] section is what makes
        // `systemctl --user enable` create the default.target want symlink.
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn parse_systemctl_show_reads_active_state_exit_code_and_timestamp() {
        // A representative `systemctl --user show` dump for a running unit.
        let dump = "\
ActiveState=active
ExecMainStatus=0
ExecMainExitTimestamp=Sat 2026-05-31 12:00:00 UTC
";
        let parsed = parse_systemctl_show(dump);
        assert!(parsed.running, "ActiveState=active must parse as running");
        assert_eq!(parsed.last_exit_code, Some(0));
        assert_eq!(
            parsed.last_fired_at.as_deref(),
            Some("Sat 2026-05-31 12:00:00 UTC")
        );
    }

    #[test]
    fn parse_systemctl_show_treats_inactive_state_as_stopped() {
        // An installed-but-stopped unit reports running = false and surfaces
        // the recorded last exit code (REQ-003 status shape).
        let dump = "\
ActiveState=inactive
ExecMainStatus=2
ExecMainExitTimestamp=Sat 2026-05-31 11:00:00 UTC
";
        let parsed = parse_systemctl_show(dump);
        assert!(!parsed.running);
        assert_eq!(parsed.last_exit_code, Some(2));
    }

    #[test]
    fn parse_systemctl_show_reports_none_when_never_run() {
        // A unit that has never run: empty ExecMainExitTimestamp and a status
        // that may be absent. Both the timestamp and (when absent) the exit
        // code report None rather than a panic.
        let dump = "ActiveState=inactive\nExecMainExitTimestamp=\n";
        let parsed = parse_systemctl_show(dump);
        assert!(!parsed.running);
        assert_eq!(parsed.last_fired_at, None);
        assert_eq!(parsed.last_exit_code, None);
    }

    #[test]
    fn write_unit_writes_a_valid_unit_to_the_target_path() {
        // `write_unit` is pure filesystem work against a path argument, so a
        // tempdir-scoped write exercises the directory creation and content
        // without systemctl. CHK-002's unit-file-exists assertion is gated by
        // the same render the CLI install path uses.
        let tmp = tempfile::tempdir().expect("tempdir");
        let unit = Utf8PathBuf::from_path_buf(tmp.path().join("user").join(UNIT_NAME))
            .expect("utf-8 temp path");
        write_unit(&unit, Utf8Path::new("/usr/local/bin/patina")).expect("write unit");

        let written = fs_err::read_to_string(unit.as_std_path()).expect("read back unit");
        assert!(written.contains("ExecStart=\"/usr/local/bin/patina\" \"watch\" \"--foreground\""));
        assert!(written.contains("Restart=on-failure"));
    }

    #[test]
    fn resolve_unit_dir_prefers_xdg_config_home_over_home() {
        // A set, non-empty XDG_CONFIG_HOME wins: the unit dir lives directly
        // under it, not under $HOME/.config.
        let dir = resolve_unit_dir(Some("/xdg"), Some("/home/u"))
            .expect("unit dir resolves under XDG_CONFIG_HOME");
        assert_eq!(dir, Utf8Path::new("/xdg/systemd/user"));
    }

    #[test]
    fn resolve_unit_dir_falls_back_to_home_dot_config() {
        // With XDG_CONFIG_HOME unset (or empty), the dir is $HOME/.config/...;
        // an empty value must not be treated as a valid base.
        let from_unset =
            resolve_unit_dir(None, Some("/home/u")).expect("unit dir resolves under HOME");
        assert_eq!(from_unset, Utf8Path::new("/home/u/.config/systemd/user"));
        let from_empty =
            resolve_unit_dir(Some(""), Some("/home/u")).expect("empty XDG falls back to HOME");
        assert_eq!(from_empty, Utf8Path::new("/home/u/.config/systemd/user"));
    }

    #[test]
    fn resolve_unit_dir_errors_when_no_base_is_available() {
        // Neither XDG_CONFIG_HOME nor HOME: there is no per-user base to write
        // the unit under, so resolution fails with a clear supervisor error
        // rather than constructing a nonsensical relative path.
        let err = resolve_unit_dir(None, None).expect_err("no base must error");
        assert!(matches!(err, ServiceError::Supervisor(_)));
    }
}
