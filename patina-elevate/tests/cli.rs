//! Process-level integration tests for the `patina-elevate` binary.
//!
//! These assert the real exit codes the spawned process produces: the
//! arg-parsing contract that the library unit tests cover in-process, here
//! proven end-to-end through `clap`'s own `Error::exit`.
//!
//! The binary is gated behind the `windows` feature, so
//! it is only built when that feature is enabled. When it is absent (a plain
//! `cargo test` on any host without `--features windows`) the
//! process-spawning tests no-op. Run them with
//! `cargo test -p patina-elevate --features windows`.

use std::process::Command;

/// Path to the built `patina-elevate` binary, or `None` when the bin was not
/// built (the `windows` feature was off, so Cargo skipped it).
///
/// Cargo sets `CARGO_BIN_EXE_patina-elevate` at compile time even when the
/// bin's `required-features` (`windows`) are off and the bin was
/// never produced, so the compile-time env var alone is not a reliable
/// "was it built" signal. Guard on the file actually existing on disk;
/// otherwise a plain `cargo test` (no `--features windows`) would spawn a
/// non-existent path and panic instead of no-opping as intended.
fn elevate_bin() -> Option<&'static str> {
    let path = option_env!("CARGO_BIN_EXE_patina-elevate")?;
    std::path::Path::new(path).exists().then_some(path)
}

#[test]
fn unknown_subcommand_exits_2() {
    // An unsupported subcommand exits 2 and prints a usage
    // message listing `enable-developer-mode`. The process surfaces the clap
    // usage error as exit code 2, and `parse_or_exit` appends the
    // supported-subcommand listing to that error's stderr so the named
    // scenario's "listing" half is gated on the exit-2 path itself.
    let Some(bin) = elevate_bin() else {
        return;
    };
    let out = Command::new(bin)
        .arg("frobnicate")
        .output()
        .expect("spawn patina-elevate");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unknown subcommand must exit 2; stderr: {stderr}"
    );
    assert!(
        stderr.contains("enable-developer-mode"),
        "the exit-2 usage message must list the supported subcommand; got:\n{stderr}"
    );
}

#[test]
fn help_lists_the_supported_subcommand() {
    // The usage surface lists `enable-developer-mode` so a
    // mis-invoking caller can discover the correct subcommand. `--help` is
    // where clap enumerates subcommands; it exits 0.
    let Some(bin) = elevate_bin() else {
        return;
    };
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .expect("spawn patina-elevate");
    assert_eq!(out.status.code(), Some(0), "`--help` exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("enable-developer-mode"),
        "help must list the supported subcommand; got:\n{stdout}"
    );
}

#[cfg(not(windows))]
#[test]
fn enable_developer_mode_off_windows_exits_1() {
    // On a non-Windows build the registry write does not exist, so the action
    // takes the typed NotWindows failure path: exit 1 with a message on
    // stderr. (On Windows this path instead performs the real write, covered
    // by the `#[ignore]` host test below.)
    let Some(bin) = elevate_bin() else {
        return;
    };
    let out = Command::new(bin)
        .arg("enable-developer-mode")
        .output()
        .expect("spawn patina-elevate");
    assert_eq!(
        out.status.code(),
        Some(1),
        "enable-developer-mode off Windows must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stderr.is_empty(),
        "the failure must carry a message on stderr"
    );
}

