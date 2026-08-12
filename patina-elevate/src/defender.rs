//! The `apply-defender-exclusions` action: add and remove Windows Defender
//! path exclusions under one-time UAC elevation.
//!
//! The unprivileged `patina` CLI derives, previews, and gets consent for the
//! exact exclusion set, writes it to a request file in the per-machine state
//! directory, then launches this helper elevated with the absolute request path
//! as its one argument. The helper reads that file, **independently
//! re-validates every path** (this is the trust boundary: the CLI's validation
//! is not trusted here), and applies the add/remove through PowerShell's
//! `Defender` module, verifying with a mandatory re-read that the change
//! actually took.
//!
//! The real work is `#[cfg(windows)]`-gated. On any other host the action
//! returns [`DefenderError::NotWindows`] without side effects, which keeps the
//! argument-parsing surface and the pure validator/parser exercisable by the
//! cross-platform tests.
//!
//! ## Reporting the verdict back
//!
//! The unprivileged CLI cannot check this helper's work: `Get-MpPreference`
//! withholds the exclusion list from an unelevated caller, so a verification
//! attempted there can only ever conclude "not applied". Nor can it read this
//! process's exit code, because `ShellExecuteEx` hands the launcher no handle
//! to wait on.
//!
//! So the verdict travels through a **result file** written beside the request
//! file, which the CLI polls for. It is written on every terminal path, so a
//! clean failure reaches the user as itself rather than as a timeout, and it
//! distinguishes a silently rejected write from a helper that never got that
//! far. Reporting "Defender refused this" over an unrelated failure is the
//! confusion the file exists to prevent.
//!
//! ## Duplicated constants
//!
//! The system-directory environment-variable names and the result-file name and
//! verdict tokens below are copied verbatim from `patina-core` *on purpose*.
//! This helper must not depend on `patina-core`, so neither the denylist nor
//! the receipt protocol can be shared across the crate boundary; the
//! duplication is the deliberate price of the minimal trust surface. Keep the
//! sites in sync by hand.

use std::fmt;
use std::path::Path;

/// Environment variables naming system directories that must never be excluded.
/// Duplicated verbatim from `patina_core::windows::defender`.
const SYSTEM_DIR_ENV_VARS: [&str; 4] = [
    "SystemRoot",
    "ProgramFiles",
    "ProgramW6432",
    "ProgramFiles(x86)",
];

/// The basename of the result file, written beside the request file.
/// Duplicated verbatim from `patina_core::windows::defender`.
#[cfg(windows)]
const RESULT_FILENAME: &str = "defender-result.txt";

/// The scratch name the result is written under before being renamed into
/// place, so the polling CLI never reads a partially-written verdict.
#[cfg(windows)]
const RESULT_TMP_FILENAME: &str = "defender-result.txt.tmp";

/// Receipt verdict: applied, and the elevated re-read confirmed it.
/// Duplicated verbatim from `patina_core::windows::defender`.
const RECEIPT_APPLIED: &str = "applied";

/// Receipt verdict: Defender accepted the call but the re-read shows the change
/// did not take. Duplicated verbatim from `patina_core::windows::defender`.
const RECEIPT_BLOCKED: &str = "blocked";

/// Receipt verdict: the request could not be applied at all.
/// Duplicated verbatim from `patina_core::windows::defender`.
const RECEIPT_FAILED: &str = "failed";

/// Failure modes of [`apply_defender_exclusions`].
#[derive(Debug)]
pub enum DefenderError {
    /// The action was invoked on a non-Windows build. The Defender apply only
    /// exists under `#[cfg(windows)]`; everywhere else this is the terminal
    /// outcome.
    NotWindows,

    /// A request-file line did not begin with the `A `/`R ` add/remove prefix.
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

    /// The mandatory re-read showed the exclusions did not change as requested:
    /// Defender accepted the call and silently rejected the write (Tamper
    /// Protection / managed Defender).
    ///
    /// Kept distinct from `Apply` because only this variant justifies telling
    /// the user Defender refused their change. It is not `#[cfg(windows)]`
    /// because it carries nothing platform-specific, and leaving it ungated is
    /// what lets [`receipt_body`] be exercised off Windows. `Apply` is not
    /// linked here: it is Windows-only, so the link would not resolve when the
    /// docs are built for another target.
    Blocked {
        /// The script's detail, naming the specific paths and the live
        /// Tamper-Protection status.
        detail: String,
    },

