use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::process::Command;
use std::sync::OnceLock;

pub(crate) fn executable() -> &'static Utf8Path {
    static EXECUTABLE: OnceLock<Utf8PathBuf> = OnceLock::new();

    EXECUTABLE.get_or_init(|| {
        let workspace = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace member has a parent directory");
        let target = Utf8Path::new(env!("CARGO_TARGET_TMPDIR")).join("lock-helper");
        let output = Command::new(env!("CARGO"))
            .current_dir(workspace)
            .args([
                "build",
                "--quiet",
                "--locked",
                "--package",
                "patina-core",
                "--example",
                "lock_helper",
                "--message-format=json",
                "--target-dir",
            ])
            .arg(target)
            .output()
            .expect("spawn cargo build for lock_helper example");
        assert!(
            output.status.success(),
            "building lock_helper example failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout)
            .expect("Cargo JSON output is UTF-8")
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("Cargo JSON message")
            })
            .find_map(|message| {
                if message["reason"] == "compiler-artifact"
                    && message["target"]["name"] == "lock_helper"
                {
                    message["executable"].as_str().map(Utf8PathBuf::from)
                } else {
                    None
                }
            })
            .expect("Cargo reported the lock_helper executable")
    })
}
