//! Terminal styles for user-facing output.
//!
//! A [`Styles`] bundles the `anstyle` styles the diff renderer and the
//! [`Reporter`](super::reporter::Reporter) paint with. Two decisions are kept
//! apart: whether styled bytes are generated at all, and whether they reach
//! the user.
//!
//! - **Whether styled bytes are generated** is [`Styles`]. `Styles::plain`
//!   (test-only) holds empty styles that render to zero bytes, so a plain
//!   render is byte-for-byte identical to unstyled output, the form the diff
//!   unit tests assert against. [`Styles::colored`] carries the production
//!   palette.
//! - **Whether those bytes reach the user** is decided at the write boundary in
//!   `reporter`. Output there goes through an `anstream` auto-stream, which
//!   strips ANSI when the destination is not a terminal, or when `--color
//!   never` / `NO_COLOR` is in effect.
//!
//! Piped and redirected output therefore comes out plain every time, which is
//! the deterministic-stdout contract.

use anstyle::AnsiColor;
use anstyle::Color;
use anstyle::Style;

/// The palette the diff renderer and reporter paint with.
///
/// Roles are grouped by the surface that prints them, and the roles in one
/// group can land on a single row. Two roles in one group must therefore never
/// render alike, or one reads as the other. Across groups, identical styling is
/// deliberate and marks one shared visual meaning.
/// [`header`](Styles::header) and [`path`](Styles::path) are both bold, because
/// each names the subject of its line. [`hint`](Styles::hint),
/// [`FindingStyles::info`], and [`RemoteStyles::implicit_ref`] are all dimmed,
/// because each is subordinate to the fact beside it. Separating those roles
/// would spend scarce terminal hues on a distinction the reader does not need.
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
    /// Interactive prompt prose: the question and its surrounding brackets,
    /// and free-form input prompts. Signals that input is awaited.
    pub prompt: Style,
    /// The affirmative key in a `[y/N]` confirmation (the `y`).
    pub prompt_affirm: Style,
    /// The default key in a `[y/N]` confirmation (the capitalized `N`).
    pub prompt_default: Style,
    /// An action that reached the state the user asked for: an apply that
    /// landed, or a watch service already running.
    pub success: Style,
    /// A path embedded in a one-line result sentence, so the path the command
    /// acted on stands out from the prose around it.
    pub path: Style,
    /// A follow-up suggestion, or a stand-in carrying no value of its own such
    /// as a cell repeating the one beside it. Nothing a line reports may rest
    /// on this role alone.
    pub hint: Style,
    /// The roles the `patina status` table paints with.
    pub status: StatusStyles,
    /// The roles the `patina doctor` findings paint with.
    pub finding: FindingStyles,
    /// The roles the `patina remote list` table paints with.
    pub remote: RemoteStyles,
    /// The roles the Defender exclusion listing paints with.
    #[cfg(windows)]
    pub exclusion: ExclusionStyles,
}

/// The roles the `patina status` table paints with, one per
/// [`TargetState`](patina_core::TargetState).
///
/// Each row keeps its state word in text, so the color only speeds the scan:
/// a clean repository reads at a glance rather than through four counters.
#[derive(Debug, Clone, Copy)]
pub struct StatusStyles {
    /// A target matching the last apply.
    pub clean: Style,
    /// A target whose content or link destination has moved.
    pub drifted: Style,
    /// A target the last apply wrote that is no longer on disk.
    pub missing: Style,
    /// A target the current plan no longer manages. It takes its own hue rather
    /// than a failure color, because an orphan is a leftover awaiting a reap
    /// and the next apply offers to remove it.
    pub orphaned: Style,
}

/// The roles the `patina doctor` findings paint with, one per
/// [`Level`](crate::cmd::doctor::Level).
///
/// The level also stays bracketed in the row's first cell, so an ANSI-stripped
/// report still tells an advisory note from an error.
#[derive(Debug, Clone, Copy)]
pub struct FindingStyles {
    /// An advisory note that never affects the exit code.
    pub info: Style,
    /// A warning the user should act on; the command still exits 0.
    pub warning: Style,
    /// A finding that exits 1.
    pub error: Style,
}

/// The roles the `patina remote list` table paints with.
///
/// Every cell these color also carries its meaning in text, so an ANSI-stripped
/// listing loses the color and nothing else.
#[derive(Debug, Clone, Copy)]
pub struct RemoteStyles {
    /// The remote's name.
    pub name: Style,
    /// The `ref` a `[[remote]]` declares.
    pub declared_ref: Style,
    /// A recorded pin's rev.
    pub rev: Style,
    /// The remote's URL.
    pub url: Style,
    /// The cells asking for action: `(unpinned)` and the `(update pending)`
    /// tag. One role because one command answers both.
    pub attention: Style,
    /// The `(default branch)` stand-in for a `[[remote]]` that declares no
    /// `ref`.
    pub implicit_ref: Style,
}

