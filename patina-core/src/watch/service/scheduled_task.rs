//! The Windows per-user Scheduled Task service backend (SPEC-0003 REQ-001 /
//! REQ-003).
//!
//! `install` registers a per-user Scheduled Task named `Patina Watcher` with a
//! logon trigger, `RunLevel = Limited` (non-elevated), and an action pointing
//! at the canonical `patina` binary invoked with `watch --foreground`, through
//! `winsafe`'s `taskschd` COM surface — the same HKCU-scoped API that
//! `schtasks /create /sc onlogon` drives. `start` runs the task, `stop` ends
//! it, `restart` is stop-then-start, `uninstall` deletes it; `status` queries
//! the registered task for liveness, last-run time, and last-exit code, and
//! recovers the watcher's log counters from the rotated structured log
//! (DEC-012).
//!
//! None of these paths require admin: the task is registered in the current
//! user's folder (`\`) with `TASK_LOGON_INTERACTIVE_TOKEN` and a
//! least-privilege run level, so the Task Scheduler accepts it for the
//! invoking user without elevation. The HKCU-scoped, non-elevated nature is
//! why this backend lives in `patina-core` rather than the elevation-only
//! `patina-elevate` helper.
//!
//! The task definition is built as Task Scheduler 2.0 registration XML
//! ([`render_task_xml`]) and handed to `ITaskService` via `put_XmlText` +
//! `RegisterTaskDefinition`. Building the descriptor as a pure string mirrors
//! the `launchd` plist and `systemd` unit siblings and lets CHK-003's
//! trigger / run-level assertions be unit-tested as a string property on any
//! platform, with no live Task Scheduler in the loop.

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
use winsafe::co;
use winsafe::prelude::*;

/// The Scheduled Task name registered in the current user's task folder.
pub const TASK_NAME: &str = "Patina Watcher";

/// The Windows per-user Scheduled Task backend.
///
/// Bound to the resolved per-machine `state_dir` so `status` can recover the
/// watcher's log counters from `<state_dir>/logs/`.
#[derive(Debug, Clone)]
pub struct ScheduledTaskBackend {
    /// The resolved per-machine state root, for log-counter recovery (DEC-012).
    state_dir: Utf8PathBuf,
}

impl ScheduledTaskBackend {
    /// Construct a backend bound to the resolved per-machine state root.
    #[must_use = "construct the backend to perform a lifecycle action through it"]
    pub fn new(state_dir: Utf8PathBuf) -> Self {
        Self { state_dir }
    }
}

impl ServiceBackend for ScheduledTaskBackend {
    fn install(&self) -> Result<LifecycleResult, ServiceError> {
        if task_exists()? {
            return Err(ServiceError::AlreadyInstalled);
        }
        let binary = canonical_binary_path()?;
        let xml = render_task_xml(&binary);
        register_task(&xml)?;
        Ok(LifecycleResult::Installed)
    }

    fn uninstall(&self) -> Result<LifecycleResult, ServiceError> {
        if !task_exists()? {
            return Ok(LifecycleResult::NotInstalled);
        }
        // Best-effort stop first (a not-running task is not an error), then
        // delete the registration.
        let _stop = schtasks(&["/end", "/tn", TASK_NAME]);
        delete_task()?;
        Ok(LifecycleResult::Uninstalled)
    }

    fn start(&self) -> Result<LifecycleResult, ServiceError> {
        if !task_exists()? {
            return Ok(LifecycleResult::NotInstalled);
        }
        schtasks(&["/run", "/tn", TASK_NAME])?;
        Ok(LifecycleResult::Started)
    }

    fn stop(&self) -> Result<LifecycleResult, ServiceError> {
        if !task_exists()? {
            return Ok(LifecycleResult::NotInstalled);
        }
        schtasks(&["/end", "/tn", TASK_NAME])?;
        Ok(LifecycleResult::Stopped)
    }

    fn restart(&self) -> Result<LifecycleResult, ServiceError> {
        if !task_exists()? {
            return Ok(LifecycleResult::NotInstalled);
        }
        self.stop()?;
        self.start()?;
        Ok(LifecycleResult::Restarted)
    }

    fn status(&self) -> Result<ServiceStatus, ServiceError> {
        let (subscriptions_count, re_applies_since_start) = recover_log_counters(&self.state_dir);

        // The supervisor-derived fields come from the registered task's COM
        // properties: `get_State` gives liveness, `get_LastTaskResult` the last
        // exit code, and `get_LastRunTime` the last-fired time. A task that has
        // never run reports a sentinel result / epoch time, which maps to None.
        let supervisor = query_task_status()?;
        Ok(ServiceStatus {
            installed: supervisor.installed,
            running: supervisor.running,
            last_fired_at: supervisor.last_fired_at,
            last_exit_code: supervisor.last_exit_code,
            subscriptions_count,
            re_applies_since_start,
        })
    }
}

