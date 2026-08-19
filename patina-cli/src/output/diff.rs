//! Embedded diff rendering with the `similar` crate.
//!
//! The diff is computed from the [`ResolvedPlan`] produced by the engine:
//! for each operation, it compares the target's current on-disk content (or
//! link target) against what the apply would materialize. Copy and
//! template-render modes produce a line-level content diff; symlink modes
//! produce an `old link target -> new link target` line.
//!
//! Output is deterministic: operations render in plan order, and the rendered
//! string contains no timestamps, PIDs, or absolute state-dir paths (only the
//! plan's source path and the target the user declared). The
//! byte-identical-stdout property depends on that determinism.
//!
//! Some content cannot be line-diffed: a present-but-non-UTF-8 (binary) source
//! or target, or an unreadable file. Each renders as a compact, deterministic
//! placeholder, `(binary content, N bytes)` or `(unreadable)`. A binary copy
//! source is legitimate, so the placeholder covers it without raising an
//! error. The user consents from this diff, and an empty or full-insert render
//! of a binary target would mislead them.
//!
//! Removals render as well. An apply reaps every target a prior apply
//! materialized that the current plan no longer manages: an entry dropped from
//! a `patina.toml`, a `when` flipped false, a leaf a new `ignore` pattern now
//! excludes. Those orphan targets are not [`ResolvedPlan`] operations, so the
//! CLI passes them to [`render`] separately. Each renders as a `remove
//! <target>` block whose deleted body is the link it pointed at or its current
//! content, so every reap appears in the consent diff.

use crate::output::style::Styles;
use anstyle::Style;
use camino::Utf8Path;
use patina_core::Disposition;
use patina_core::FileMode;
use patina_core::Orphan;
use patina_core::ResolvedPlan;
use patina_core::Resolver;
use patina_core::TemplateEngine;
use similar::ChangeTag;
use similar::TextDiff;
use std::fmt::Write as _;

/// Render the full plan diff to a deterministic string.
///
/// `orphans` is the reap set the engine would delete this run
/// ([`patina_core::plan_orphans`]): targets a prior apply materialized that
/// the current plan no longer manages, each paired with the reason it is
/// reaped. They are not
/// [`ResolvedPlan`] operations, so the caller passes them in; each renders as a
/// `remove` block after the create/update blocks and before the unchanged
/// summary.
///
/// # Errors
///
/// Returns an error string when a template source cannot be read, or cannot be
/// rendered for preview (the same strict-undefined failure the apply would
/// hit).
pub fn render(resolved: &ResolvedPlan, orphans: &[Orphan]) -> Result<String, String> {
    let mut out = String::new();
    if resolved.operations.is_empty() && orphans.is_empty() {
        out.push_str("No changes: the plan is empty.\n");
        return Ok(out);
    }

    let engine = TemplateEngine::new();
    let vars = &resolved.resolver;
    let styles = Styles::colored();

    // `Unchanged` targets render as one count rather than a block.
    // A tree mode counts materialized leaves, so a drifted tree renders blocks
    // for its drifted leaves and adds its clean leaves to `unchanged`.
    let mut unchanged = 0usize;
    for op in &resolved.operations {
        for (target, disposition) in op.targets.iter().zip(&op.dispositions) {
            // A symlinked tree root renders as one replacement block: the
            // link on the deleted side, the leaf count on the inserted side.
            // Per-leaf blocks would each read a path through the link, so
            // they are suppressed for the op.
            if disposition.replace_root {
                render_root_replacement(
                    &mut out,
                    op.mode,
                    target,
                    disposition.mode_change,
                    disposition.leaves.len(),
                    &styles,
                );
                continue;
            }
            if disposition.leaves.is_empty() {
                if disposition.aggregate == Disposition::Unchanged {
                    unchanged += 1;
                } else {
                    render_leaf(
                        &mut out,
                        op.mode,
                        &op.source,
                        target,
                        disposition.mode_change,
                        &engine,
                        vars,
                        &styles,
                    )?;
                }
            } else {
                // Tree mode: render per materialized leaf, so a single drifted
                // leaf does not put its clean siblings in the diff body.
                for leaf in &disposition.leaves {
                    if leaf.disposition == Disposition::Unchanged {
                        unchanged += 1;
                    } else {
                        let leaf_source = op.source.join(&leaf.relative);
                        let leaf_target = target.join(&leaf.relative);
                        render_leaf(
                            &mut out,
                            op.mode,
                            &leaf_source,
                            &leaf_target,
                            leaf.mode_change,
                            &engine,
                            vars,
                            &styles,
                        )?;
                    }
                }
            }
        }
    }

    // Every orphan the engine would back up and remove renders as a `remove`
    // block, so confirming never silently deletes one. `plan_orphans` sorted
    // the orphans, so the block order is a stable function of the reap set.
    for orphan in orphans {
        render_removal(&mut out, orphan, &styles);
    }

    if unchanged > 0 {
        let noun = if unchanged == 1 { "entry" } else { "entries" };
        emit(&mut out, format_args!("{unchanged} unchanged {noun}.\n"));
    }
    Ok(out)
}

