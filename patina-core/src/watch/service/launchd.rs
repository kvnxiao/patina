//! The macOS `launchd` `LaunchAgent` service backend (SPEC-0003 REQ-001 /
//! REQ-003).
//!
//! `install` writes a per-user `LaunchAgent` plist to
//! `~/Library/LaunchAgents/com.patina.watcher.plist` (mode `0644`) with
//! `RunAtLoad = true`, `KeepAlive` for on-failure restart, and
//! `ProgramArguments` pointing at the canonical `patina` binary invoked with
//! `watch --foreground`, then bootstraps it into the per-user GUI domain with
//! `launchctl bootstrap gui/$(id -u) <plist>`. `start` / `stop` drive
//! `launchctl start` / `stop com.patina.watcher`; `uninstall` stops, boots the
//! service out, and removes the plist; `status` reads
//! `launchctl print gui/$(id -u)/com.patina.watcher` for liveness, last-fired,
//! and last-exit, and recovers the watcher's log counters from the rotated
//! structured log (DEC-012).
//!
//! None of these paths require admin or sudo: a per-user GUI-domain
//! `LaunchAgent` is owned by the invoking user.

use super::FOREGROUND_ARGS;
use super::LifecycleResult;
use super::SERVICE_LABEL;
use super::ServiceBackend;
use super::ServiceError;
use super::ServiceStatus;
use super::canonical_binary_path;
use super::recover_log_counters;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::process::Command;

/// The macOS `launchd` `LaunchAgent` backend.
///
/// Bound to the resolved per-machine `state_dir` so `status` can recover the
/// watcher's log counters from `<state_dir>/logs/`.
#[derive(Debug, Clone)]
pub struct LaunchdBackend {
    /// The resolved per-machine state root, for log-counter recovery (DEC-012).
    state_dir: Utf8PathBuf,
}

impl LaunchdBackend {
    /// Construct a backend bound to the resolved per-machine state root.
    #[must_use = "construct the backend to perform a lifecycle action through it"]
    pub fn new(state_dir: Utf8PathBuf) -> Self {
        Self { state_dir }
    }

    /// The current user's numeric uid, for the `gui/<uid>` `launchd` domain
    /// target. Resolved via `id -u`, exactly the substitution the SPEC's
    /// `launchctl bootstrap gui/$(id -u)` command names; falls back to the
    /// `UID` environment variable, then `0`, if `id` is unavailable.
    fn uid() -> String {
        if let Ok(output) = Command::new("id").arg("-u").output()
            && output.status.success()
        {
            let raw = String::from_utf8_lossy(&output.stdout);
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
        std::env::var("UID").unwrap_or_else(|_| "0".to_owned())
    }

    /// The per-user GUI domain target for the service
    /// (`gui/<uid>/com.patina.watcher`).
    fn service_target() -> String {
        format!("gui/{}/{SERVICE_LABEL}", Self::uid())
    }

    /// The per-user GUI domain target (`gui/<uid>`).
    fn domain_target() -> String {
        format!("gui/{}", Self::uid())
    }

    /// The absolute path of the `LaunchAgent` plist
    /// (`~/Library/LaunchAgents/com.patina.watcher.plist`).
    fn plist_path() -> Result<Utf8PathBuf, ServiceError> {
        let home = std::env::var("HOME")
            .map_err(|source| ServiceError::Supervisor(format!("HOME is unset: {source}")))?;
        Ok(Utf8PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{SERVICE_LABEL}.plist")))
    }

    /// Whether the `LaunchAgent` plist is registered (the plist file exists).
    fn is_installed() -> bool {
        Self::plist_path().is_ok_and(|path| path.exists())
    }

    /// Run a `launchctl` subcommand, mapping a non-zero exit to
    /// [`ServiceError::Supervisor`] carrying the captured stderr.
    fn launchctl(args: &[&str]) -> Result<std::process::Output, ServiceError> {
        let output = Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|source| {
                ServiceError::Supervisor(format!("failed to run launchctl: {source}"))
            })?;
        if output.status.success() {
            Ok(output)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(ServiceError::Supervisor(format!(
                "launchctl {} failed: {}",
                args.join(" "),
                stderr.trim()
            )))
        }
    }
}

impl ServiceBackend for LaunchdBackend {
    fn install(&self) -> Result<LifecycleResult, ServiceError> {
        if Self::is_installed() {
            return Err(ServiceError::AlreadyInstalled);
        }
        let binary = canonical_binary_path()?;
        let plist_path = Self::plist_path()?;
        write_plist(&plist_path, &binary)?;

        Self::launchctl(&["bootstrap", &Self::domain_target(), plist_path.as_str()])?;
        Ok(LifecycleResult::Installed)
    }

