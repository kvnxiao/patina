//! Embedded diff rendering with the `similar` crate.
//!
//! The diff is computed from the [`ResolvedPlan`] produced by the engine:
//! for each operation we compare the target's current on-disk content (or
//! link target) against what the apply would materialize. Copy and
//! template-render modes produce a line-level content diff; symlink modes
//! produce an `old link target -> new link target` line.
//!
//! Output is deterministic: operations render in plan order, and the
//! rendered string carries no timestamps, PIDs, or absolute state-dir
//! paths (only the repo-relative-ish source and the target the user
//! declared). The byte-identical-stdout property is built on this.
//!
//! Content that cannot be line-diffed — a present-but-non-UTF-8 (binary)
//! source or target, or an unreadable file — renders as a compact
//! deterministic placeholder (`(binary content, N bytes)`) rather than an
//! empty/full-insert diff. A binary copy source is legitimate, so it is a
//! placeholder, not an error; the misleading "empty target" render would
//! otherwise distort the apply consent decision.

use camino::Utf8Path;
use patina_core::Disposition;
use patina_core::FileMode;
use patina_core::ResolvedPlan;
use patina_core::Resolver;
use patina_core::TemplateEngine;
use similar::ChangeTag;
use similar::TextDiff;
use std::fmt::Write as _;

/// Render the full plan diff to a deterministic string.
///
/// # Errors
///
/// Returns an error string when a template source cannot be rendered for
/// preview (the same strict-undefined failure the apply would hit).
pub fn render(resolved: &ResolvedPlan) -> Result<String, String> {
    let mut out = String::new();
    if resolved.operations.is_empty() {
        out.push_str("No changes: the plan is empty.\n");
        return Ok(out);
    }

    let engine = TemplateEngine::new();
    let vars = &resolved.resolver;

    // Only `Create` and `Update` targets render a
    // per-entry block; `Unchanged` targets are summarized by a single count
    // line below. For tree modes the count is over materialized leaves:
    // a drifted tree renders blocks for its drifted leaves and
    // contributes its clean leaves to `unchanged`.
    let mut unchanged = 0usize;
    for op in &resolved.operations {
        for (target, disposition) in op.targets.iter().zip(&op.dispositions) {
            if disposition.leaves.is_empty() {
                // Single-target mode: one disposition for the whole target.
                if disposition.aggregate == Disposition::Unchanged {
                    unchanged += 1;
                } else {
                    render_leaf(&mut out, op.mode, &op.source, target, &engine, vars)?;
                }
            } else {
                // Tree mode: route per materialized leaf so a single drifted
                // leaf does not pull its clean siblings into the diff body.
                for leaf in &disposition.leaves {
                    if leaf.disposition == Disposition::Unchanged {
                        unchanged += 1;
                    } else {
                        let leaf_source = op.source.join(&leaf.relative);
                        let leaf_target = target.join(&leaf.relative);
                        render_leaf(&mut out, op.mode, &leaf_source, &leaf_target, &engine, vars)?;
                    }
                }
            }
        }
    }

    // Exactly one deterministic summary line for the Unchanged count.
    // Omitted when nothing is unchanged so a fully-changing plan's
    // body is unchanged from prior behaviour.
    if unchanged > 0 {
        let noun = if unchanged == 1 { "entry" } else { "entries" };
        emit(&mut out, format_args!("{unchanged} unchanged {noun}.\n"));
    }
    Ok(out)
}

/// Render one block for a `(mode, source, target)` triple into `out`. Shared
/// by the single-target path and the tree-mode per-leaf path so a drifted
/// leaf renders the same block shape as a single-target entry of the same
/// mode.
fn render_leaf(
    out: &mut String,
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    engine: &TemplateEngine,
    vars: &Resolver,
) -> Result<(), String> {
    match mode {
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => {
            let current = current_link_target(target);
            emit(out, format_args!("symlink {target}\n"));
            emit(
                out,
                format_args!("  - {}\n", current.as_deref().unwrap_or("(absent)")),
            );
            emit(out, format_args!("  + {source}\n"));
        }
        FileMode::Copy | FileMode::CopyTree => {
            // A copy source may legitimately be a binary file (fonts, images,
            // compiled config); `read_for_diff` renders each side as an opaque
            // placeholder rather than a misleading empty/full-insert diff.
            let new = read_for_diff(source);
            let current = read_for_diff(target);
            content_diff(out, "copy", target, &current, &new);
        }
        FileMode::TemplateRender => {
            let body = fs_err::read_to_string(source)
                .map_err(|e| format!("failed to read template {source}: {e}"))?;
            let rendered = engine
                .render(&body, vars)
                .map_err(|e| format!("failed to render template {source}: {e}"))?;
            let current = read_for_diff(target);
            content_diff(
                out,
                "render",
                target,
                &current,
                &DiffContent::Text(rendered),
            );
        }
    }
    Ok(())
}