/// The header verb for a mode's plain (non-`replace`) block.
fn mode_verb(mode: FileMode) -> &'static str {
    match mode {
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => "symlink",
        FileMode::Copy | FileMode::CopyTree => "copy",
        FileMode::TemplateRender => "render",
    }
}

/// Render the one-block preview of a symlinked tree root's replacement: the
/// whole-directory link on the deleted side, the materialized leaf count on
/// the inserted side.
///
/// The header carries the `replace` verb only for a provenance-confirmed
/// mode edit; a transferred directory target keeps the plain mode verb with
/// the same body.
fn render_root_replacement(
    out: &mut String,
    mode: FileMode,
    target: &Utf8Path,
    mode_change: bool,
    leaf_count: usize,
    styles: &Styles,
) {
    let header = if mode_change {
        format!("replace {target} (symlink -> tree)")
    } else {
        format!("{} {target}", mode_verb(mode))
    };
    let current = match current_link_target(target) {
        Some(link) => format!("(symlink -> {link})"),
        None => read_for_diff(target).describe(),
    };
    let noun = if leaf_count == 1 { "file" } else { "files" };
    paint_line(out, styles.header, "", &header);
    paint_line(out, styles.delete, "  - ", &current);
    paint_line(
        out,
        styles.insert,
        "  + ",
        &format!("(tree, {leaf_count} {noun})"),
    );
}

/// Render one block for a `(mode, source, target)` triple into `out`. Shared
/// by the single-target path and the tree-mode per-leaf path so a drifted
/// leaf renders the same block shape as a single-target entry of the same
/// mode.
///
/// `mode_change` selects the `replace` header for a provenance-confirmed
/// mode edit. Independently of it, a target whose live entry kind differs
/// from what the mode writes renders kind-aware placeholder bodies: a
/// content header over a live symlink shows the link rather than a content
/// diff read through it, and a symlink header over a live regular file or
/// directory shows `(text, N bytes)` / `(directory)` rather than
/// `(absent)`.
#[expect(
    clippy::too_many_arguments,
    reason = "one block needs the op triple, the replace-verb flag, the \
              template engine/resolver, and the palette; threading a struct \
              here would only move the same fields behind a name."
)]
fn render_leaf(
    out: &mut String,
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    mode_change: bool,
    engine: &TemplateEngine,
    vars: &Resolver,
    styles: &Styles,
) -> Result<(), String> {
    match mode {
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => {
            let header = if mode_change {
                format!("replace {target} (file -> symlink)")
            } else {
                format!("symlink {target}")
            };
            let target_is_dir =
                || fs_err::symlink_metadata(target.as_std_path()).is_ok_and(|meta| meta.is_dir());
            let current = match current_link_target(target) {
                Some(link) => link,
                None if target_is_dir() => "(directory)".to_owned(),
                None => read_for_diff(target).describe(),
            };
            let new = if mode_change {
                format!("(symlink -> {source})")
            } else {
                source.as_str().to_owned()
            };
            paint_line(out, styles.header, "", &header);
            paint_line(out, styles.delete, "  - ", &current);
            paint_line(out, styles.insert, "  + ", &new);
        }
        FileMode::Copy | FileMode::CopyTree => {
            let new = read_for_diff(source);
            render_content_block(out, "copy", target, mode_change, &new, styles);
        }
        FileMode::TemplateRender => {
            let body = fs_err::read_to_string(source)
                .map_err(|e| format!("failed to read template {source}: {e}"))?;
            let rendered = engine
                .render(&body, vars)
                .map_err(|e| format!("failed to render template {source}: {e}"))?;
            let new = DiffContent::Text(rendered);
            render_content_block(out, "render", target, mode_change, &new, styles);
        }
    }
    Ok(())
}