/// An elevated `patina-elevate.exe enable-developer-mode` sets the
/// registry flag to `1` and exits `0`. Gated `#[cfg(windows)]` `#[ignore]`
/// because CI is not Windows and the path needs a real UAC accept against a
/// machine whose Developer Mode is OFF. Neither is available in automation.
/// Run by hand on an elevated Windows shell with `--ignored`.
#[cfg(windows)]
#[test]
#[ignore = "needs an elevated Windows host with Developer Mode OFF and a real UAC accept"]
fn enable_developer_mode_elevated_sets_flag_and_exits_0() {
    let bin = elevate_bin().expect("the bin is built on Windows under --features windows");
    let out = Command::new(bin)
        .arg("enable-developer-mode")
        .output()
        .expect("spawn patina-elevate");
    assert_eq!(
        out.status.code(),
        Some(0),
        "an elevated enable-developer-mode must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Re-read the flag through the same key the helper wrote to.
    let flag =
        read_dev_mode_flag().expect("read AllowDevelopmentWithoutDevLicense after the write");
    assert_eq!(flag, Some(1), "the Developer Mode flag must read back as 1");
}

/// The elevated `apply-defender-exclusions` action adds a path exclusion, its
/// mandatory re-read confirms the write took (so the process exits `0` and
/// records `applied`), and a follow-up removal clears it again. Gated
/// `#[cfg(windows)]` `#[ignore]` because it needs an elevated Windows host with
/// an active, unmanaged Defender, which CI does not provide. On a
/// Tamper-Protected or policy-managed host the add instead exits `1` and
/// records `blocked`, since
/// the re-read shows the exclusion never appeared. Run by hand on an elevated
/// Windows shell with `--ignored`.
///
/// The result file is asserted alongside the exit code because it, not the exit
/// code, is what the unprivileged CLI actually reads. `ShellExecuteEx` gives
/// the launcher no way to collect a child's status.
#[cfg(windows)]
#[test]
#[ignore = "needs an elevated Windows host with an active, unmanaged Defender"]
fn apply_defender_exclusions_adds_then_removes_a_path() {
    let bin = elevate_bin().expect("the bin is built on Windows under --features windows");
    let dir = tempfile::tempdir().expect("create a temp dir for the exclusion and request");
    let excluded = dir.path().join("patina-defender-it");
    std::fs::create_dir_all(&excluded).expect("create the directory to exclude");
    let excluded = excluded.to_string_lossy().into_owned();
    let receipt = dir.path().join("defender-result.txt");

    let run = |body: &str| {
        let request = dir.path().join("request.txt");
        std::fs::write(&request, body).expect("write the request file");
        Command::new(bin)
            .arg("apply-defender-exclusions")
            .arg(&request)
            .output()
            .expect("spawn patina-elevate")
    };

    let add = run(&format!("A {excluded}\n"));
    assert_eq!(
        add.status.code(),
        Some(0),
        "adding an exclusion must exit 0 after the re-read verification; stderr: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&receipt).expect("the helper must record its verdict"),
        "applied\n",
        "a confirmed add must be recorded as `applied` for the launching CLI"
    );

    let remove = run(&format!("R {excluded}\n"));
    assert_eq!(
        remove.status.code(),
        Some(0),
        "removing the exclusion must exit 0 after the re-read verification; stderr: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&receipt).expect("the helper must record its verdict"),
        "applied\n"
    );
}

/// A request the helper refuses records `failed`, not `blocked`: the launching
/// CLI must not tell the user Defender rejected a change Defender never saw.
///
/// Needs no elevation and no Defender, because the path is rejected by the
/// helper's own validator before any cmdlet runs, so unlike the test above this
/// one runs unattended in CI on Windows.
///
/// It skips when the helper binary is absent, like the argument-parsing tests
/// above: the bin exists only under `--features windows`, and CI's per-OS test
/// leg runs a bare `cargo test --workspace`.
#[cfg(windows)]
#[test]
fn a_refused_path_is_recorded_as_failed_not_blocked() {
    let Some(bin) = elevate_bin() else {
        return;
    };
    let dir = tempfile::tempdir().expect("create a temp dir for the request");
    let request = dir.path().join("request.txt");
    // A drive root: refused by the validator, so the run never reaches Defender.
    std::fs::write(&request, "A C:\\\n").expect("write the request file");

    let output = Command::new(bin)
        .arg("apply-defender-exclusions")
        .arg(&request)
        .output()
        .expect("spawn patina-elevate");
    assert_eq!(output.status.code(), Some(1));

    let receipt = std::fs::read_to_string(dir.path().join("defender-result.txt"))
        .expect("a refused request must still be recorded");
    assert!(
        receipt.starts_with("failed "),
        "a refused path is not Defender rejecting the write: {receipt}"
    );
    assert_eq!(
        receipt.lines().count(),
        1,
        "the verdict must be one line: {receipt}"
    );
}

/// Read the Developer Mode DWORD back out for the assertion above.
/// Duplicated read (the helper must not depend on `patina-core`).
#[cfg(windows)]
fn read_dev_mode_flag() -> Result<Option<u32>, Box<dyn std::error::Error>> {
    use winsafe::co;

    const DEV_MODE_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock";
    const DEV_MODE_VALUE: &str = "AllowDevelopmentWithoutDevLicense";

    let key = winsafe::HKEY::LOCAL_MACHINE.RegOpenKeyEx(
        Some(DEV_MODE_KEY),
        co::REG_OPTION::default(),
        co::KEY::READ,
    )?;
    match key.RegQueryValueEx(Some(DEV_MODE_VALUE))? {
        winsafe::RegistryValue::Dword(value) => Ok(Some(value)),
        _ => Ok(None),
    }
}