/// A file's content as it should appear on one side of a content diff.
enum DiffContent {
    /// The path does not exist; the apply creates it. Diffs as an empty
    /// "before", so a create renders a full insert.
    Absent,
    /// Valid UTF-8 text, line-diffable.
    Text(String),
    /// Present but not valid UTF-8, or otherwise unreadable. Rendered as a
    /// compact deterministic placeholder instead of a misleading empty diff —
    /// the preview must never imply a binary target is empty, because the diff
    /// drives the apply consent decision.
    Opaque(String),
}

impl DiffContent {
    /// The diffable text, treating an absent file as empty. `None` for
    /// content that cannot be line-diffed (binary / unreadable).
    fn as_text(&self) -> Option<&str> {
        match self {
            DiffContent::Absent => Some(""),
            DiffContent::Text(text) => Some(text),
            DiffContent::Opaque(_) => None,
        }
    }

    /// A compact, deterministic one-line descriptor for the opaque-diff
    /// fallback. Carries only byte counts and fixed words — never OS error
    /// strings — so byte-identical stdout is preserved.
    fn describe(&self) -> String {
        match self {
            DiffContent::Absent => "(absent)".to_owned(),
            DiffContent::Text(text) => format!("(text, {} bytes)", text.len()),
            DiffContent::Opaque(desc) => format!("({desc})"),
        }
    }
}

/// Read `path` for diffing: valid UTF-8 becomes [`DiffContent::Text`], a
/// missing file becomes [`DiffContent::Absent`] (a create), and a
/// present-but-non-UTF-8 or otherwise-unreadable file becomes
/// [`DiffContent::Opaque`] so the preview never renders binary content as an
/// empty file.
fn read_for_diff(path: &Utf8Path) -> DiffContent {
    match fs_err::read(path.as_std_path()) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => DiffContent::Text(text.to_owned()),
            Err(_) => DiffContent::Opaque(format!("binary content, {} bytes", bytes.len())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DiffContent::Absent,
        Err(_) => DiffContent::Opaque("unreadable".to_owned()),
    }
}

/// Append a line-level content diff between the target's current content and
/// the content the apply would write, under the action label. When either side
/// cannot be line-diffed (binary / unreadable), render a compact placeholder
/// pair instead of a misleading empty/full-insert diff.
fn content_diff(
    out: &mut String,
    action: &str,
    target: &Utf8Path,
    current: &DiffContent,
    new: &DiffContent,
) {
    emit(out, format_args!("{action} {target}\n"));
    // Both sides line-diffable → a line-level diff; otherwise (binary /
    // unreadable on either side) a compact placeholder pair.
    if let (Some(current), Some(new)) = (current.as_text(), new.as_text()) {
        let diff = TextDiff::from_lines(current, new);
        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "  - ",
                ChangeTag::Insert => "  + ",
                ChangeTag::Equal => "    ",
            };
            out.push_str(sign);
            out.push_str(change.value());
            if !change.value().ends_with('\n') {
                out.push('\n');
            }
        }
    } else {
        emit(out, format_args!("  - {}\n", current.describe()));
        emit(out, format_args!("  + {}\n", new.describe()));
    }
}

/// Read the link target at `target` if it is a symlink, as a UTF-8 string.
fn current_link_target(target: &Utf8Path) -> Option<String> {
    let raw = fs_err::read_link(target.as_std_path()).ok()?;
    raw.into_os_string().into_string().ok()
}

/// Append formatted text to an in-memory diff buffer. Writing to a
/// `String` is infallible, so the `fmt::Result` is intentionally
/// discarded here (keeping the must-use lint satisfied without a bare
/// `let _`).
fn emit(out: &mut String, args: std::fmt::Arguments<'_>) {
    discard(out.write_fmt(args));
}