    /// PowerShell ran but the apply-and-verify script failed for some other
    /// reason: `Add`/`Remove-MpPreference` errored, or the script itself did.
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

/// Parse a request-file body into its add and remove path lists.
///
/// Each non-empty line is `A <path>` (add) or `R <path>` (remove); the path is
/// taken verbatim from the third byte onward, so a path containing spaces is
/// preserved. Any other line shape is a [`DefenderError::MalformedRequest`].
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
/// Purely lexical, mirroring `patina_core`'s `validate_exclusion_path`: a path
/// is accepted only when it is a drive-letter-absolute Windows path, is not
/// UNC, contains no wildcard, is not a bare drive root, and does not fall under
/// an env-derived system directory. This is the trust boundary: the helper
/// never trusts the CLI to have validated.
///
/// # Errors
///
/// Returns [`DefenderError::InvalidPath`] naming the first rule the path
/// violates.
pub fn validate_exclusion_path(path: &str) -> Result<(), DefenderError> {
    validate_exclusion_path_with(path, &system_dir_denylist())
}

/// The lexical core of [`validate_exclusion_path`], taking the system-directory
/// denylist explicitly so the denylist rule is testable with injected
/// directories on any platform.
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

/// The env-derived system-directory denylist for the running process. Empty off
/// Windows, where the variables are unset.
fn system_dir_denylist() -> Vec<String> {
    SYSTEM_DIR_ENV_VARS
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
        .collect()
}

/// Whether `s` is a drive-letter-absolute Windows path (`X:\...` or `X:/...`).
fn is_windows_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(&drive), Some(&b':'), Some(&sep)) = (bytes.first(), bytes.get(1), bytes.get(2))
    else {
        return false;
    };
    drive.is_ascii_alphabetic() && (sep == b'\\' || sep == b'/')
}

/// Whether `s` is a bare drive root (`C:`, `C:\`, `C:/`).
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

/// Whether `path` is equal to or nested under `dir`, comparing case- and
/// separator-insensitively.
fn is_within(path: &str, dir: &str) -> bool {
    let p = normalize(path);
    let d = normalize(dir);
    if d.is_empty() {
        return false;
    }
    p == d || p.starts_with(&format!("{d}\\"))
}

/// Normalize a path for comparison: unify separators to `\`, strip a trailing
/// separator, and ASCII-lowercase. Mirrors `patina_core`'s `normalized_key`.
fn normalize(s: &str) -> String {
    let unified: String = s.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
    unified.trim_end_matches('\\').to_ascii_lowercase()
}

/// Apply the add/remove exclusions listed in the request file, and record the
/// verdict beside it.
///
/// On Windows this reads the request file at the given absolute path (it must
/// **not** recompute the state directory: a `runas` to a different admin has a
/// different `%LOCALAPPDATA%`, so the given path is authoritative),
/// re-validates every path, then runs a single PowerShell invocation that
/// batches `Add-MpPreference` / `Remove-MpPreference` and verifies the result
/// with a mandatory re-read.
///
/// Whatever the outcome, it is written to the result file the launching CLI
/// polls for. See the module's *Reporting the verdict back*.
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

/// The apply proper, with no receipt concern. Split out so
/// [`apply_defender_exclusions`] records every terminal path through one seam
/// rather than repeating the write at each `?`.
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

/// Write the verdict the launching CLI polls for, beside the request file.
///
/// Written to a scratch name and renamed into place, so a poll that reads the
/// file mid-write cannot mistake a partial line for a verdict.
///
/// Failures here are deliberately silent. There is nowhere to report them,
/// since the helper runs with no console attached, and the consequence is
/// already well-defined: the CLI waits out its deadline and tells the user it
/// could not confirm the outcome, which is exactly true.
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
/// The line is `<verdict>` or `<verdict> <detail>`. The detail is flattened to
/// one line because the format reserves the first token for the verdict and a
/// PowerShell error rendering spans several lines.
///
/// Public, like [`parse_request`] and [`validate_exclusion_path`], so the
/// protocol it defines is exercisable on any host. The writer that calls it is
/// Windows-only, but what it writes is what the CLI parses and so is worth
/// pinning everywhere.
#[must_use = "the rendered body is what the launching CLI reads as the verdict"]
pub fn receipt_body(outcome: &Result<(), DefenderError>) -> String {
    match outcome {
        Ok(()) => format!("{RECEIPT_APPLIED}\n"),
        Err(DefenderError::Blocked { detail }) => {
            format!("{RECEIPT_BLOCKED} {}\n", one_line(detail))
        }
        Err(other) => format!("{RECEIPT_FAILED} {}\n", one_line(&other.to_string())),
    }
}