/// The styles the Defender exclusion listing paints with.
///
/// Gated with the command that uses them: `patina defender` does not exist off
/// Windows, so neither do its roles.
///
/// The kind roles paint the **whole path**, and the listing prints no `(file)`
/// / `(folder)` text alongside. Color is therefore the only place the kind
/// appears in human output, and it is lost wherever ANSI is stripped: piped
/// output, `--color never`, `NO_COLOR`. `--json` carries `kind` as a field for
/// that reason; a consumer that needs the distinction should read that
/// instead.
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
pub struct ExclusionStyles {
    /// The path of a file exclusion.
    pub file: Style,
    /// The path of a folder exclusion. Distinct from
    /// [`file`](ExclusionStyles::file): a folder exclusion is the broader blind
    /// spot, so which kind an entry is has to be readable at a glance down a
    /// list that runs to dozens of paths.
    pub folder: Style,
    /// The state tag on an exclusion already in place and recorded by Patina:
    /// `[present]`, or `[recorded]` when the state came from the ledger.
    pub state_present: Style,
    /// The state tag on an exclusion Defender already excludes that Patina does
    /// not record. Its own color because it is neither in place *for Patina*
    /// nor missing from Defender: nothing is wrong, but reversibility
    /// differs, so it reads as attention rather than success or failure.
    pub state_unmanaged: Style,
    /// The state tag on an exclusion not in place: `[missing]`, or
    /// `[not recorded]` when the state came from the ledger.
    pub state_absent: Style,
}

impl Styles {
    /// All-empty styles. Every field renders to zero bytes, so styled output
    /// is byte-identical to unstyled, the invariant the diff unit tests and
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
            prompt_affirm: none,
            prompt_default: none,
            success: none,
            path: none,
            hint: none,
            status: StatusStyles {
                clean: none,
                drifted: none,
                missing: none,
                orphaned: none,
            },
            finding: FindingStyles {
                info: none,
                warning: none,
                error: none,
            },
            remote: RemoteStyles {
                name: none,
                declared_ref: none,
                rev: none,
                url: none,
                attention: none,
                implicit_ref: none,
            },
            #[cfg(windows)]
            exclusion: ExclusionStyles {
                file: none,
                folder: none,
                state_present: none,
                state_unmanaged: none,
                state_absent: none,
            },
        }
    }

    /// The production palette: green inserts, red deletes and errors, yellow
    /// warnings, bold headers, and cyan interactive prompts. A prompt's
    /// `[y/N]` keys read green (affirm) and red (default), so the two answers
    /// stand apart from the prose and from each other.
    ///
    /// Severity runs green → yellow → red wherever it appears, so the status
    /// states and the doctor levels read the same way as a diff does. An
    /// orphaned target breaks out of that run into magenta, because it is a
    /// leftover awaiting a reap. It has no place on a severity scale.
    ///
    /// The Defender-exclusion roles paint the path blue (file) or magenta
    /// (folder), leaving green, yellow, and red for the state tag: green in
    /// place and Patina's, yellow in place but not Patina's, red not in place.
    /// Path and state therefore never compete for the same hue on one line.
    #[must_use = "construct the style set to render with it"]
    pub const fn colored() -> Self {
        Self {
            insert: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
            delete: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
            context: Style::new(),
            header: Style::new().bold(),
            warn: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
            error: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
            prompt: Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
                .bold(),
            prompt_affirm: Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Green)))
                .bold(),
            prompt_default: Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .bold(),
            success: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
            path: Style::new().bold(),
            hint: Style::new().dimmed(),
            status: StatusStyles {
                clean: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
                drifted: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
                missing: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
                orphaned: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta))),
            },
            finding: FindingStyles {
                info: Style::new().dimmed(),
                warning: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
                error: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
            },
            remote: RemoteStyles {
                name: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))),
                declared_ref: Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightYellow))),
                rev: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
                url: Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightBlue))),
                attention: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
                implicit_ref: Style::new().dimmed(),
            },
            #[cfg(windows)]
            exclusion: ExclusionStyles {
                file: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Blue))),
                folder: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta))),
                state_present: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))),
                state_unmanaged: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
                state_absent: Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))),
            },
        }
    }
}

