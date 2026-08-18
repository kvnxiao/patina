//! The `output::Reporter` abstraction: the only sanctioned site for
//! user-facing output in `patina`.
//!
//! Every byte the CLI prints for the user goes through a [`Reporter`]: the
//! rendered diff, the JSON envelope, prompt text, and warnings. `tracing` logs
//! are a separate channel with its own sink. One trait covers all output, so a
//! test has a single seam: it asserts the deterministic-stdout property against
//! that seam, and captures a command's output without spawning a subprocess.
//!
//! The implementations:
//!
//! - [`StreamReporter`] is the production wiring. It writes rendered blocks /
//!   JSON to stdout and prompts / warnings / errors to stderr, through an
//!   `anstream` auto-stream. The auto-stream strips ANSI styling whenever the
//!   destination is not a terminal, or `--color never` / `NO_COLOR` is in
//!   effect. The palette the reporter supplies and the warn / error / prompt /
//!   confirm paths are what apply styling. The [`anstream::ColorChoice`] passed
//!   at construction decides whether that styling is written or stripped.
//! - `BufferReporter` captures both streams into in-memory buffers, so a test
//!   can assert on exactly what would have been printed. It paints wherever the
//!   stream reporter paints, from a palette fixed at construction.
//!   `BufferReporter::new` is plain, so a byte-level assertion runs against
//!   text with no escapes. `BufferReporter::colored` uses the production
//!   palette. `assert_color_is_additive` drives a renderer through both
//!   palettes and compares the renders.

use crate::output::style::Styles;
use crate::output::style::paint;
use anstream::AutoStream;
use anstream::ColorChoice;
use anstyle::Style;
use std::io::Write;

/// User-facing output sink. Rendered blocks and JSON go to the "out" stream;
/// prompt text, warnings, and errors go to the "err" stream.
pub trait Reporter {
    /// The palette a renderer paints with.
    ///
    /// The sink decides whether escapes are written, so the palette belongs to
    /// the sink. The return is by value, since `Styles` is `Copy`:
    /// reading the palette leaves no borrow outstanding against the
    /// `&mut self` writes that follow.
    fn styles(&self) -> Styles;
    /// Emit an already-painted block to the out stream verbatim, newlines and
    /// all. Every multi-line surface on stdout calls this, including the
    /// rendered diff. Add the trailing newline yourself.
    fn out_block(&mut self, rendered: &str);
    /// Emit the JSON envelope to the out stream, followed by a newline.
    fn json(&mut self, document: &str);
    /// Emit a one-line status / summary message to the out stream.
    fn line(&mut self, message: &str);
    /// Emit a free-form input prompt (no trailing newline) to the err stream so
    /// it does not mix into the diff on stdout. Styled in the prompt color to
    /// signal that input is awaited. Use [`Reporter::confirm`] for a yes/no
    /// question.
    fn prompt(&mut self, text: &str);
    /// Emit a `<question> [y/N] ` confirmation prompt (no trailing newline) to
    /// the err stream. The production reporter colors the prose and highlights
    /// the affirmative `y` and default `N` keys distinctly. Under a plain
    /// palette the bytes are `"<question> [y/N] "`. `y` and `Y` are the only
    /// affirmative answers under either palette.
    fn confirm(&mut self, question: &str);
    /// Emit a warning to the err stream.
    fn warn(&mut self, message: &str);
    /// Emit an error-chain cause to the err stream. Distinguished from
    /// [`Reporter::warn`] so the production reporter can style genuine
    /// failures differently from advisory warnings.
    fn error(&mut self, message: &str);
    /// Emit an already-painted block to the err stream verbatim, newlines and
    /// all. The caller owns every style inside it, so an aligned table can use
    /// one color per cell. [`Reporter::warn`] forces a single style over
    /// a whole line. Add the trailing newline yourself.
    fn err_block(&mut self, painted: &str);
}

/// Production reporter writing to the process stdout / stderr through an
/// `anstream` auto-stream.
#[derive(Debug)]
pub struct StreamReporter {
    /// The resolved color policy (from `--color`, then env / TTY under
    /// `Auto`). Handed to each per-write auto-stream.
    choice: ColorChoice,
    /// The palette painted onto warnings, errors, and prompts, and returned by
    /// [`Reporter::styles`].
    styles: Styles,
}