/// Collapse whitespace runs, newlines included, into single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Non-Windows fallback: the Defender apply does not exist on this target.
///
/// # Errors
///
/// Always returns [`DefenderError::NotWindows`].
#[cfg(not(windows))]
pub fn apply_defender_exclusions(_request: &Path) -> Result<(), DefenderError> {
    Err(DefenderError::NotWindows)
}

/// Run the PowerShell script that reads the request file, batches the
/// add/remove, and verifies the change with a mandatory re-read.
///
/// The request path is embedded as a single-quoted PowerShell literal (with any
/// embedded quote doubled) so it is read as data via `Get-Content
/// -LiteralPath`; the exclusion paths themselves are only ever bound to `$path`
/// variables, never interpolated into a command string. On a verification
/// mismatch the script raises with the specific paths and the live
/// Tamper-Protection status, which surfaces here as
/// [`DefenderError::Blocked`]; any other non-zero exit is
/// [`DefenderError::Apply`].
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

/// The marker the apply-and-verify script prefixes onto its
/// verification-mismatch throw.
///
/// It exists so a silently rejected write is recognizable in stderr. Without
/// it, a rejected write and an unrelated cmdlet failure are both "PowerShell
/// exited non-zero", and the user gets told Defender refused a change over
/// failures that had nothing to do with Defender's policy.
#[cfg(windows)]
const BLOCKED_MARKER: &str = "PATINA-BLOCKED";

/// Extract the verification-mismatch detail from the script's stderr, or `None`
/// if this was some other failure.
///
/// PowerShell wraps a thrown message in its own multi-line error rendering, so
/// the marker is searched for anywhere in stderr rather than expected at the
/// start, and the detail runs to the end of the marker's line.
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

/// Wrap `value` as a single-quoted PowerShell string literal, doubling any
/// embedded single quote.
#[cfg(windows)]
fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Build the apply-and-verify PowerShell script for a request file given as an
/// already-quoted PowerShell literal.
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
        // Denylist parity with `patina_core`'s validator: UNC, wildcard,
        // drive-relative, empty, drive-root, and system-dir must all be
        // rejected here too. The system-dir case uses an injected directory so
        // it runs on any platform.
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
        // The three host-independent variants must display a message naming the
        // offending input (so the CLI can surface it) and expose no `source`,
        // because they wrap nothing.
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
        // The early-return guard: `is_drive_root` is only reached with a
        // drive-absolute path through the validator, so exercise the guard
        // directly with input that lacks the drive-letter-and-colon prefix.
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
        // The distinction the CLI relays to the user: only `Blocked` earns the
        // "Defender refused this" message, so the two must not share a verdict
        // token.
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
        // The format reserves the first token for the verdict, so a detail
        // spanning lines (a PowerShell error rendering always does) must be
        // flattened or the CLI reads only its first fragment.
        let body = receipt_body(&Err(DefenderError::Blocked {
            detail: "not added:\n  C:\\a\n  C:\\b".to_owned(),
        }));
        assert_eq!(body, "blocked not added: C:\\a C:\\b\n");
        assert_eq!(body.lines().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn blocked_detail_is_found_inside_powershells_error_rendering() {
        // PowerShell wraps a thrown message in its own multi-line rendering, so
        // the marker never sits at the start of stderr.
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
        // An `Add-MpPreference` error carries no marker, so it must not be
        // reported as Defender refusing the change.
        assert_eq!(
            blocked_detail("Add-MpPreference : The service cannot be started"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_apply_script_emits_the_marker_the_parser_looks_for() {
        // The script and `blocked_detail` are two halves of one protocol; a
        // marker renamed on one side and not the other silently downgrades
        // every rejected write to a generic failure.
        assert!(apply_script("'C:\\request.txt'").contains(BLOCKED_MARKER));
    }
}
