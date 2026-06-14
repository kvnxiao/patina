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
            let new_content = fs_err::read_to_string(source).unwrap_or_default();
            content_diff(out, "copy", target, &new_content);
        }
        FileMode::TemplateRender => {
            let body = fs_err::read_to_string(source)
                .map_err(|e| format!("failed to read template {source}: {e}"))?;
            let rendered = engine
                .render(&body, vars)
                .map_err(|e| format!("failed to render template {source}: {e}"))?;
            content_diff(out, "render", target, &rendered);
        }
    }
    Ok(())
}

/// Append a line-level content diff between the target's current content
/// and `new_content` under the action label.
fn content_diff(out: &mut String, action: &str, target: &Utf8Path, new_content: &str) {
    let current = fs_err::read_to_string(target).unwrap_or_default();
    emit(out, format_args!("{action} {target}\n"));
    let diff = TextDiff::from_lines(current.as_str(), new_content);
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