/// Render a content-mode (`copy` / `render`) block, kind-aware at the
/// target.
///
/// A live symlink at the target is never line-diffed: reading through it would
/// diff the linked file's bytes while the entry being replaced is the link.
/// The block shows the link on the deleted side and the incoming content's
/// descriptor on the inserted side, under the `replace` header when the
/// kind flip is a provenance-confirmed mode edit. Any other target renders
/// the ordinary content diff.
fn render_content_block(
    out: &mut String,
    verb: &str,
    target: &Utf8Path,
    mode_change: bool,
    new: &DiffContent,
    styles: &Styles,
) {
    if let Some(link) = current_link_target(target) {
        let header = if mode_change {
            format!("replace {target} (symlink -> file)")
        } else {
            format!("{verb} {target}")
        };
        paint_line(out, styles.header, "", &header);
        paint_line(out, styles.delete, "  - ", &format!("(symlink -> {link})"));
        paint_line(out, styles.insert, "  + ", &new.describe());
        return;
    }
    let current = read_for_diff(target);
    content_diff(out, &format!("{verb} {target}"), &current, new, styles);
}

/// Render one `remove <target> (<reason>)` block for an orphan the reap phase
/// will back up and delete.
///
/// A symlink shows the link it pointed at. Reading *through* it would show the
/// linked file's bytes, not the link being removed. Any other target shows its
/// current content as a full deletion. Binary or unreadable bytes fall back to
/// the compact placeholder, under the same never-imply-empty rule
/// [`content_diff`] uses.
///
/// A reap is the only block that deletes something the user did not ask for in
/// this run, so the header includes the reason. Without the reason, a leaf
/// dropped by a pattern the author wrote minutes ago reads as an unexplained
/// removal.
fn render_removal(out: &mut String, orphan: &Orphan, styles: &Styles) {
    let target = orphan.target.as_path();
    let header = format!("remove {target} ({})", orphan.reason.label());
    if let Some(link) = current_link_target(target) {
        paint_line(out, styles.header, "", &header);
        paint_line(out, styles.delete, "  - ", &link);
        return;
    }
    let current = read_for_diff(target);
    content_diff(out, &header, &current, &DiffContent::Absent, styles);
}

/// A file's content as it should appear on one side of a content diff.
enum DiffContent {
    /// The path does not exist; the apply creates it. Diffs as an empty
    /// "before", so a create renders a full insert.
    Absent,
    /// Valid UTF-8 text, line-diffable.
    Text(String),
    /// Present but not valid UTF-8, or otherwise unreadable. Rendered as a
    /// compact deterministic placeholder instead of a misleading empty diff.
    /// The diff drives the apply consent decision, so the preview must never
    /// imply a binary target is empty.
    Opaque(String),
}

impl DiffContent {
    /// The diffable text. An absent file reads as empty. `None` for content
    /// that cannot be line-diffed (binary / unreadable).
    fn as_text(&self) -> Option<&str> {
        match self {
            DiffContent::Absent => Some(""),
            DiffContent::Text(text) => Some(text),
            DiffContent::Opaque(_) => None,
        }
    }

