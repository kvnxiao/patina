//! Terminal styles for user-facing output.
//!
//! A [`Styles`] bundles the `anstyle` styles the diff renderer and the
//! [`Reporter`](super::reporter::Reporter) paint with. Two concerns are kept
//! apart:
//!
//! - **Whether styled bytes are generated** is [`Styles`]. `Styles::plain`
//!   (test-only) holds empty styles that render to zero bytes, so a plain
//!   render is byte-for-byte identical to unstyled output — the form the diff
//!   unit tests assert against. [`Styles::colored`] carries the production
//!   palette.
//! - **Whether those bytes reach the user** is decided at the write boundary in
//!   `reporter`, where output goes through an `anstream` auto-stream that
//!   strips ANSI when the destination is not a terminal (or `--color never` /
//!   `NO_COLOR` is in effect).
//!
//! The renderer always emits the colored palette; the auto-stream removes it
//! when color is not wanted. This preserves the deterministic-stdout contract
//! (piped / redirected output is always plain) while a real terminal gets
//! color.

use anstyle::AnsiColor;
use anstyle::Color;
use anstyle::Style;

/// The palette the diff renderer and reporter paint with.
#[derive(Debug, Clone, Copy)]
pub struct Styles {
    /// Inserted / added lines: the diff `+` body and a new symlink target.
    pub insert: Style,
    /// Deleted / removed lines: the diff `-` body and an old symlink target.
    pub delete: Style,
    /// Unchanged diff context lines. Left plain.
    pub context: Style,
    /// The per-entry header naming the action and target path.
    pub header: Style,
    /// Warnings.
    pub warn: Style,
    /// Error-chain causes.
    pub error: Style,
    /// The interactive confirmation prompt text.
    pub prompt: Style,
}

impl Styles {
    /// All-empty styles. Every field renders to zero bytes, so styled output
    /// is byte-identical to unstyled — the invariant the diff unit tests and
    /// the deterministic-stdout contract depend on. Test-only: production
    /// always renders with [`Styles::colored`] and lets the reporter's
    /// auto-stream strip when color is not wanted.
    #[cfg(test)]
    #[must_use = "construct the style set to render with it"]
    pub const fn plain() -> Self {
        let none = Style::new();
        Self {
            insert: none,
            delete: none,
            context: none,
            header: none,
            warn: none,
            error: none,
            prompt: none,
        }
    }

    /// The production palette: green inserts, red deletes and errors, yellow
    /// warnings, bold headers and prompts.
    #[must_use = "construct the style set to render with it"]
    pub const fn colored() -> Self {
        Self {
            insert: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
            delete: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
            context: Style::new(),
            header: Style::new().bold(),
            warn: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
            error: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
            prompt: Style::new().bold(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain palette must render to zero bytes on both the opening code
    /// and the reset — this is what keeps plain output byte-identical to
    /// unstyled and underpins the deterministic-stdout contract. A regression
    /// giving `plain()` a real color would make this fail.
    #[test]
    fn plain_styles_render_to_zero_bytes() {
        let p = Styles::plain();
        for style in [
            p.insert, p.delete, p.context, p.header, p.warn, p.error, p.prompt,
        ] {
            assert_eq!(
                style.render().to_string(),
                "",
                "a plain style must emit no opening escape"
            );
            assert_eq!(
                style.render_reset().to_string(),
                "",
                "a plain style must emit no reset escape"
            );
        }
    }

    /// The colored palette must actually emit escape sequences for the +/-
    /// roles; otherwise "coloring" would be a silent no-op that no other test
    /// would catch.
    #[test]
    fn colored_insert_and_delete_emit_escapes() {
        let c = Styles::colored();
        assert!(
            c.insert.render().to_string().contains('\u{1b}'),
            "inserts must carry a color escape"
        );
        assert!(
            c.delete.render().to_string().contains('\u{1b}'),
            "deletes must carry a color escape"
        );
        // Distinct colors: an insert and a delete must not render identically,
        // or +/- would be indistinguishable in a terminal.
        assert_ne!(
            c.insert.render().to_string(),
            c.delete.render().to_string(),
            "insert and delete must use different colors"
        );
    }
}
