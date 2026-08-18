//! The `apply-defender-exclusions` action: add and remove Windows Defender
//! path exclusions under one-time UAC elevation.
//!
//! The unprivileged `patina` CLI derives and previews the exclusion set, takes
//! consent, writes a request file, and launches this helper with its absolute
//! path. The helper re-validates every path before it calls PowerShell's
//! `Defender` module and verifies the change with a mandatory re-read.
//!
//! On non-Windows targets, the apply returns [`DefenderError::NotWindows`]
//! without side effects. The parser and validator remain cross-platform.
//!
//! ## Reporting the verdict back
//!
//! The helper writes a result file beside the request file because the
//! launching CLI cannot read the elevated process's exit code or Defender
//! state. The CLI polls this file, and the helper writes a verdict for every
//! terminal path.
//!
//! ## Duplicated constants
//!
//! The helper owns copies of the system-directory names, result-file name, and
//! verdict tokens; keep these protocol values synchronized with the CLI.

use std::fmt;
use std::path::Path;

const SYSTEM_DIR_ENV_VARS: [&str; 4] = [
    "SystemRoot",
    "ProgramFiles",
    "ProgramW6432",
    "ProgramFiles(x86)",
];

#[cfg(windows)]
const RESULT_FILENAME: &str = "defender-result.txt";

// Write the temporary body before renaming it over the polled result file.
#[cfg(windows)]
const RESULT_TMP_FILENAME: &str = "defender-result.txt.tmp";

const RECEIPT_APPLIED: &str = "applied";

const RECEIPT_BLOCKED: &str = "blocked";

const RECEIPT_FAILED: &str = "failed";

/// Failure modes of [`apply_defender_exclusions`].
#[derive(Debug)]
pub enum DefenderError {
    /// The action was invoked on a non-Windows build.
    NotWindows,

    /// A request-file line did not begin with the `A <path>` or `R <path>`
    /// prefix.
    MalformedRequest {
        /// The offending line, verbatim.
        line: String,
    },

    /// A path in the request failed the independent re-validation.
    InvalidPath {
        /// The offending path.
        path: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// The request file could not be read.
    #[cfg(windows)]
    ReadRequest {
        /// The request path the helper was given.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// Spawning the PowerShell process failed.
    #[cfg(windows)]
    PowerShell {
        /// The spawn error.
        source: std::io::Error,
    },

    /// Defender accepted the call, but the re-read shows the requested add or
    /// remove never took effect: Tamper Protection or a management policy
    /// rejected the write silently. Only this variant means the user should be
    /// told Defender refused their change.
    Blocked {
        /// The script's detail, naming the specific paths and the live
        /// Tamper-Protection status.
        detail: String,
    },

    /// PowerShell ran, but the apply-and-verify script failed for some
    /// other reason. `Add`/`Remove-MpPreference` errored, or the script
    /// itself did.
    #[cfg(windows)]
    Apply {
        /// The script's stderr.
        detail: String,
    },
}

impl fmt::Display for DefenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWindows => write!(
                f,
                "apply-defender-exclusions is a Windows-only action; this binary was not built for Windows"
            ),
            Self::MalformedRequest { line } => {
                write!(
                    f,
                    "malformed request line (expected `A <path>` or `R <path>`): {line}"
                )
            }
            Self::InvalidPath { path, reason } => {
                write!(f, "refusing to exclude `{path}`: {reason}")
            }
            Self::Blocked { detail } => {
                write!(f, "Defender rejected the exclusion change: {detail}")
            }
            #[cfg(windows)]
            Self::ReadRequest { path, source } => {
                write!(
                    f,
                    "failed to read the request file `{}`: {source}",
                    path.display()
                )
            }
            #[cfg(windows)]
            Self::PowerShell { source } => write!(f, "failed to run powershell: {source}"),
            #[cfg(windows)]
            Self::Apply { detail } => {
                write!(f, "Defender exclusions were not applied: {detail}")
            }
        }
    }
}

impl std::error::Error for DefenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotWindows
            | Self::MalformedRequest { .. }
            | Self::InvalidPath { .. }
            | Self::Blocked { .. } => None,
            #[cfg(windows)]
            Self::ReadRequest { source, .. } | Self::PowerShell { source } => Some(source),
            #[cfg(windows)]
            Self::Apply { .. } => None,
        }
    }
}

