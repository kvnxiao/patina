//! The `output::Reporter` abstraction: the only sanctioned site for
//! user-facing output in `patina-cli`.
//!
//! Every byte the CLI prints for the user funnels through a [`Reporter`]:
//! the rendered diff, the JSON envelope, prompt text, and warnings.
//! Logs (via `tracing`) are a separate channel and never go here. Routing
//! all output through one trait is what lets a test assert the
//! deterministic-stdout property over a single seam, and lets these
//! command tests capture output without spawning a subprocess.
//!
//! Two implementations ship:
//!
//! - [`StreamReporter`] writes rendered blocks / JSON to stdout and prompts /
//!   warnings / errors to stderr. That is the production wiring. It writes
//!   through an `anstream` auto-stream. That strips ANSI styling whenever the
//!   stream is not a terminal, or `--color never` / `NO_COLOR` is in effect.
//!   The styling comes from the palette the reporter supplies and from the warn
//!   / error / prompt / confirm paths. The color decision is carried by the
//!   [`anstream::ColorChoice`] passed at construction.
//! - `BufferReporter` captures both streams into in-memory buffers so a test
//!   can assert on exactly what would have been printed. It paints wherever the
//!   stream reporter paints, from a palette fixed at construction.
//!   `BufferReporter::new` is plain, so a test asserting on bytes sees no
//!   escapes; `BufferReporter::colored` carries the production palette.
//!   `assert_color_is_additive` drives a renderer through both and compares the
//!   two.

use crate::output::style::Styles;
use crate::output::style::paint;
use anstream::AutoStream;
use anstream::ColorChoice;
use anstyle::Style;
use std::io::Write;

/// User-facing output sink. Rendered blocks and JSON go to the "out" stream;
/// prompt text, warnings, and errors go to the "err" stream, matching the
/// documented split (diff on stdout, prompt on stderr).
///
/// The sink also owns the palette every renderer paints with;
/// [`Reporter::styles`] returns it.
pub trait Reporter {
    /// The palette a renderer paints with.
    ///
    /// The sink owns the palette because it owns the decision to strip. The
    /// production reporter always returns the colored palette, and its
    /// auto-stream drops the escapes when the destination is not a terminal.
    /// The return is by value (`Styles` is `Copy`), so reading the palette
    /// leaves no borrow outstanding against the `&mut self` writes that follow.
    fn styles(&self) -> Styles;
    /// Emit an already-painted block to the out stream verbatim, newlines and
    /// all. Every multi-line surface on stdout goes through here, including the
    /// rendered diff. Add the trailing newline yourself; nothing is appended
    /// here.
    fn out_block(&mut self, rendered: &str);
    /// Emit the JSON envelope to the out stream, followed by a newline.
    fn json(&mut self, document: &str);
    /// Emit a one-line status / summary message to the out stream.
    fn line(&mut self, message: &str);
    /// Emit a free-form input prompt (no trailing newline) to the err stream
    /// so it does not pollute the diff on stdout. Styled in the prompt color
    /// to signal that input is awaited. Use [`Reporter::confirm`] for a
    /// yes/no question.
    fn prompt(&mut self, text: &str);
    /// Emit a `<question> [y/N] ` confirmation prompt (no trailing newline) to
    /// the err stream. The production reporter colors the prose and
    /// highlights the affirmative `y` and default `N` keys distinctly; `y` /
    /// `Y` remain the only affirmative answers. Under a plain palette the
    /// bytes are exactly `"<question> [y/N] "`.
    fn confirm(&mut self, question: &str);
    /// Emit a warning to the err stream.
    fn warn(&mut self, message: &str);
    /// Emit an error-chain cause to the err stream. Distinguished from
    /// [`Reporter::warn`] so the production reporter can style genuine
    /// failures differently from advisory warnings.
    fn error(&mut self, message: &str);
    /// Emit an already-painted block to the err stream verbatim, newlines and
    /// all. The caller owns every style inside it, which is what lets an
    /// aligned table carry one color per cell instead of the single style
    /// [`Reporter::warn`] forces on a whole line. Add the trailing newline
    /// yourself; nothing is appended here.
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

/// Compose the styled `<question> [y/N] ` confirmation prompt: the prose and
/// brackets in the prompt style, the affirmative `y` and default `N` in their
/// own styles so the two answers read distinctly. Under the plain palette
/// every segment renders to zero bytes, so the result is exactly
/// `"<question> [y/N] "`, the form the buffer reporter and `--color never`
/// share. Shared by both reporters so the plain shape cannot drift between
/// them.
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
    /// Write a message styled with `style` to stderr, one line, through the
    /// auto-stream. An empty style renders to nothing, so this is safe for
    /// the plain palette too. `newline` controls whether a trailing `\n` is
    /// appended; prompts omit it so the answer is typed on the same line.
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
        // Pre-composed with per-segment styles; write it verbatim (empty
        // outer style) so the auto-stream still strips color when not wanted.
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
        // Pre-painted per cell; write it with an empty outer style, as
        // `confirm` does, so the auto-stream still strips color when not wanted.
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
    /// Construct an empty capturing reporter over the plain palette, so a test
    /// asserting on captured bytes sees no escape sequences.
    #[must_use = "construct the reporter to capture user-facing output"]
    pub fn new() -> Self {
        Self::with_styles(&Styles::plain())
    }

