//! Human-readable rendering of a decoded plan journal.
//!
//! `patina debug journal <path>` decodes a `<ts>.plan` file (the binary,
//! `postcard`-encoded [`Plan`](super::Plan) behind its version envelope)
//! and prints it for a human reading a post-mortem. The output is a
//! one-line summary per operation followed by indented detail; it is
//! deliberately **not** a stable, machine-parsed format and is
//! the one user-facing path allowed to carry a wall-clock timestamp
//! (the plan's recorded `<ts>`).
//!
//! The plan body records only the resolved file operations — symlink,
//! render, and copy, each with a repo-relative `source` and an absolute
//! `target`. Hooks and the resolved variable context are evaluated during
//! apply but are not serialized into the plan, so they do not appear here.
//!
//! # Examples
//!
//! ```
//! use patina_core::Disposition;
//! use patina_core::journal::{Plan, PlannedOperation, render_plan};
//!
//! let plan = Plan::new(vec![PlannedOperation::symlink(
//!     "zsh/zshrc",
//!     "/home/u/.zshrc",
//!     Disposition::Create,
//! )]);
//! let text = render_plan(&plan, "20260528T120000Z");
//! assert!(text.contains("symlink"));
//! assert!(text.contains("/home/u/.zshrc"));
//! ```

use super::JournalError;
use super::Plan;
use super::PlannedOperation;
use super::record::timestamp_to_rfc3339;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;

/// Errors raised while loading a plan file for the `debug journal` view.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlanRenderError {
    /// The plan file at `path` could not be opened or read. The wrapped
    /// error carries the underlying IO cause; `path` is surfaced so the
    /// CLI can name it.
    #[error("could not read plan file `{path}`: {source}")]
    Read {
        /// The path that could not be read.
        path: Utf8PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// The bytes at the path could not be decoded as a plan. This carries a
    /// [`JournalError`] — most notably
    /// [`JournalError::VersionMismatch`](super::JournalError::VersionMismatch)
    /// for a plan from a newer binary, which the CLI surfaces naming both
    /// versions.
    #[error("could not decode plan file `{path}`: {source}")]
    Decode {
        /// The path whose contents failed to decode.
        path: Utf8PathBuf,
        /// The decode failure (version mismatch, truncation, or corruption).
        source: JournalError,
    },
}

/// Read and decode the plan file at `path`, returning the decoded
/// [`Plan`] alongside the `<ts>` timestamp recovered from the filename.
///
/// The timestamp is the `<ts>` prefix of a `<ts>.plan` filename; if the
/// filename does not match that shape the whole file stem is returned so
/// the caller still has something to print.
///
/// # Errors
///
/// - [`PlanRenderError::Read`] if the file is missing or unreadable.
/// - [`PlanRenderError::Decode`] if the bytes fail to decode, including a
///   [`JournalError::VersionMismatch`](super::JournalError::VersionMismatch)
///   for a plan written by a newer binary.
pub fn load_plan_file(path: &Utf8Path) -> Result<(Plan, String), PlanRenderError> {
    let bytes = fs_err::read(path).map_err(|source| PlanRenderError::Read {
        path: path.to_owned(),
        source,
    })?;
    let plan = Plan::decode(&bytes).map_err(|source| PlanRenderError::Decode {
        path: path.to_owned(),
        source,
    })?;
    Ok((plan, timestamp_from_plan_path(path)))
}

/// Recover the `<ts>` timestamp from a `<ts>.plan` path. Falls back to the
/// full file stem (or the path string) when the name does not match.
fn timestamp_from_plan_path(path: &Utf8Path) -> String {
    path.file_name()
        .and_then(|name| name.strip_suffix(super::PLAN_SUFFIX))
        .map_or_else(|| path.as_str().to_owned(), str::to_owned)
}

/// Render a decoded [`Plan`] and its recorded `<ts>` to a human-readable
/// string: a header line carrying the plan timestamp (both the compact
/// journal form and its RFC 3339 rendering) and the operation count,
/// followed by one block per operation identifying its mode, source, and
/// target.
#[must_use = "the rendered plan is the debug command's stdout payload"]
pub fn render_plan(plan: &Plan, timestamp: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    // `write!`/`writeln!` into a `String` is infallible (the `fmt::Error`
    // path is for IO sinks), so dropping the `Result` here is correct and
    // introduces no panic path — no `expect` in production.
    ignore_fmt(writeln!(
        out,
        "plan {timestamp} ({}) — {} operation(s)",
        timestamp_to_rfc3339(timestamp),
        plan.len()
    ));
    for (index, op) in plan.operations().iter().enumerate() {
        let (mode, source, target) = describe(op);
        ignore_fmt(writeln!(out, "[{index}] {mode}"));
        ignore_fmt(writeln!(out, "    source: {source}"));
        ignore_fmt(writeln!(out, "    target: {target}"));
    }
    out
}