    /// A compact, deterministic one-line descriptor for the opaque-diff
    /// fallback. It contains only byte counts and fixed words, so stdout stays
    /// byte-identical.
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
/// the content the apply would write, under `header`. When either side
/// cannot be line-diffed (binary / unreadable), render a compact placeholder
/// pair instead of a misleading empty/full-insert diff.
///
/// A reap header ends with a reason tag no other caller wants, so `header` is
/// passed fully formed rather than as an action plus a path.
fn content_diff(
    out: &mut String,
    header: &str,
    current: &DiffContent,
    new: &DiffContent,
    styles: &Styles,
) {
    paint_line(out, styles.header, "", header);
    if let (Some(current), Some(new)) = (current.as_text(), new.as_text()) {
        let diff = TextDiff::from_lines(current, new);
        for change in diff.iter_all_changes() {
            let (style, sign) = match change.tag() {
                ChangeTag::Delete => (styles.delete, "  - "),
                ChangeTag::Insert => (styles.insert, "  + "),
                ChangeTag::Equal => (styles.context, "    "),
            };
            // `similar` yields one line per change. When the source line had a
            // trailing newline, that value keeps it. Stripping it puts the
            // style reset before the newline, and `paint_line` re-appends
            // exactly one, so an unterminated final line still ends with one.
            let value = change.value();
            let line = value.strip_suffix('\n').unwrap_or(value);
            paint_line(out, style, sign, line);
        }
    } else {
        paint_line(out, styles.delete, "  - ", &current.describe());
        paint_line(out, styles.insert, "  + ", &new.describe());
    }
}

/// Write one styled line, `<style><prefix><text><reset>\n`, to `out`. An
/// empty style renders to zero bytes on both the opening code and the reset,
/// so a plain style produces exactly `<prefix><text>\n`, byte-identical to
/// the unstyled form.
fn paint_line(out: &mut String, style: Style, prefix: &str, text: &str) {
    discard(writeln!(
        out,
        "{}{prefix}{text}{}",
        style.render(),
        style.render_reset()
    ));
}

/// If `target` is a symbolic link, read its link target as a UTF-8 string.
/// `None` for anything else, and for a link whose target is not UTF-8.
fn current_link_target(target: &Utf8Path) -> Option<String> {
    let raw = fs_err::read_link(target.as_std_path()).ok()?;
    raw.into_os_string().into_string().ok()
}

/// Append formatted text to an in-memory diff buffer. Writing to a `String`
/// is infallible.
fn emit(out: &mut String, args: std::fmt::Arguments<'_>) {
    discard(out.write_fmt(args));
}

/// Consume an infallible `fmt::Result`. `clippy::let_underscore_must_use`
/// denies a bare `let _`, and the leading underscore keeps the
/// unused-variable lint quiet.
fn discard(_result: std::fmt::Result) {}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use patina_core::OrphanReason;
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
        content_diff(
            &mut out,
            &format!("copy {target}"),
            &current,
            &new,
            &Styles::plain(),
        );

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
        // Every change tag plus the no-trailing-newline branch: "same" is
        // Equal, "old" is Delete, "new" is Insert, and the final "tail" (no
        // trailing newline) forces the appended newline.
        let mut out = String::new();
        let current = DiffContent::Text("same\nold\ntail".to_owned());
        let new = DiffContent::Text("same\nnew\ntail".to_owned());
        content_diff(&mut out, "copy /t", &current, &new, &Styles::plain());

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
        assert!(
            out.ends_with("    tail\n"),
            "an unterminated line still ends with a newline, got:\n{out}"
        );
    }

    #[test]
    fn render_removal_of_a_file_deletes_each_line_under_a_remove_header() {
        let (_td, dir) = tempdir();
        let target = dir.join("gone.conf");
        fs_err::write(&target, "one\ntwo\n").expect("write target");

        let mut out = String::new();
        let orphan = Orphan::new(target.clone(), OrphanReason::Unmanaged);
        render_removal(&mut out, &orphan, &Styles::plain());

        assert!(
            out.contains(&format!("remove {target} (unmanaged)")),
            "the block header must include the removed target and the reason, got:\n{out}"
        );
        assert!(out.contains("  - one\n"), "first line deleted, got:\n{out}");
        assert!(
            out.contains("  - two\n"),
            "second line deleted, got:\n{out}"
        );
        assert!(
            !out.contains("  + "),
            "a removal has no insert side, got:\n{out}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn render_removal_of_a_symlink_shows_the_link_target_not_the_linked_bytes() {
        // Reading *through* the symlink would show the linked file's content;
        // the removal must show the link itself, not what it points at.
        let (_td, dir) = tempdir();
        let linked = dir.join("real.conf");
        fs_err::write(&linked, "linked-bytes\n").expect("write link destination");
        let target = dir.join("link.conf");
        std::os::unix::fs::symlink(linked.as_std_path(), target.as_std_path())
            .expect("create symlink");

        let mut out = String::new();
        let orphan = Orphan::new(target.clone(), OrphanReason::Unmanaged);
        render_removal(&mut out, &orphan, &Styles::plain());

        assert!(
            out.contains(&format!("remove {target} (unmanaged)")),
            "the block header must include the removed symlink and the reason, got:\n{out}"
        );
        assert!(
            out.contains(&format!("  - {linked}")),
            "the deleted body must be the link target, got:\n{out}"
        );
        assert!(
            !out.contains("linked-bytes"),
            "must not read through the symlink to the linked bytes, got:\n{out}"
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
        content_diff(
            &mut out,
            &format!("copy {target}"),
            &current,
            &new,
            &Styles::plain(),
        );

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

    #[cfg(unix)]
    fn make_symlink(source: &Utf8Path, link: &Utf8Path) {
        std::os::unix::fs::symlink(source.as_std_path(), link.as_std_path())
            .expect("create symlink");
    }

    #[cfg(windows)]
    fn make_symlink(source: &Utf8Path, link: &Utf8Path) {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(source.as_std_path(), link.as_std_path())
                .expect("create dir symlink");
        } else {
            std::os::windows::fs::symlink_file(source.as_std_path(), link.as_std_path())
                .expect("create file symlink");
        }
    }

    fn leaf_block(
        mode: patina_core::FileMode,
        source: &Utf8Path,
        target: &Utf8Path,
        mode_change: bool,
    ) -> String {
        let mut out = String::new();
        render_leaf(
            &mut out,
            mode,
            source,
            target,
            mode_change,
            &TemplateEngine::new(),
            &Resolver::new(patina_core::Builtins::current()),
            &Styles::plain(),
        )
        .expect("render block");
        out
    }

    #[test]
    fn mode_change_copy_over_a_live_symlink_renders_the_replace_block() {
        let (_td, dir) = tempdir();
        let source = dir.join("rc");
        fs_err::write(&source, "new text\n").expect("write source");
        let target = dir.join("out");
        make_symlink(&source, &target);

        let out = leaf_block(patina_core::FileMode::Copy, &source, &target, true);

        assert!(
            out.contains(&format!("replace {target} (symlink -> file)")),
            "the header carries the replace verb and the arrow, got:\n{out}"
        );
        assert!(
            out.contains(&format!("  - (symlink -> {source})")),
            "the deleted side is the link placeholder, got:\n{out}"
        );
        assert!(
            out.contains("  + (text, 9 bytes)"),
            "the inserted side is the incoming content's descriptor, got:\n{out}"
        );
        assert!(
            !out.contains("new text"),
            "no content is diffed through the link, got:\n{out}"
        );
    }

    #[test]
    fn mode_change_symlink_over_a_live_file_renders_the_replace_block() {
        let (_td, dir) = tempdir();
        let source = dir.join("rc");
        fs_err::write(&source, "repo text\n").expect("write source");
        let target = dir.join("out");
        fs_err::write(&target, "live text\n").expect("write live file");

        let out = leaf_block(patina_core::FileMode::Symlink, &source, &target, true);

        assert!(
            out.contains(&format!("replace {target} (file -> symlink)")),
            "the header carries the replace verb and the arrow, got:\n{out}"
        );
        assert!(
            out.contains("  - (text, 10 bytes)"),
            "the deleted side is the live file's descriptor, got:\n{out}"
        );
        assert!(
            out.contains(&format!("  + (symlink -> {source})")),
            "the inserted side is the link placeholder, got:\n{out}"
        );
    }

    #[test]
    fn root_replacement_renders_one_replace_block_with_the_tree_arrow() {
        let (_td, dir) = tempdir();
        let source = dir.join("srcdir");
        fs_err::create_dir_all(&source).expect("mkdir source");
        let target = dir.join("out");
        make_symlink(&source, &target);

        let mut out = String::new();
        render_root_replacement(
            &mut out,
            patina_core::FileMode::SymlinkTree,
            &target,
            true,
            2,
            &Styles::plain(),
        );

        assert!(
            out.contains(&format!("replace {target} (symlink -> tree)")),
            "the header carries the replace verb and the tree arrow, got:\n{out}"
        );
        assert!(
            out.contains(&format!("  - (symlink -> {source})")),
            "the deleted side is the whole-directory link, got:\n{out}"
        );
        assert!(
            out.contains("  + (tree, 2 files)"),
            "the inserted side is the leaf count, got:\n{out}"
        );
    }

    #[test]
    fn transferred_root_keeps_the_mode_verb_with_the_same_body() {
        let (_td, dir) = tempdir();
        let source = dir.join("srcdir");
        fs_err::create_dir_all(&source).expect("mkdir source");
        let target = dir.join("out");
        make_symlink(&source, &target);

        let mut out = String::new();
        render_root_replacement(
            &mut out,
            patina_core::FileMode::CopyTree,
            &target,
            false,
            1,
            &Styles::plain(),
        );

        assert!(
            out.contains(&format!("copy {target}")) && !out.contains("replace"),
            "a transfer keeps the plain mode verb, got:\n{out}"
        );
        assert!(
            out.contains(&format!("  - (symlink -> {source})")),
            "the body still shows the link being replaced, got:\n{out}"
        );
        assert!(
            out.contains("  + (tree, 1 file)"),
            "the body still shows the leaf count, got:\n{out}"
        );
    }

    #[test]
    fn transferred_copy_over_a_live_symlink_keeps_the_verb_with_a_kind_aware_body() {
        let (_td, dir) = tempdir();
        let linked = dir.join("elsewhere");
        fs_err::write(&linked, "linked bytes\n").expect("write link destination");
        let source = dir.join("rc");
        fs_err::write(&source, "new text\n").expect("write source");
        let target = dir.join("out");
        make_symlink(&linked, &target);

        let out = leaf_block(patina_core::FileMode::Copy, &source, &target, false);

        assert!(
            out.contains(&format!("copy {target}")) && !out.contains("replace"),
            "a transfer keeps the plain mode verb, got:\n{out}"
        );
        assert!(
            out.contains(&format!("  - (symlink -> {linked})")),
            "the deleted side is the link placeholder, got:\n{out}"
        );
        assert!(
            !out.contains("linked bytes"),
            "no content is diffed through the link, got:\n{out}"
        );
    }

    #[test]
    fn symlink_header_over_a_live_file_describes_it_instead_of_absent() {
        let (_td, dir) = tempdir();
        let source = dir.join("rc");
        fs_err::write(&source, "repo text\n").expect("write source");
        let target = dir.join("out");
        fs_err::write(&target, "live text\n").expect("write live file");

        let out = leaf_block(patina_core::FileMode::Symlink, &source, &target, false);

        assert!(
            out.contains(&format!("symlink {target}")),
            "the plain header keeps the mode verb, got:\n{out}"
        );
        assert!(
            out.contains("  - (text, 10 bytes)"),
            "the live regular file renders its descriptor, got:\n{out}"
        );
        assert!(
            !out.contains("(absent)"),
            "a present file must not read as absent, got:\n{out}"
        );

        let absent = leaf_block(
            patina_core::FileMode::Symlink,
            &source,
            &dir.join("missing"),
            false,
        );
        assert!(
            absent.contains("  - (absent)"),
            "an absent target still renders (absent), got:\n{absent}"
        );
    }

    #[test]
    fn symlink_header_over_a_live_directory_renders_the_directory_descriptor() {
        let (_td, dir) = tempdir();
        let source = dir.join("srcdir");
        fs_err::create_dir_all(&source).expect("mkdir source");
        let target = dir.join("out");
        fs_err::create_dir_all(&target).expect("mkdir live directory target");

        let out = leaf_block(patina_core::FileMode::SymlinkDir, &source, &target, false);

        assert!(
            out.contains("  - (directory)"),
            "a live directory renders its own descriptor, not (unreadable), got:\n{out}"
        );
    }

    #[test]
    fn colored_styles_wrap_changed_lines_while_plain_stays_escape_free() {
        // Context lines use the empty context style, so they stay unstyled
        // under both palettes.
        let current = DiffContent::Text("keep\ndrop\n".to_owned());
        let new = DiffContent::Text("keep\nadd\n".to_owned());

        let mut plain = String::new();
        content_diff(&mut plain, "copy /t", &current, &new, &Styles::plain());
        assert!(
            !plain.contains('\u{1b}'),
            "plain output must contain no escapes: {plain:?}"
        );
        assert!(
            plain.contains("  - drop\n"),
            "the plain delete line is preserved: {plain:?}"
        );
        assert!(
            plain.contains("  + add\n"),
            "the plain insert line is preserved: {plain:?}"
        );

        let mut colored = String::new();
        content_diff(&mut colored, "copy /t", &current, &new, &Styles::colored());
        assert!(
            colored.contains('\u{1b}'),
            "colored output must contain escapes: {colored:?}"
        );
        assert!(
            colored.contains("  - drop"),
            "the delete body is present under color: {colored:?}"
        );
        assert!(
            colored.contains("  + add"),
            "the insert body is present under color: {colored:?}"
        );
        assert!(
            colored.contains("    keep\n"),
            "context line stays unstyled under color: {colored:?}"
        );
    }
}