/// The current user's task folder path. A per-user Scheduled Task is registered
/// at the root folder (`\`) under the invoking user's credentials; there is no
/// admin-only subfolder involved.
const TASK_FOLDER: &str = r"\";

/// Connect to the local Task Scheduler service, returning the root
/// `ITaskService` with COM already initialized for this call.
///
/// The returned [`winsafe::guard::CoUninitializeGuard`] must be held for the
/// lifetime of every COM object derived from the service: dropping it
/// uninitializes COM on this thread and invalidates the interface pointers.
/// Callers therefore bind the guard to a local that outlives the work.
fn connect_service()
-> Result<(winsafe::guard::CoUninitializeGuard, winsafe::ITaskService), ServiceError> {
    let guard =
        winsafe::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE)
            .map_err(|err| supervisor_err("CoInitializeEx", err))?;
    let service = winsafe::CoCreateInstance::<winsafe::ITaskService>(
        &co::CLSID::TaskScheduler,
        None::<&winsafe::IUnknown>,
        co::CLSCTX::INPROC_SERVER,
    )
    .map_err(|err| supervisor_err("CoCreateInstance(TaskScheduler)", err))?;
    // Connect to the local scheduler under the current user (all `None` args =
    // local machine, current credentials — the HKCU-scoped, non-elevated path).
    service
        .Connect(None, None, None, None)
        .map_err(|err| supervisor_err("ITaskService::Connect", err))?;
    Ok((guard, service))
}

/// Whether the `Patina Watcher` task is registered in the current user's
/// folder. `GetTask` succeeds for a registered task and fails with a
/// not-found `HRESULT` otherwise; the latter is reported as `false`, any other
/// failure as a supervisor error.
fn task_exists() -> Result<bool, ServiceError> {
    let (_guard, service) = connect_service()?;
    let folder = service
        .GetFolder(TASK_FOLDER)
        .map_err(|err| supervisor_err("ITaskService::GetFolder", err))?;
    match folder.GetTask(TASK_NAME) {
        Ok(_task) => Ok(true),
        // A missing task is the normal not-installed state, not an error.
        Err(err) if is_not_found(err.raw()) => Ok(false),
        Err(err) => Err(supervisor_err("ITaskFolder::GetTask", err)),
    }
}

/// Register the `Patina Watcher` task from its definition XML in the current
/// user's folder (REQ-001).
fn register_task(xml: &str) -> Result<(), ServiceError> {
    let (_guard, service) = connect_service()?;
    let folder = service
        .GetFolder(TASK_FOLDER)
        .map_err(|err| supervisor_err("ITaskService::GetFolder", err))?;
    let definition = service
        .NewTask()
        .map_err(|err| supervisor_err("ITaskService::NewTask", err))?;
    definition
        .put_XmlText(xml)
        .map_err(|err| supervisor_err("ITaskDefinition::put_XmlText", err))?;

    // Register under the current interactive user's token (no stored password,
    // non-elevated). CREATE refuses to clobber an existing task — but `install`
    // has already screened for that with `task_exists`, so the AlreadyInstalled
    // error is surfaced before we get here.
    folder
        .RegisterTaskDefinition(
            Some(TASK_NAME),
            &definition,
            co::TASK_CREATION::CREATE,
            None,
            None,
            co::TASK_LOGON::INTERACTIVE_TOKEN,
            None,
        )
        .map_err(|err| supervisor_err("ITaskFolder::RegisterTaskDefinition", err))?;
    Ok(())
}

/// Delete the `Patina Watcher` task from the current user's folder.
fn delete_task() -> Result<(), ServiceError> {
    let (_guard, service) = connect_service()?;
    let folder = service
        .GetFolder(TASK_FOLDER)
        .map_err(|err| supervisor_err("ITaskService::GetFolder", err))?;
    folder
        .DeleteTask(TASK_NAME)
        .map_err(|err| supervisor_err("ITaskFolder::DeleteTask", err))?;
    Ok(())
}

/// The supervisor-derived fields read off the registered task's COM properties.
struct TaskStatus {
    /// Whether the task is registered.
    installed: bool,
    /// Whether the task's state is `RUNNING`.
    running: bool,
    /// The task's last-run time, if it has ever run.
    last_fired_at: Option<String>,
    /// The task's last result code, if recorded.
    last_exit_code: Option<i64>,
}