/// Discard an infallible `String` formatting result. Mirrors the
/// `ignore_io` pattern in the CLI reporter: it satisfies the `must_use`
/// lint without a bare `let _` and documents that the discard is
/// deliberate.
fn ignore_fmt(_result: std::fmt::Result) {}

/// Decompose a planned operation into its `(mode label, source, target)`.
/// The mode label uses the same lowercase, hyphenated vocabulary the
/// `--json` apply envelope uses, so a reader sees consistent words.
fn describe(op: &PlannedOperation) -> (&'static str, &str, &str) {
    match op {
        PlannedOperation::Symlink { source, target, .. } => ("symlink", source, target),
        PlannedOperation::Render { source, target, .. } => ("template-render", source, target),
        PlannedOperation::Copy { source, target, .. } => ("copy", source, target),
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::FILE_MAJOR_VERSION;
    use super::*;

    fn sample() -> Plan {
        use crate::journal::Disposition;
        Plan::new(vec![
            PlannedOperation::symlink("zsh/zshrc", "/home/u/.zshrc", Disposition::Create),
            PlannedOperation::copy("git/gitconfig", "/home/u/.gitconfig", Disposition::Create),
            PlannedOperation::render("git/c.tmpl", "/home/u/.c", Disposition::Create),
        ])
    }

    #[test]
    fn render_names_each_mode_source_and_target() {
        let text = render_plan(&sample(), "20260528T120000Z");
        assert!(text.contains("symlink"), "{text}");
        assert!(text.contains("copy"), "{text}");
        assert!(text.contains("template-render"), "{text}");
        assert!(text.contains("/home/u/.zshrc"), "{text}");
        assert!(text.contains("/home/u/.gitconfig"), "{text}");
        assert!(text.contains("zsh/zshrc"), "{text}");
    }

    #[test]
    fn render_header_carries_both_timestamp_forms_and_count() {
        let text = render_plan(&sample(), "20260528T120000Z");
        assert!(text.contains("20260528T120000Z"), "{text}");
        assert!(text.contains("2026-05-28T12:00:00Z"), "{text}");
        assert!(text.contains("3 operation(s)"), "{text}");
    }

    #[test]
    fn load_round_trips_through_encode_and_recovers_timestamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join("20260528T120000Z.plan");
        fs_err::write(&path, sample().encode().expect("encode")).expect("write plan");

        let (plan, ts) = load_plan_file(&path).expect("load");
        assert_eq!(plan, sample());
        assert_eq!(ts, "20260528T120000Z");
    }

    #[test]
    fn load_missing_path_is_a_read_error_naming_the_path() {
        let err = load_plan_file(Utf8Path::new("/no/such/plan.plan"))
            .expect_err("missing path must error");
        assert!(
            matches!(&err, PlanRenderError::Read { path, .. } if path.as_str() == "/no/such/plan.plan"),
            "expected a Read error naming the path, got {err:?}"
        );
    }

    #[test]
    fn load_newer_major_is_a_decode_version_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(dir.path()).expect("utf8 tempdir");
        let path = dir.join("20260528T120000Z.plan");
        let mut bytes = sample().encode().expect("encode");
        bytes
            .get_mut(..2)
            .expect("envelope")
            .copy_from_slice(&(FILE_MAJOR_VERSION + 1).to_le_bytes());
        fs_err::write(&path, bytes).expect("write plan");

        let err = load_plan_file(&path).expect_err("newer major must error");
        assert!(
            matches!(
                &err,
                PlanRenderError::Decode {
                    source: JournalError::VersionMismatch { found, supported },
                    ..
                } if *found == FILE_MAJOR_VERSION + 1 && *supported == FILE_MAJOR_VERSION
            ),
            "expected a Decode version-mismatch naming both majors, got {err:?}"
        );
    }
}