/// Parse non-empty `A <path>` and `R <path>` lines into add and remove lists.
///
/// # Errors
///
/// Returns [`DefenderError::MalformedRequest`] on a line without a recognized
/// prefix.
pub fn parse_request(content: &str) -> Result<(Vec<String>, Vec<String>), DefenderError> {
    let mut adds = Vec::new();
    let mut removes = Vec::new();
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("A ") {
            adds.push(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("R ") {
            removes.push(rest.to_owned());
        } else {
            return Err(DefenderError::MalformedRequest {
                line: line.to_owned(),
            });
        }
    }
    Ok((adds, removes))
}

/// Re-validate one exclusion path, independently of the CLI.
///
/// Accept drive-letter-absolute paths outside protected system directories.
/// Reject UNC paths, drive roots, and wildcard paths before the helper calls
/// Defender.
///
/// # Errors
///
/// Returns [`DefenderError::InvalidPath`] naming the first rule the path
/// violates.
pub fn validate_exclusion_path(path: &str) -> Result<(), DefenderError> {
    validate_exclusion_path_with(path, &system_dir_denylist())
}

fn validate_exclusion_path_with(path: &str, system_dirs: &[String]) -> Result<(), DefenderError> {
    let invalid = |reason: &'static str| DefenderError::InvalidPath {
        path: path.to_owned(),
        reason,
    };
    if path.is_empty() {
        return Err(invalid("the path is empty"));
    }
    if path.contains('*') || path.contains('?') {
        return Err(invalid("wildcards are not allowed; only exact paths"));
    }
    if path.starts_with("\\\\") {
        return Err(invalid("UNC paths are not allowed"));
    }
    if !is_windows_absolute(path) {
        return Err(invalid("the path is not an absolute Windows path"));
    }
    if is_drive_root(path) {
        return Err(invalid("refusing to exclude an entire drive root"));
    }
    for dir in system_dirs {
        if is_within(path, dir) {
            return Err(invalid("the path is inside a protected system directory"));
        }
    }
    Ok(())
}

fn system_dir_denylist() -> Vec<String> {
    SYSTEM_DIR_ENV_VARS
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
        .collect()
}

fn is_windows_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(&drive), Some(&b':'), Some(&sep)) = (bytes.first(), bytes.get(1), bytes.get(2))
    else {
        return false;
    };
    drive.is_ascii_alphabetic() && (sep == b'\\' || sep == b'/')
}

fn is_drive_root(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(&drive), Some(&b':')) = (bytes.first(), bytes.get(1)) else {
        return false;
    };
    drive.is_ascii_alphabetic()
        && bytes
            .get(2..)
            .unwrap_or_default()
            .iter()
            .all(|&b| b == b'\\' || b == b'/')
}

fn is_within(path: &str, dir: &str) -> bool {
    let p = normalize(path);
    let d = normalize(dir);
    if d.is_empty() {
        return false;
    }
    p == d || p.starts_with(&format!("{d}\\"))
}

fn normalize(s: &str) -> String {
    let unified: String = s.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
    unified.trim_end_matches('\\').to_ascii_lowercase()
}

/// Apply request-file exclusions and write the result beside the request.
///
/// Reads the given absolute request path, re-validates every path, then runs
/// one PowerShell invocation that batches `Add-MpPreference` and
/// `Remove-MpPreference` and verifies the result with a re-read. The request
/// path remains authoritative across `runas` identities; the helper does not
/// recompute the state directory.
///
/// Write a result for every terminal outcome.
///
/// # Errors
///
/// Returns [`DefenderError`] when the request cannot be read or parsed, a path
/// fails re-validation, PowerShell cannot be spawned, or the apply-and-verify
/// script reports the exclusions did not take.
#[cfg(windows)]
pub fn apply_defender_exclusions(request: &Path) -> Result<(), DefenderError> {
    let outcome = apply_from_request(request);
    write_receipt(request, &outcome);
    outcome
}

#[cfg(windows)]
fn apply_from_request(request: &Path) -> Result<(), DefenderError> {
    let content =
        std::fs::read_to_string(request).map_err(|source| DefenderError::ReadRequest {
            path: request.to_path_buf(),
            source,
        })?;
    let (adds, removes) = parse_request(&content)?;
    let system_dirs = system_dir_denylist();
    for path in adds.iter().chain(&removes) {
        validate_exclusion_path_with(path, &system_dirs)?;
    }
    run_apply_and_verify(request)
}