    fn uninstall(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        let plist_path = Self::plist_path()?;

        // Stop the running watcher first (best-effort: a stopped service is not
        // an error), then boot it out of the domain.
        let _stop = Self::launchctl(&["stop", SERVICE_LABEL]);
        let _bootout = Self::launchctl(&["bootout", &Self::service_target()]);

        fs_err::remove_file(plist_path.as_std_path()).map_err(|source| {
            ServiceError::WriteDescriptor {
                path: plist_path,
                source,
            }
        })?;
        Ok(LifecycleResult::Uninstalled)
    }

    fn start(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        Self::launchctl(&["start", SERVICE_LABEL])?;
        Ok(LifecycleResult::Started)
    }

    fn stop(&self) -> Result<LifecycleResult, ServiceError> {
        if !Self::is_installed() {
            return Ok(LifecycleResult::NotInstalled);
        }
        Self::launchctl(&["stop", SERVICE_LABEL])?;
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

        // `launchctl print` exits non-zero when the service is installed but
        // not currently loaded; treat that as "not running" rather than an
        // error so a stopped-but-installed service still produces a clean
        // status object (CHK-006).
        let print = Command::new("launchctl")
            .args(["print", &Self::service_target()])
            .output()
            .map_err(|source| {
                ServiceError::Supervisor(format!("failed to run launchctl print: {source}"))
            })?;
        let text = String::from_utf8_lossy(&print.stdout);
        let parsed = parse_launchctl_print(&text);

        Ok(ServiceStatus {
            installed: true,
            running: parsed.running,
            last_fired_at: None,
            last_exit_code: parsed.last_exit_code,
            subscriptions_count,
            re_applies_since_start,
        })
    }
}

/// Write the `LaunchAgent` plist for `binary` to `path` with mode `0644`,
/// creating `~/Library/LaunchAgents/` if it does not exist.
fn write_plist(path: &Utf8Path, binary: &Utf8Path) -> Result<(), ServiceError> {
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent.as_std_path()).map_err(|source| {
            ServiceError::WriteDescriptor {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    fs_err::write(path.as_std_path(), render_plist(binary).as_bytes()).map_err(|source| {
        ServiceError::WriteDescriptor {
            path: path.to_path_buf(),
            source,
        }
    })?;
    set_mode_0644(path)?;
    Ok(())
}

/// Set the plist file's permission bits to `0644` (REQ-001 / CHK-001).
#[cfg(unix)]
fn set_mode_0644(path: &Utf8Path) -> Result<(), ServiceError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o644);
    fs_err::set_permissions(path.as_std_path(), perms).map_err(|source| {
        ServiceError::WriteDescriptor {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Non-Unix builds cannot set Unix mode bits; this backend is macOS-only, so
/// this arm exists solely to keep the function total. It is never reached.
#[cfg(not(unix))]
fn set_mode_0644(_path: &Utf8Path) -> Result<(), ServiceError> {
    Ok(())
}

/// Render the `LaunchAgent` plist XML for the watcher service: `RunAtLoad =
/// true`, `KeepAlive` for on-failure restart, and `ProgramArguments` of the
/// canonical `binary` plus `watch --foreground` (REQ-001).
fn render_plist(binary: &Utf8Path) -> String {
    use std::fmt::Write as _;

    // The ProgramArguments array: the canonical binary path followed by the
    // foreground tokens, each rendered as one escaped `<string>` element.
    let mut program_args = String::new();
    for token in std::iter::once(binary.as_str()).chain(FOREGROUND_ARGS) {
        // Writing into a String is infallible; the Result is discarded.
        ignore_fmt(writeln!(
            program_args,
            "\t\t<string>{}</string>",
            xml_escape(token)
        ));
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\
\t<string>{SERVICE_LABEL}</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
{program_args}\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
\t<key>KeepAlive</key>\n\
\t<dict>\n\
\t\t<key>SuccessfulExit</key>\n\
\t\t<false/>\n\
\t</dict>\n\
</dict>\n\
</plist>\n"
    )
}

/// Escape the five XML special characters so a binary path or argument
/// containing `&`, `<`, `>`, `"`, or `'` produces a well-formed plist.
fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Discard an infallible `std::fmt::Write` result, mirroring the sibling
/// `ignore_fmt` helpers in [`crate::journal::render`] and
/// [`crate::watch::drift_cache`]. Writing into an in-memory `String` cannot
/// fail, so the `Result` carries no recoverable information.
fn ignore_fmt(_result: std::fmt::Result) {}

/// The supervisor-derived fields parsed out of `launchctl print` output.
struct LaunchctlPrint {
    /// Whether the service is currently running (`state = running`, or a live
    /// `pid = N`).
    running: bool,
    /// The most recent recorded exit code (`last exit code = N`), if any.
    last_exit_code: Option<i64>,
}

/// Parse the liveness state and last-exit code out of `launchctl print`
/// output. `launchctl print gui/<uid>/<label>` emits an indented key-value
/// dump; this reads the `state = …`, `pid = …`, and `last exit code = …`
/// lines, tolerating the absent-field case (each yields a `None` / `false`).
fn parse_launchctl_print(text: &str) -> LaunchctlPrint {
    let mut running = false;
    let mut last_exit_code = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("state = ") {
            running = value.trim() == "running";
        } else if let Some(value) = trimmed.strip_prefix("pid = ") {
            // A live pid line implies a running service even when no explicit
            // `state = running` line is present in this launchctl version.
            if value.trim().parse::<u32>().is_ok() {
                running = true;
            }
        } else if let Some(value) = trimmed.strip_prefix("last exit code = ") {
            last_exit_code = value.trim().parse::<i64>().ok();
        }
    }

    LaunchctlPrint {
        running,
        last_exit_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plist_contains_the_required_keys_and_program_arguments() {
        let binary = Utf8Path::new("/usr/local/bin/patina");
        let plist = render_plist(binary);

        // REQ-001: the plist must declare the service label, RunAtLoad, the
        // KeepAlive on-failure restart, and the foreground program arguments.
        assert!(plist.contains(&format!("<string>{SERVICE_LABEL}</string>")));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains("<string>/usr/local/bin/patina</string>"));
        assert!(plist.contains("<string>watch</string>"));
        assert!(plist.contains("<string>--foreground</string>"));
    }

    #[test]
    fn render_plist_is_well_formed_xml_with_a_single_program_arguments_array() {
        let plist = render_plist(Utf8Path::new("/bin/patina"));
        assert!(plist.starts_with("<?xml version=\"1.0\""));
        assert!(plist.trim_end().ends_with("</plist>"));
        // The binary plus the two foreground tokens make exactly three
        // <string> entries inside ProgramArguments, plus the Label string.
        assert_eq!(plist.matches("<string>").count(), 4);
    }

    #[test]
    fn xml_escape_escapes_the_five_special_characters() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        // A path with a space (common on macOS) needs no escaping and is left
        // intact.
        assert_eq!(xml_escape("/Users/a b/bin/patina"), "/Users/a b/bin/patina");
    }

    #[test]
    fn parse_launchctl_print_reads_running_state_and_last_exit_code() {
        // A representative `launchctl print` dump: indented key = value lines.
        let dump = "\
gui/501/com.patina.watcher = {
\tactive count = 1
\tstate = running
\tpid = 4321
\tlast exit code = 0
}
";
        let parsed = parse_launchctl_print(dump);
        assert!(parsed.running, "state = running must parse as running");
        assert_eq!(parsed.last_exit_code, Some(0));
    }

    #[test]
    fn parse_launchctl_print_treats_a_non_running_state_as_stopped() {
        // CHK-006: an installed-but-stopped service reports running = false and
        // surfaces the recorded last exit code.
        let dump = "\
gui/501/com.patina.watcher = {
\tstate = not running
\tlast exit code = 2
}
";
        let parsed = parse_launchctl_print(dump);
        assert!(!parsed.running);
        assert_eq!(parsed.last_exit_code, Some(2));
    }

    #[cfg(unix)]
    #[test]
    fn write_plist_sets_the_file_mode_to_0644() {
        use std::os::unix::fs::PermissionsExt;

        // CHK-001: the written LaunchAgent plist must exist with mode 0644.
        // `write_plist` is pure filesystem work against a path argument, so a
        // tempdir-scoped write fully exercises the mode without launchctl.
        let tmp = tempfile::tempdir().expect("tempdir");
        let plist = Utf8PathBuf::from_path_buf(tmp.path().join("com.patina.watcher.plist"))
            .expect("utf-8 temp path");
        write_plist(&plist, Utf8Path::new("/usr/local/bin/patina")).expect("write plist");

        let mode = fs_err::metadata(plist.as_std_path())
            .expect("stat plist")
            .permissions()
            .mode();
        // Compare only the permission bits; the file-type bits in `mode` are
        // platform-set and not what CHK-001 constrains.
        assert_eq!(
            mode & 0o777,
            0o644,
            "the written plist must carry mode 0644, got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn parse_launchctl_print_reports_none_exit_code_when_never_run() {
        // CHK-006: `last_exit_code` is None when the service has no recorded
        // exit (never run since load).
        let dump = "gui/501/com.patina.watcher = {\n\tstate = waiting\n}\n";
        let parsed = parse_launchctl_print(dump);
        assert!(!parsed.running);
        assert_eq!(parsed.last_exit_code, None);
    }
}
