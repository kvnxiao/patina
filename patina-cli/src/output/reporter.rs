//! The `output::Reporter` abstraction: the only sanctioned site for
//! user-facing output in `patina-cli`.
//!
//! Every byte the CLI prints for the user — the rendered diff, the JSON
//! envelope, prompt text, and warnings — funnels through a [`Reporter`].
//! Logs (via `tracing`) are a separate channel and never go here. Routing
//! all output through one trait is what lets a test assert the
//! deterministic-stdout property over a single seam, and lets these
//! command tests capture output without spawning a subprocess.
//!
//! Two implementations ship:
//!
//! - [`StreamReporter`] writes the diff / JSON to stdout and prompts / warnings
//!   / errors to stderr — the production wiring. It writes through an
//!   `anstream` auto-stream, so ANSI styling emitted by the diff renderer and
//!   the warn / error / prompt paths is stripped whenever the stream is not a
//!   terminal (or `--color never` / `NO_COLOR` is in effect). The color
//!   decision is carried by the [`anstream::ColorChoice`] passed at
//!   construction.
//! - `BufferReporter` captures both streams into in-memory buffers so a test
//!   can assert on exactly what would have been printed. It never styles, so
//!   its buffers are always plain text.

use crate::output::style::Styles;
use anstream::AutoStream;
use anstream::ColorChoice;
use anstyle::Style;
use std::io::Write;

/// User-facing output sink. Diff and JSON go to the "out" stream; prompt
/// text, warnings, and errors go to the "err" stream, matching the documented
/// split (diff on stdout, prompt on stderr).
pub trait Reporter {
    /// Emit the rendered diff (human mode) to the out stream.
    fn diff(&mut self, rendered: &str);
    /// Emit the JSON envelope to the out stream, followed by a newline.
    fn json(&mut self, document: &str);
    /// Emit a one-line status / summary message to the out stream.
    fn line(&mut self, message: &str);
    /// Emit the `Apply? [y/N]` prompt text (no trailing newline) to the
    /// err stream so it does not pollute the diff on stdout.
    fn prompt(&mut self, text: &str);
    /// Emit a warning to the err stream.
    fn warn(&mut self, message: &str);
    /// Emit an error-chain cause to the err stream. Distinguished from
    /// [`Reporter::warn`] so the production reporter can style genuine
    /// failures differently from advisory warnings.
    fn error(&mut self, message: &str);
}

/// Production reporter writing to the process stdout / stderr through an
/// `anstream` auto-stream.
#[derive(Debug)]
pub struct StreamReporter {
    /// The resolved color policy (from `--color`, then env / TTY under
    /// `Auto`). Handed to each per-write auto-stream.
    choice: ColorChoice,
    /// The palette painted onto warnings, errors, and prompts. The diff
    /// arrives already styled from the renderer.
    styles: Styles,
}

impl StreamReporter {
    /// Construct a reporter with the given color policy. The palette is always
    /// the colored one; `choice` (plus the per-stream terminal check inside
    /// `anstream`) decides whether the styling survives to the terminal.
    #[must_use = "construct the reporter to route user-facing output through it"]
    pub fn new(choice: ColorChoice) -> Self {
        Self {
            choice,
            styles: Styles::colored(),
        }
    }
}

/// Intentionally discard an IO result. A broken stdout/stderr pipe is not
/// recoverable from a print sink and must not abort the apply; swallowing
/// it here is deliberate (and keeps the `must_use` lint satisfied without
/// a bare `let _`).
fn ignore_io<T>(_result: std::io::Result<T>) {}

impl StreamReporter {
    /// Write a message styled with `style` to stderr, one line, through the
    /// auto-stream. An empty style renders to nothing, so this is safe for
    /// the plain palette too. `newline` controls whether a trailing `\n` is
    /// appended — prompts omit it so the answer is typed on the same line.
    fn styled_err(&self, style: Style, message: &str, newline: bool) {
        let mut err = AutoStream::new(std::io::stderr().lock(), self.choice);
        let nl = if newline { "\n" } else { "" };
        ignore_io(write!(
            err,
            "{}{message}{}{nl}",
            style.render(),
            style.render_reset()
        ));
        ignore_io(err.flush());
    }
}

impl Reporter for StreamReporter {
    fn diff(&mut self, rendered: &str) {
        let mut out = AutoStream::new(std::io::stdout().lock(), self.choice);
        ignore_io(out.write_all(rendered.as_bytes()));
        ignore_io(out.flush());
    }

    fn json(&mut self, document: &str) {
        let mut out = AutoStream::new(std::io::stdout().lock(), self.choice);
        ignore_io(writeln!(out, "{document}"));
        ignore_io(out.flush());
    }

    fn line(&mut self, message: &str) {
        let mut out = AutoStream::new(std::io::stdout().lock(), self.choice);
        ignore_io(writeln!(out, "{message}"));
        ignore_io(out.flush());
    }

    fn prompt(&mut self, text: &str) {
        self.styled_err(self.styles.prompt, text, false);
    }

    fn warn(&mut self, message: &str) {
        self.styled_err(self.styles.warn, message, true);
    }

    fn error(&mut self, message: &str) {
        self.styled_err(self.styles.error, message, true);
    }
}

/// Test reporter capturing both streams into in-memory strings.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct BufferReporter {
    /// Everything that would have gone to stdout.
    pub out: String,
    /// Everything that would have gone to stderr.
    pub err: String,
}

#[cfg(test)]
impl BufferReporter {
    /// Construct an empty capturing reporter.
    #[must_use = "construct the reporter to capture user-facing output"]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl Reporter for BufferReporter {
    fn diff(&mut self, rendered: &str) {
        self.out.push_str(rendered);
    }

    fn json(&mut self, document: &str) {
        self.out.push_str(document);
        self.out.push('\n');
    }

    fn line(&mut self, message: &str) {
        self.out.push_str(message);
        self.out.push('\n');
    }

    fn prompt(&mut self, text: &str) {
        self.err.push_str(text);
    }

    fn warn(&mut self, message: &str) {
        self.err.push_str(message);
        self.err.push('\n');
    }

    fn error(&mut self, message: &str) {
        self.err.push_str(message);
        self.err.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_and_json_go_to_out_prompt_warn_error_go_to_err() {
        let mut r = BufferReporter::new();
        r.diff("D");
        r.line("L");
        r.json("{\"k\":1}");
        r.prompt("P");
        r.warn("W");
        r.error("E");
        // The out stream carries diff, line, and json (json + trailing
        // newline); the err stream carries prompt (no newline), warn, and
        // error (each newline-terminated).
        assert_eq!(r.out, "DL\n{\"k\":1}\n");
        assert_eq!(r.err, "PW\nE\n");
    }
}