impl StreamReporter {
    /// Construct a reporter with the given color policy. The palette is always
    /// the colored one; `choice` (plus the per-stream terminal check inside
    /// `anstream`) decides whether the styling is written or stripped.
    #[must_use = "construct the reporter to route user-facing output through it"]
    pub fn new(choice: ColorChoice) -> Self {
        Self {
            choice,
            styles: Styles::colored(),
        }
    }
}

/// Discard an IO result.
///
/// A print sink cannot recover from a broken stdout/stderr pipe, and a broken
/// pipe must not abort the apply. `clippy::let_underscore_must_use` denies a
/// bare `let _ = write!(…)`, so the discard is a named function.
fn ignore_io<T>(_result: std::io::Result<T>) {}

/// Compose the styled `<question> [y/N] ` confirmation prompt: the prose and
/// brackets in the prompt style, the affirmative `y` and default `N` in their
/// own styles so the two answers read distinctly. Under the plain palette
/// every segment renders to zero bytes, leaving `"<question> [y/N] "`. Both
/// reporters call this, so the plain shape cannot drift between them.
fn compose_confirm(styles: &Styles, question: &str) -> String {
    [
        paint(styles.prompt, &format!("{question} [")),
        paint(styles.prompt_affirm, "y"),
        paint(styles.prompt, "/"),
        paint(styles.prompt_default, "N"),
        paint(styles.prompt, "] "),
    ]
    .concat()
}

impl StreamReporter {
    /// Write `message` to stderr through the auto-stream, wrapped in `style`.
    ///
    /// An empty style renders to zero bytes, so a plain palette writes
    /// `message` verbatim. `newline` appends a trailing `\n`; a prompt
    /// omits it so the answer is typed on the same line.
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
    fn styles(&self) -> Styles {
        self.styles
    }

    fn out_block(&mut self, rendered: &str) {
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

    fn confirm(&mut self, question: &str) {
        // Per-segment styles are already applied. An empty outer style writes
        // them verbatim, and the auto-stream still strips color when it is not
        // wanted.
        let composed = compose_confirm(&self.styles, question);
        self.styled_err(Style::new(), &composed, false);
    }

    fn warn(&mut self, message: &str) {
        self.styled_err(self.styles.warn, message, true);
    }

    fn error(&mut self, message: &str) {
        self.styled_err(self.styles.error, message, true);
    }

    fn err_block(&mut self, painted: &str) {
        // Pre-painted per cell, so the outer style is empty, as in `confirm`.
        self.styled_err(Style::new(), painted, false);
    }
}

/// Test reporter capturing both streams into in-memory strings.
#[cfg(test)]
#[derive(Debug)]
pub struct BufferReporter {
    /// Everything that would have gone to stdout.
    pub out: String,
    /// Everything that would have gone to stderr.
    pub err: String,
    /// The palette this reporter returns to a renderer, and paints onto the
    /// prompt / warn / error paths exactly as the stream reporter does.
    styles: Styles,
}

#[cfg(test)]
impl BufferReporter {
    /// Construct an empty capturing reporter over the plain palette, so the
    /// captured bytes contain no escape sequences.
    #[must_use = "construct the reporter to capture user-facing output"]
    pub fn new() -> Self {
        Self::with_styles(&Styles::plain())
    }

    /// Construct an empty capturing reporter over the production palette, for a
    /// test that asserts on the bytes written to a terminal.
    #[must_use = "construct the reporter to capture user-facing output"]
    pub fn colored() -> Self {
        Self::with_styles(&Styles::colored())
    }