    /// Construct an empty capturing reporter over the production palette, for a
    /// test that asserts on what a terminal would receive.
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
/// This is the output layer's own contract, so it lives here rather than being
/// restated by each surface that paints. Two failures are in scope. Painting a
/// cell's padding along with the cell would misalign piped and `--color never`
/// output, and making color the sole carrier of a fact would lose that fact
/// wherever ANSI is stripped. Both streams are checked, so a renderer cannot
/// pass by writing its painted bytes to the one nobody looked at.
#[cfg(test)]
pub fn assert_color_is_additive(render: impl Fn(&mut BufferReporter)) {
    let mut plain = BufferReporter::new();
    render(&mut plain);
    let mut colored = BufferReporter::colored();
    render(&mut colored);

    assert!(
        colored.out.contains('\u{1b}') || colored.err.contains('\u{1b}'),
        "the colored render must carry escapes, or it is painting nothing:\nout: {:?}\nerr: {:?}",
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
    fn blocks_and_json_go_to_out_prompt_warn_error_go_to_err() {
        let mut r = BufferReporter::new();
        r.out_block("D");
        r.line("L");
        r.json("{\"k\":1}");
        r.prompt("P");
        r.warn("W");
        r.error("E");
        r.err_block("B1\nB2\n");
        // The out stream carries the block, line, and json (json + trailing
        // newline); the err stream carries prompt (no newline), warn, and
        // error (each newline-terminated), then the block verbatim.
        assert_eq!(r.out, "DL\n{\"k\":1}\n");
        assert_eq!(r.err, "PW\nE\nB1\nB2\n");
    }

    /// The capturing reporter must paint wherever the stream reporter paints.
    /// Otherwise a renderer that reports only through `warn` shows no escapes,
    /// and `assert_color_is_additive` stops covering it.
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
        // Under the plain palette the composed confirm prompt must be the
        // bare `<question> [y/N] `, with no escapes and the trailing space
        // intact, so `--color never` and the buffer reporter emit the same
        // bytes.
        let plain = compose_confirm(&Styles::plain(), "Apply?");
        assert_eq!(plain, "Apply? [y/N] ");
    }

    #[test]
    fn confirm_colored_highlights_y_and_n_distinctly_but_strips_to_plain() {
        // The colored composition must carry escapes and wrap the `y` and `N`
        // in different styles from the prose (and each other), yet reduce to
        // the exact plain form once every escape is removed, which proves
        // color is purely additive over the stable bytes.
        let colored = compose_confirm(&Styles::colored(), "Apply?");
        assert!(
            colored.contains('\u{1b}'),
            "colored confirm must carry escapes: {colored:?}"
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
        // Stripping every ANSI escape must leave exactly the plain prompt.
        let stripped = anstream::adapter::strip_str(&colored).to_string();
        assert_eq!(stripped, "Apply? [y/N] ");
    }
}