/// Query the registered task's liveness, last-run time, and last-result code.
///
/// A not-installed task reports `installed = false` with the other fields
/// cleared (parallel to the launchd / systemd `status` shape) rather than
/// erroring. The three COM property reads are mapped to status fields by the
/// pure [`map_task_status`] (the testable half, mirroring the siblings'
/// `parse_launchctl_print` / `parse_systemctl_show`).
fn query_task_status() -> Result<TaskStatus, ServiceError> {
    let (_guard, service) = connect_service()?;
    let folder = service
        .GetFolder(TASK_FOLDER)
        .map_err(|err| supervisor_err("ITaskService::GetFolder", err))?;
    let task = match folder.GetTask(TASK_NAME) {
        Ok(task) => task,
        Err(err) if is_not_found(err.raw()) => {
            return Ok(TaskStatus {
                installed: false,
                running: false,
                last_fired_at: None,
                last_exit_code: None,
            });
        }
        Err(err) => return Err(supervisor_err("ITaskFolder::GetTask", err)),
    };

    // A read error on any individual property degrades that field to its
    // never-run sentinel rather than failing the whole informational status
    // path: an unreadable state reads as not-running, an unreadable result as
    // never-run, an unreadable run-time as the epoch sentinel.
    let state = task.get_State().unwrap_or(co::TASK_STATE::UNKNOWN);
    let result = task
        .get_LastTaskResult()
        .unwrap_or(SCHED_S_TASK_HAS_NOT_RUN);
    let last_run = task.get_LastRunTime().unwrap_or(0.0);

    let readout = map_task_status(state, result, last_run);
    Ok(TaskStatus {
        installed: true,
        running: readout.running,
        last_fired_at: readout.last_fired_at,
        last_exit_code: readout.last_exit_code,
    })
}

/// The supervisor-derived fields mapped out of a registered task's three COM
/// reads — `get_State`, `get_LastTaskResult`, and `get_LastRunTime`.
struct TaskReadout {
    /// Whether the task's state is `RUNNING`.
    running: bool,
    /// The task's last-run time, if it has ever run.
    last_fired_at: Option<String>,
    /// The task's last result code, if recorded.
    last_exit_code: Option<i64>,
}

/// Map a registered task's `(get_State, get_LastTaskResult, get_LastRunTime)`
/// triple to the liveness / last-fired / last-exit-code status fields
/// (REQ-003, CHK-006).
///
/// This is the pure value-mapping half of [`query_task_status`], extracted —
/// mirroring the launchd / systemd siblings' `parse_launchctl_print` /
/// `parse_systemctl_show` — so the three predicates are unit-testable without a
/// live Task Scheduler:
///
/// - `state == RUNNING` is the liveness test; every other state is stopped.
/// - `result == SCHED_S_TASK_HAS_NOT_RUN` (the never-run sentinel) maps the
///   exit code to `None`; any other value is the recorded code (CHK-006).
/// - `date == 0.0` (the epoch sentinel a never-run task reports) maps the
///   last-fired time to `None` rather than rendering a meaningless 1899-12-30
///   timestamp.
fn map_task_status(state: co::TASK_STATE, result: i32, last_run: f64) -> TaskReadout {
    let running = state == co::TASK_STATE::RUNNING;
    let last_exit_code = (result != SCHED_S_TASK_HAS_NOT_RUN).then(|| i64::from(result));
    let last_fired_at = (last_run != 0.0).then(|| format!("{last_run}"));
    TaskReadout {
        running,
        last_fired_at,
        last_exit_code,
    }
}

/// The `SCHED_S_TASK_HAS_NOT_RUN` success-status code the Task Scheduler
/// reports as a task's last result before its first run.
const SCHED_S_TASK_HAS_NOT_RUN: i32 = 0x0004_1303;

/// Whether a `GetTask` `HRESULT` (as a raw `u32`) is a "task does not exist"
/// not-found code.
///
/// `GetTask` on a missing task returns `COR_E_FILENOTFOUND` /
/// `ERROR_FILE_NOT_FOUND` wrapped as an `HRESULT` (`0x8007_0002`); the Task
/// Scheduler also surfaces `ERROR_PATH_NOT_FOUND` (`0x8007_0003`) for a missing
/// task path. Treating both as not-found keeps a never-installed task off the
/// error path. Taking the raw `u32` (the caller passes `err.raw()`) keeps this
/// classification pure and unit-testable without constructing an `HRESULT` —
/// the workspace forbids `unsafe`, and `HRESULT::from_raw` is `unsafe`.
fn is_not_found(raw: u32) -> bool {
    const FILE_NOT_FOUND: u32 = 0x8007_0002;
    const PATH_NOT_FOUND: u32 = 0x8007_0003;
    raw == FILE_NOT_FOUND || raw == PATH_NOT_FOUND
}