    fn with_styles(styles: &Styles) -> Self {
        Self {
            out: String::new(),
            err: String::new(),
            styles: *styles,
        }
    }
}

/// Assert that color is purely additive over whatever `render` prints.
///
/// The contract belongs to the output layer, so this function states it once
/// rather than every painting surface repeating it. Painting a cell's padding
/// along with the cell would misalign piped and `--color never` output.
/// Anything a line reports through color alone disappears wherever ANSI is
/// stripped. Both streams are checked, so a renderer cannot pass by painting
/// only one of them.
#[cfg(test)]
pub fn assert_color_is_additive(render: impl Fn(&mut BufferReporter)) {
    let mut plain = BufferReporter::new();
    render(&mut plain);
    let mut colored = BufferReporter::colored();
    render(&mut colored);

    assert!(
        colored.out.contains('\u{1b}') || colored.err.contains('\u{1b}'),
        "the colored render must contain escapes, or it is painting nothing:\nout: {:?}\nerr: {:?}",
        colored.out,
        colored.err
    );
    for (stream, painted, bare) in [
        ("stdout", &colored.out, &plain.out),
        ("stderr", &colored.err, &plain.err),
    ] {
        assert_eq!(
            &anstream::adapter::strip_str(painted).to_string(),
            bare,
            "stripping color from {stream} must leave the plain render untouched"
        );
    }
}

#[cfg(test)]
impl Reporter for BufferReporter {
    fn styles(&self) -> Styles {
        self.styles
    }

    fn out_block(&mut self, rendered: &str) {
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
        self.err.push_str(&paint(self.styles.prompt, text));
    }

    fn confirm(&mut self, question: &str) {
        self.err.push_str(&compose_confirm(&self.styles, question));
    }

    fn warn(&mut self, message: &str) {
        self.err.push_str(&paint(self.styles.warn, message));
        self.err.push('\n');
    }

    fn error(&mut self, message: &str) {
        self.err.push_str(&paint(self.styles.error, message));
        self.err.push('\n');
    }

    fn err_block(&mut self, painted: &str) {
        self.err.push_str(painted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_json_write_to_out_prompts_and_diagnostics_write_to_err() {
        let mut r = BufferReporter::new();
        r.out_block("D");
        r.line("L");
        r.json("{\"k\":1}");
        r.prompt("P");
        r.warn("W");
        r.error("E");
        r.err_block("B1\nB2\n");
        // `out_block` and `err_block` do not append a newline. `line`, `json`,
        // `warn`, and `error` each terminate their own. `prompt` does not, so
        // the answer is typed on the same line.
        assert_eq!(r.out, "DL\n{\"k\":1}\n");
        assert_eq!(r.err, "PW\nE\nB1\nB2\n");
    }

    /// The capturing reporter must paint wherever the stream reporter paints.
    /// Otherwise a renderer that reports only through `warn` does not produce
    /// escapes, and `assert_color_is_additive` stops covering it.
    #[test]
    fn a_colored_buffer_paints_the_same_paths_the_stream_does() {
        let mut r = BufferReporter::colored();
        r.prompt("P");
        r.warn("W");
        r.error("E");
        assert_eq!(
            anstream::adapter::strip_str(&r.err).to_string(),
            "PW\nE\n",
            "painting must stay additive over the plain bytes: {:?}",
            r.err
        );
        for (role, text) in [
            (Styles::colored().prompt, "P"),
            (Styles::colored().warn, "W"),
            (Styles::colored().error, "E"),
        ] {
            assert!(
                r.err.contains(&paint(role, text)),
                "{text:?} must be wrapped in its role: {:?}",
                r.err
            );
        }
    }

    #[test]
    fn confirm_plain_is_exactly_the_question_and_bracketed_keys() {
        // These are the bytes `--color never` and the buffer reporter both
        // emit, trailing space included.
        let plain = compose_confirm(&Styles::plain(), "Apply?");
        assert_eq!(plain, "Apply? [y/N] ");
    }

    #[test]
    fn confirm_colored_highlights_y_and_n_distinctly_and_strips_to_plain() {
        // Color must stay purely additive over the stable bytes. The `y` and
        // `N` are painted in styles distinct from the prose and from each
        // other. Stripping every escape still reduces the whole to the plain
        // form.
        let colored = compose_confirm(&Styles::colored(), "Apply?");
        assert!(
            colored.contains('\u{1b}'),
            "colored confirm must contain escapes: {colored:?}"
        );
        let affirm = Styles::colored().prompt_affirm.render().to_string();
        let default = Styles::colored().prompt_default.render().to_string();
        assert!(
            colored.contains(&format!("{affirm}y")),
            "the affirmative `y` must be wrapped in its own style: {colored:?}"
        );
        assert!(
            colored.contains(&format!("{default}N")),
            "the default `N` must be wrapped in its own style: {colored:?}"
        );
        let stripped = anstream::adapter::strip_str(&colored).to_string();
        assert_eq!(stripped, "Apply? [y/N] ");
    }
}
