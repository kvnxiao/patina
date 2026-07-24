//! The `apply-defender-exclusions` action: add and remove Windows Defender
//! path exclusions under one-time UAC elevation.
//!
//! The unprivileged `patina` CLI derives, previews, and gets consent for the
//! exact exclusion set, writes it to a request file in the per-machine state
//! directory, then launches this helper elevated with the absolute request path
//! as its one argument. The helper reads that file, **independently
//! re-validates every path** (this is the trust boundary — the CLI's validation
//! is not trusted here), and applies the add/remove through PowerShell's
//! `Defender` module, verifying with a mandatory re-read that the change
//! actually took.
//!
//! The real work is `#[cfg(windows)]`-gated. On any other host the action
//! returns [`DefenderError::NotWindows`] without side effects, which keeps the
//! argument-parsing surface and the pure validator/parser exercisable by the
//! cross-platform tests.
//!
//! ## Duplicated constants
//!
//! The system-directory environment-variable names below are copied verbatim
//! from `patina-core`'s validator *on purpose*. This helper must not depend on
//! `patina-core`, so the denylist cannot be shared across the crate boundary;
//! the duplication is the deliberate price of the minimal trust surface. Keep
//! the two sites in sync by hand.

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

    /// PowerShell ran but the apply-and-verify script reported failure — either
    /// `Add`/`Remove-MpPreference` errored, or the mandatory re-read showed the
    /// exclusions did not change as requested (Tamper Protection / managed
    /// Defender silently rejecting the write).
    #[cfg(windows)]
    Apply {
        /// The script's stderr, carrying the specific paths and Defender
        /// status.
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
            Self::NotWindows | Self::MalformedRequest { .. } | Self::InvalidPath { .. } => None,
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
/// an env-derived system directory. This is the trust boundary — the helper
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

/// Apply the add/remove exclusions listed in the request file.
///
/// On Windows this reads the request file at the given absolute path (it must
/// **not** recompute the state directory — a `runas` to a different admin has a
/// different `%LOCALAPPDATA%`, so the given path is authoritative),
/// re-validates every path, then runs a single PowerShell invocation that
/// batches `Add-MpPreference` / `Remove-MpPreference` and verifies the result
/// with a mandatory re-read.
///
/// # Errors
///
/// Returns [`DefenderError`] when the request cannot be read or parsed, a path
/// fails re-validation, PowerShell cannot be spawned, or the apply-and-verify
/// script reports the exclusions did not take.
#[cfg(windows)]
pub fn apply_defender_exclusions(request: &Path) -> Result<(), DefenderError> {
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
/// Tamper-Protection status, which surfaces on stderr as
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
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(DefenderError::Apply { detail: stderr })
    }
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
           throw \"exclusions not applied (TamperProtected=$($s.IsTamperProtected), RunningMode=$($s.AMRunningMode)); not added: $($missing -join ', '); not removed: $($lingering -join ', ')\" \
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
        // offending input (so the CLI can surface it) and expose no `source` —
        // they wrap nothing.
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
}