/// Wrap `text` in `style`'s opening escape and reset.
///
/// An empty style renders to zero bytes on both, so under the plain palette
/// this returns `text` unchanged. That keeps a plain render byte-identical
/// to unstyled output.
#[must_use = "the painted string is what gets written"]
pub fn paint(style: Style, text: &str) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain palette must render to zero bytes on both the opening code
    /// and the reset. That keeps plain output byte-identical to unstyled and
    /// underpins the deterministic-stdout contract. A regression giving
    /// `plain()` a real color would make this fail.
    #[test]
    fn plain_styles_render_to_zero_bytes() {
        let p = Styles::plain();
        for style in [
            p.insert,
            p.delete,
            p.context,
            p.header,
            p.warn,
            p.error,
            p.prompt,
            p.prompt_affirm,
            p.prompt_default,
            p.success,
            p.path,
            p.hint,
            p.status.clean,
            p.status.drifted,
            p.status.missing,
            p.status.orphaned,
            p.finding.info,
            p.finding.warning,
            p.finding.error,
            p.remote.name,
            p.remote.declared_ref,
            p.remote.rev,
            p.remote.url,
            p.remote.attention,
            p.remote.implicit_ref,
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

    /// The same zero-byte guarantee for the Windows-only exclusion roles, which
    /// live in their own struct and so are missed by the loop above.
    #[cfg(windows)]
    #[test]
    fn plain_exclusion_styles_render_to_zero_bytes() {
        let e = Styles::plain().exclusion;
        for style in [
            e.file,
            e.folder,
            e.state_present,
            e.state_unmanaged,
            e.state_absent,
        ] {
            assert_eq!(style.render().to_string(), "");
            assert_eq!(style.render_reset().to_string(), "");
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

    /// The confirmation prompt's three roles must each emit an escape and be
    /// mutually distinct, so the prose, the affirmative `y`, and the default
    /// `N` are visually separable in a terminal.
    #[test]
    fn colored_prompt_roles_are_distinct_and_escaped() {
        let c = Styles::colored();
        for style in [c.prompt, c.prompt_affirm, c.prompt_default] {
            assert!(
                style.render().to_string().contains('\u{1b}'),
                "each prompt role must carry a color escape"
            );
        }
        let prose = c.prompt.render().to_string();
        let affirm = c.prompt_affirm.render().to_string();
        let default = c.prompt_default.render().to_string();
        assert_ne!(prose, affirm, "prose and affirm must differ");
        assert_ne!(prose, default, "prose and default must differ");
        assert_ne!(affirm, default, "affirm and default must differ");
    }

    /// Every role in a group must emit an escape and render unlike every other
    /// role in the same group. A silent `Style::new()` would make one role's
    /// color a no-op, and two roles sharing a hue would make one read as the
    /// other wherever they appear on the same row.
    fn assert_distinct_and_escaped(roles: &[(&str, Style)]) {
        for (name, style) in roles {
            assert!(
                style.render().to_string().contains('\u{1b}'),
                "the {name} role must carry a color escape"
            );
        }
        for (index, (left_name, left)) in roles.iter().enumerate() {
            for (right_name, right) in roles.iter().skip(index + 1) {
                assert_ne!(
                    left.render().to_string(),
                    right.render().to_string(),
                    "{left_name} and {right_name} must render differently"
                );
            }
        }
    }

    #[test]
    fn colored_remote_roles_are_distinct_and_escaped() {
        let r = Styles::colored().remote;
        assert_distinct_and_escaped(&[
            ("name", r.name),
            ("declared_ref", r.declared_ref),
            ("rev", r.rev),
            ("url", r.url),
            ("attention", r.attention),
            ("implicit_ref", r.implicit_ref),
        ]);
    }

    /// The four state colors let a reader scan a long listing without
    /// reading every state word. Two states rendering alike would defeat
    /// that purpose.
    #[test]
    fn colored_status_roles_are_distinct_and_escaped() {
        let s = Styles::colored().status;
        assert_distinct_and_escaped(&[
            ("clean", s.clean),
            ("drifted", s.drifted),
            ("missing", s.missing),
            ("orphaned", s.orphaned),
        ]);
    }

    /// Three levels sharing one hue would leave the severity of a report
    /// readable only by reading every bracketed word.
    #[test]
    fn colored_finding_roles_are_distinct_and_escaped() {
        let f = Styles::colored().finding;
        assert_distinct_and_escaped(&[
            ("info", f.info),
            ("warning", f.warning),
            ("error", f.error),
        ]);
    }

    /// `init` prints a painted path and a hint on consecutive lines, so the two
    /// have to read apart even though neither is a table cell.
    #[test]
    fn colored_flat_roles_are_distinct_and_escaped() {
        let c = Styles::colored();
        assert_distinct_and_escaped(&[("success", c.success), ("path", c.path), ("hint", c.hint)]);
    }

    /// Kind and state appear on the same line, so a hue shared between the two
    /// groups would make one read as the other. The three state colors let a
    /// reader tell the states apart without reading the bracketed tag.
    #[cfg(windows)]
    #[test]
    fn colored_exclusion_roles_are_distinct_and_escaped() {
        let c = Styles::colored();
        assert_distinct_and_escaped(&[
            ("file", c.exclusion.file),
            ("folder", c.exclusion.folder),
            ("present", c.exclusion.state_present),
            ("unmanaged", c.exclusion.state_unmanaged),
            ("absent", c.exclusion.state_absent),
        ]);
    }

    /// `paint` must be a no-op under the plain palette, which the diff and
    /// Defender renderers' plain-output tests rest on. Under the colored
    /// palette it must wrap the text in both an opening escape and a reset.
    #[test]
    fn paint_is_transparent_when_plain_and_wraps_when_colored() {
        assert_eq!(paint(Styles::plain().insert, "text"), "text");

        let painted = paint(Styles::colored().insert, "text");
        assert!(painted.contains("text"), "the text must survive intact");
        assert!(
            painted.starts_with('\u{1b}'),
            "an opening escape is required"
        );
        assert!(painted.ends_with('m'), "a reset must close the run");
    }
}