/// Map a winsafe COM `HRESULT` to a [`ServiceError::Supervisor`] naming the
/// failing call, mirroring the `launchctl` / `systemctl` error mapping in the
/// sibling backends.
fn supervisor_err(call: &'static str, err: co::HRESULT) -> ServiceError {
    ServiceError::Supervisor(format!("{call} failed: {err}"))
}

/// Run a `schtasks` subcommand, mapping a non-zero exit to
/// [`ServiceError::Supervisor`] with the captured stderr.
///
/// `start` / `stop` drive the task's run / end action through `schtasks`
/// because this `winsafe` version exposes no `IRegisteredTask::Run`; the
/// registration and deletion go through the `taskschd` COM API directly. This
/// mirrors the systemd backend shelling out to `systemctl` for the
/// liveness-changing actions.
fn schtasks(args: &[&str]) -> Result<std::process::Output, ServiceError> {
    let output = Command::new("schtasks")
        .args(args)
        .output()
        .map_err(|source| ServiceError::Supervisor(format!("failed to run schtasks: {source}")))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(ServiceError::Supervisor(format!(
            "schtasks {} failed: {}",
            args.join(" "),
            stderr.trim()
        )))
    }
}

/// Render the Task Scheduler 2.0 registration XML for the watcher task: an
/// `OnLogon` trigger, a least-privilege (`RunLevel = Limited`) principal, and
/// an `Exec` action of the canonical `binary` plus `watch --foreground`
/// (REQ-001 / CHK-003).
///
/// Each path / argument is XML-escaped through [`xml_escape`] so a binary path
/// containing `&`, `<`, `>`, `"`, or `'` produces a well-formed document,
/// mirroring how the `launchd` sibling escapes the same path for its plist.
fn render_task_xml(binary: &Utf8Path) -> String {
    // The Exec action splits the command (the binary) from its arguments; the
    // foreground tokens join into a single space-separated `<Arguments>`.
    let command = xml_escape(binary.as_str());
    let arguments = xml_escape(&FOREGROUND_ARGS.join(" "));

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
\t<RegistrationInfo>\n\
\t\t<Description>Patina dotfile watcher</Description>\n\
\t</RegistrationInfo>\n\
\t<Triggers>\n\
\t\t<LogonTrigger>\n\
\t\t\t<Enabled>true</Enabled>\n\
\t\t</LogonTrigger>\n\
\t</Triggers>\n\
\t<Principals>\n\
\t\t<Principal id=\"Author\">\n\
\t\t\t<LogonType>InteractiveToken</LogonType>\n\
\t\t\t<RunLevel>LeastPrivilege</RunLevel>\n\
\t\t</Principal>\n\
\t</Principals>\n\
\t<Settings>\n\
\t\t<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
\t\t<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
\t\t<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
\t\t<StartWhenAvailable>true</StartWhenAvailable>\n\
\t\t<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
\t\t<Enabled>true</Enabled>\n\
\t</Settings>\n\
\t<Actions Context=\"Author\">\n\
\t\t<Exec>\n\
\t\t\t<Command>{command}</Command>\n\
\t\t\t<Arguments>{arguments}</Arguments>\n\
\t\t</Exec>\n\
\t</Actions>\n\
</Task>\n"
    )
}