/// Write the verdict beside the request file for the launching CLI to poll.
///
/// A scratch file and rename keep polls from reading a partial verdict. A
/// failed write remains silent because the helper has no console; the CLI
/// reports an unconfirmed outcome after its deadline.
#[cfg(windows)]
fn write_receipt(request: &Path, outcome: &Result<(), DefenderError>) {
    let tmp = request.with_file_name(RESULT_TMP_FILENAME);
    if std::fs::write(&tmp, receipt_body(outcome)).is_ok() {
        drop(std::fs::rename(
            &tmp,
            request.with_file_name(RESULT_FILENAME),
        ));
    }
}

/// Render an outcome as the result file's single-line body.
///
/// Return `<verdict>` or `<verdict> <detail>` as one line.
#[must_use = "the launching CLI reads this body as the verdict"]
pub fn receipt_body(outcome: &Result<(), DefenderError>) -> String {
    match outcome {
        Ok(()) => format!("{RECEIPT_APPLIED}\n"),
        Err(DefenderError::Blocked { detail }) => {
            format!("{RECEIPT_BLOCKED} {}\n", one_line(detail))
        }
        Err(other) => format!("{RECEIPT_FAILED} {}\n", one_line(&other.to_string())),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Return [`DefenderError::NotWindows`] on non-Windows targets.
///
/// # Errors
///
/// Always returns [`DefenderError::NotWindows`].
#[cfg(not(windows))]
pub fn apply_defender_exclusions(_request: &Path) -> Result<(), DefenderError> {
    Err(DefenderError::NotWindows)
}

#[cfg(windows)]
fn run_apply_and_verify(request: &Path) -> Result<(), DefenderError> {
    use std::process::Command;

    let literal = single_quote(&request.display().to_string());
    let script = apply_script(&literal);

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|source| DefenderError::PowerShell { source })?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    match blocked_detail(&stderr) {
        Some(detail) => Err(DefenderError::Blocked { detail }),
        None => Err(DefenderError::Apply { detail: stderr }),
    }
}

#[cfg(windows)]
const BLOCKED_MARKER: &str = "PATINA-BLOCKED";

#[cfg(windows)]
fn blocked_detail(stderr: &str) -> Option<String> {
    let (_, after_marker) = stderr.split_once(BLOCKED_MARKER)?;
    Some(
        after_marker
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned(),
    )
}

#[cfg(windows)]
fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn apply_script(request_literal: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; \
         $req = Get-Content -LiteralPath {request_literal}; \
         $adds = @(); $removes = @(); \
         foreach ($line in $req) {{ \
           if ($line.Length -lt 2) {{ continue }} \
           $op = $line.Substring(0,1); $path = $line.Substring(2); \
           if ($op -eq 'A') {{ $adds += $path }} elseif ($op -eq 'R') {{ $removes += $path }} \
         }}; \
         if ($adds.Count -gt 0) {{ Add-MpPreference -ExclusionPath $adds -ErrorAction Stop }}; \
         if ($removes.Count -gt 0) {{ Remove-MpPreference -ExclusionPath $removes -ErrorAction Stop }}; \
         $current = @(Get-MpPreference | Select-Object -ExpandProperty ExclusionPath) | \
           ForEach-Object {{ $_.TrimEnd('\\').ToLowerInvariant() }}; \
         $missing = @(); $lingering = @(); \
         foreach ($p in $adds) {{ if ($current -notcontains $p.TrimEnd('\\').ToLowerInvariant()) {{ $missing += $p }} }}; \
         foreach ($p in $removes) {{ if ($current -contains $p.TrimEnd('\\').ToLowerInvariant()) {{ $lingering += $p }} }}; \
         if ($missing.Count -gt 0 -or $lingering.Count -gt 0) {{ \
           $s = Get-MpComputerStatus; \
           throw \"{BLOCKED_MARKER} exclusions not applied (TamperProtected=$($s.IsTamperProtected), RunningMode=$($s.AMRunningMode)); not added: $($missing -join ', '); not removed: $($lingering -join ', ')\" \
         }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_add_and_remove_lines() {
        let (adds, removes) = parse_request("A C:\\a\nR C:\\b\nA C:\\c\n").expect("parse");
        assert_eq!(adds, vec!["C:\\a".to_owned(), "C:\\c".to_owned()]);
        assert_eq!(removes, vec!["C:\\b".to_owned()]);
    }

    #[test]
    fn parse_preserves_paths_with_spaces() {
        let (adds, _) = parse_request("A C:\\Program Files\\app\n").expect("parse");
        assert_eq!(adds, vec!["C:\\Program Files\\app".to_owned()]);
    }

    #[test]
    fn parse_rejects_a_line_without_a_prefix() {
        assert!(matches!(
            parse_request("garbage line"),
            Err(DefenderError::MalformedRequest { .. })
        ));
    }

    #[test]
    fn validator_accepts_a_normal_absolute_path() {
        validate_exclusion_path("C:\\Users\\kevin\\.gitconfig")
            .expect("a normal absolute path validates");
    }

    #[test]
    fn validator_rejects_the_same_categories_as_core() {
        for bad in [
            "",
            "\\\\server\\share\\x",
            "C:\\Users\\*",
            "\\Users\\x",
            "C:relative",
            "C:\\",
        ] {
            validate_exclusion_path(bad).expect_err(&format!(
                "`{bad}` must be rejected by the independent validator"
            ));
        }
        let system_dirs = vec!["C:\\Windows".to_owned()];
        assert!(matches!(
            validate_exclusion_path_with("C:\\Windows\\System32\\x", &system_dirs),
            Err(DefenderError::InvalidPath { .. })
        ));
        validate_exclusion_path_with("C:\\WindowsApps\\x", &system_dirs)
            .expect("a shared prefix that is not a component boundary must not match");
    }

    #[test]
    fn parse_skips_blank_lines_between_entries() {
        let (adds, removes) = parse_request("A C:\\a\n\nR C:\\b\n").expect("parse");
        assert_eq!(adds, vec!["C:\\a".to_owned()]);
        assert_eq!(removes, vec!["C:\\b".to_owned()]);
    }

    #[test]
    fn cross_platform_errors_render_and_carry_no_source() {
        let malformed = DefenderError::MalformedRequest {
            line: "bogus".to_owned(),
        };
        assert!(malformed.to_string().contains("bogus"));

        let invalid = DefenderError::InvalidPath {
            path: "C:\\x".to_owned(),
            reason: "wildcards are not allowed; only exact paths",
        };
        let rendered = invalid.to_string();
        assert!(rendered.contains("C:\\x") && rendered.contains("wildcards"));

        for err in [malformed, invalid, DefenderError::NotWindows] {
            assert!(
                std::error::Error::source(&err).is_none(),
                "cross-platform variants wrap no source"
            );
        }
    }

    #[test]
    fn is_drive_root_guards_input_without_a_drive_and_colon() {
        assert!(is_drive_root("C:"));
        assert!(is_drive_root("C:\\"));
        assert!(!is_drive_root("C"));
        assert!(!is_drive_root("C:\\Users"));
    }

    #[test]
    fn is_within_is_false_for_an_empty_directory() {
        assert!(!is_within("C:\\Users\\kevin", ""));
        assert!(is_within("C:\\Users\\kevin\\x", "C:\\Users\\kevin"));
    }

    #[test]
    fn receipt_body_marks_success_with_the_applied_verdict_alone() {
        assert_eq!(receipt_body(&Ok(())), "applied\n");
    }

    #[test]
    fn receipt_body_distinguishes_a_rejected_write_from_any_other_failure() {
        let blocked = receipt_body(&Err(DefenderError::Blocked {
            detail: "exclusions not applied (TamperProtected=True)".to_owned(),
        }));
        assert_eq!(
            blocked,
            "blocked exclusions not applied (TamperProtected=True)\n"
        );

        let failed = receipt_body(&Err(DefenderError::MalformedRequest {
            line: "garbage".to_owned(),
        }));
        assert!(
            failed.starts_with("failed "),
            "a non-Defender failure must not claim Defender rejected it: {failed}"
        );
    }

    #[test]
    fn receipt_body_is_a_single_line_even_for_a_multi_line_detail() {
        let body = receipt_body(&Err(DefenderError::Blocked {
            detail: "not added:\n  C:\\a\n  C:\\b".to_owned(),
        }));
        assert_eq!(body, "blocked not added: C:\\a C:\\b\n");
        assert_eq!(body.lines().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn blocked_detail_is_found_inside_powershells_error_rendering() {
        let stderr = "\
C:\\script.ps1 : PATINA-BLOCKED exclusions not applied (TamperProtected=True)
At line:1 char:1
+ powershell -Command ...
    + CategoryInfo : OperationStopped";
        assert_eq!(
            blocked_detail(stderr).as_deref(),
            Some("exclusions not applied (TamperProtected=True)")
        );
    }

    #[cfg(windows)]
    #[test]
    fn blocked_detail_is_absent_for_an_unrelated_failure() {
        assert_eq!(
            blocked_detail("Add-MpPreference : The service cannot be started"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_apply_script_emits_the_marker_the_parser_looks_for() {
        assert!(apply_script("'C:\\request.txt'").contains(BLOCKED_MARKER));
    }
}