/// Intentionally consume an infallible `fmt::Result` without binding it,
/// so neither the must-use nor the unused-variable lint fires.
fn discard(_result: std::fmt::Result) {}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    fn tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf8 temp path");
        (td, path)
    }

    #[test]
    fn read_for_diff_reads_utf8_as_text() {
        let (_td, dir) = tempdir();
        let file = dir.join("a.txt");
        fs_err::write(&file, "hello\n").expect("write utf8");
        assert_eq!(read_for_diff(&file).as_text(), Some("hello\n"));
    }

    #[test]
    fn read_for_diff_reports_a_missing_file_as_absent() {
        let (_td, dir) = tempdir();
        let absent = read_for_diff(&dir.join("nope"));
        // Absent diffs as empty (a create), and describes itself distinctly.
        assert_eq!(absent.as_text(), Some(""));
        assert_eq!(absent.describe(), "(absent)");
    }

    #[test]
    fn read_for_diff_reports_non_utf8_as_opaque_binary_with_byte_count() {
        let (_td, dir) = tempdir();
        let file = dir.join("bin");
        // Invalid UTF-8: a lone 0xFF is never a valid UTF-8 sequence.
        fs_err::write(&file, [0xff, 0xfe, 0x00]).expect("write binary");
        let content = read_for_diff(&file);
        assert_eq!(content.as_text(), None, "binary must not be line-diffable");
        assert_eq!(content.describe(), "(binary content, 3 bytes)");
    }

    #[test]
    fn read_for_diff_reports_an_unreadable_path_as_opaque() {
        // A present path that cannot be read as bytes (a directory) is neither
        // absent nor text; it renders as the opaque "unreadable" placeholder
        // rather than a misleading empty diff.
        let (_td, dir) = tempdir();
        let subdir = dir.join("subdir");
        fs_err::create_dir_all(&subdir).expect("mkdir subdir");
        let content = read_for_diff(&subdir);
        assert_eq!(content.as_text(), None, "a directory is not line-diffable");
        assert_eq!(content.describe(), "(unreadable)");
    }

    #[test]
    fn content_diff_renders_a_binary_target_as_a_placeholder_not_an_empty_diff() {
        let (_td, dir) = tempdir();
        let target = dir.join("target");
        fs_err::write(&target, [0xff, 0xfe, 0x00]).expect("write binary target");

        let mut out = String::new();
        let current = read_for_diff(&target);
        let new = DiffContent::Text("new text\n".to_owned());
        content_diff(&mut out, "copy", &target, &current, &new);

        // The placeholder pair must appear, and the new text must NOT be
        // rendered as a full-insert line diff against an assumed-empty target.
        assert!(
            out.contains("  - (binary content, 3 bytes)"),
            "the binary current side must render as a placeholder, got:\n{out}"
        );
        assert!(
            out.contains("  + (text, 9 bytes)"),
            "the incoming text side must render as a compact descriptor, got:\n{out}"
        );
        assert!(
            !out.contains("new text"),
            "a binary target must not be line-diffed as if empty, got:\n{out}"
        );
    }

    #[test]
    fn content_diff_marks_deletions_equals_and_unterminated_lines() {
        // Exercise all three change tags plus the no-trailing-newline branch:
        // "same" is Equal, "old" is Delete, "new" is Insert, and the final
        // "tail" (no trailing newline) forces the appended newline.
        let mut out = String::new();
        let current = DiffContent::Text("same\nold\ntail".to_owned());
        let new = DiffContent::Text("same\nnew\ntail".to_owned());
        content_diff(&mut out, "copy", Utf8Path::new("/t"), &current, &new);

        assert!(
            out.contains("    same\n"),
            "unchanged line marked Equal, got:\n{out}"
        );
        assert!(
            out.contains("  - old\n"),
            "removed line marked Delete, got:\n{out}"
        );
        assert!(
            out.contains("  + new\n"),
            "added line marked Insert, got:\n{out}"
        );
        // The unterminated "tail" line is emitted with an appended newline.
        assert!(
            out.ends_with("    tail\n"),
            "unterminated line gets a newline, got:\n{out}"
        );
    }

    #[test]
    fn content_diff_still_line_diffs_a_create_full_insert() {
        let (_td, dir) = tempdir();
        // Absent current + text new is the normal create path: it must remain a
        // line-level full insert, not fall into the opaque placeholder branch.
        let target = dir.join("absent");
        let mut out = String::new();
        let current = read_for_diff(&target);
        let new = DiffContent::Text("line1\nline2\n".to_owned());
        content_diff(&mut out, "copy", &target, &current, &new);

        assert!(
            out.contains("  + line1"),
            "create must insert line1, got:\n{out}"
        );
        assert!(
            out.contains("  + line2"),
            "create must insert line2, got:\n{out}"
        );
        assert!(
            !out.contains("(absent)"),
            "an absent target paired with text must line-diff, not use the placeholder, got:\n{out}"
        );
    }
}
