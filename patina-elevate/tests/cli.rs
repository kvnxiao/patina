//! Integration tests for cli.

use std::process::Command;

fn elevate_bin() -> Option<&'static str> {
    let path = option_env!("CARGO_BIN_EXE_patina-elevate")?;
    std::path::Path::new(path).exists().then_some(path)
}

#[test]
fn unknown_subcommand_exits_2() {
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
    let flag =
        read_dev_mode_flag().expect("read AllowDevelopmentWithoutDevLicense after the write");
    assert_eq!(flag, Some(1), "the Developer Mode flag must read back as 1");
}

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

#[cfg(windows)]
#[test]
fn a_refused_path_is_recorded_as_failed_not_blocked() {
    let Some(bin) = elevate_bin() else {
        return;
    };
    let dir = tempfile::tempdir().expect("create a temp dir for the request");
    let request = dir.path().join("request.txt");
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