/// Escape the five XML special characters so a binary path or argument
/// containing `&`, `<`, `>`, `"`, or `'` produces a well-formed task document.
///
/// Mirrors the `launchd` sibling's `xml_escape`; the two are not shared because
/// each is `#[cfg]`-gated to its own target OS and a cross-OS `pub(super)`
/// helper would have to compile (and be reachable) on the platform whose
/// backend is excluded.
fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_task_xml_declares_the_logon_trigger_run_level_and_exec_action() {
        let binary = Utf8Path::new(r"C:\Users\dev\bin\patina.exe");
        let xml = render_task_xml(binary);

        // CHK-003: a logon trigger, a non-elevated (least-privilege) run level,
        // and an Exec action pointing at the canonical binary plus the
        // foreground tokens.
        assert!(
            xml.contains("<LogonTrigger>"),
            "the task must carry an OnLogon trigger, got: {xml}"
        );
        assert!(
            xml.contains("<RunLevel>LeastPrivilege</RunLevel>"),
            "the task must run at least-privilege (RunLevel = Limited), got: {xml}"
        );
        assert!(
            xml.contains(r"<Command>C:\Users\dev\bin\patina.exe</Command>"),
            "the Exec command must be the canonical binary, got: {xml}"
        );
        assert!(
            xml.contains("<Arguments>watch --foreground</Arguments>"),
            "the Exec arguments must be the foreground tokens, got: {xml}"
        );
    }

    #[test]
    fn render_task_xml_is_well_formed_task_scheduler_2_0_xml() {
        let xml = render_task_xml(Utf8Path::new(r"C:\bin\patina.exe"));
        assert!(xml.starts_with("<?xml version=\"1.0\""));
        // The 2.0 schema namespace is what the Task Scheduler validates the
        // registration XML against; an absent / wrong namespace is rejected at
        // RegisterTaskDefinition.
        assert!(
            xml.contains(
                "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">"
            ),
            "the task must declare the Task Scheduler 2.0 schema, got: {xml}"
        );
        assert!(xml.trim_end().ends_with("</Task>"));
    }

    #[test]
    fn render_task_xml_escapes_special_characters_in_the_binary_path() {
        // A path with XML metacharacters must land escaped so the registration
        // document stays well-formed (and a `<` in a path cannot inject a fresh
        // element).
        let binary = Utf8Path::new(r"C:\a&b\<c>\patina.exe");
        let xml = render_task_xml(binary);
        assert!(
            xml.contains(r"<Command>C:\a&amp;b\&lt;c&gt;\patina.exe</Command>"),
            "metacharacters in the path must be XML-escaped, got: {xml}"
        );
        // The raw, unescaped `<c>` must not appear as a literal element.
        assert!(
            !xml.contains(r"<c>"),
            "an unescaped angle bracket must not survive into the document, got: {xml}"
        );
    }

    #[test]
    fn xml_escape_escapes_the_five_special_characters() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        // A plain Windows path with a backslash and space needs no escaping.
        assert_eq!(
            xml_escape(r"C:\Program Files\patina.exe"),
            r"C:\Program Files\patina.exe"
        );
    }

    #[test]
    fn is_not_found_recognizes_the_task_not_found_hresults() {
        // FILE_NOT_FOUND (0x80070002) and PATH_NOT_FOUND (0x80070003) are the
        // not-installed states; any other HRESULT raw code is a real supervisor
        // error. The classifier takes the raw `u32` (the call site passes
        // `err.raw()`) so it is testable without the `unsafe` `HRESULT::from_raw`.
        assert!(is_not_found(0x8007_0002));
        assert!(is_not_found(0x8007_0003));
        // ACCESS_DENIED (0x80070005) is a real error, not a not-found.
        assert!(!is_not_found(0x8007_0005));
        // S_OK (0) is success, not a not-found.
        assert!(!is_not_found(0));
    }

    #[test]
    fn map_task_status_reports_running_for_the_running_state() {
        // RUNNING liveness with a recorded last run: running, and the recorded
        // exit code / timestamp surface (REQ-003 status shape).
        let readout = map_task_status(co::TASK_STATE::RUNNING, 0, 45_000.5);
        assert!(
            readout.running,
            "TASK_STATE::RUNNING must map to running = true"
        );
        assert_eq!(readout.last_exit_code, Some(0));
        assert_eq!(readout.last_fired_at.as_deref(), Some("45000.5"));
    }

    #[test]
    fn map_task_status_treats_a_non_running_state_as_stopped() {
        // An installed-but-idle task reports running = false while still
        // surfacing its recorded non-zero last exit code.
        let readout = map_task_status(co::TASK_STATE::READY, 2, 45_000.0);
        assert!(
            !readout.running,
            "a non-RUNNING state must map to running = false"
        );
        assert_eq!(readout.last_exit_code, Some(2));
        assert_eq!(readout.last_fired_at.as_deref(), Some("45000"));
    }

    #[test]
    fn map_task_status_reports_none_exit_code_when_never_run() {
        // CHK-006: a task that has never run reports the SCHED_S_TASK_HAS_NOT_RUN
        // sentinel result and the epoch (0.0) last-run time; both map to None so
        // a freshly-installed task surfaces no exit code and no last-fired time.
        let readout = map_task_status(co::TASK_STATE::READY, SCHED_S_TASK_HAS_NOT_RUN, 0.0);
        assert!(!readout.running);
        assert_eq!(
            readout.last_exit_code, None,
            "the never-run sentinel must map the exit code to None (CHK-006)"
        );
        assert_eq!(
            readout.last_fired_at, None,
            "the epoch sentinel must map the last-fired time to None"
        );
    }
}
