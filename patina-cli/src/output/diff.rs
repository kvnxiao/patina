//! Embedded diff rendering with the `similar` crate (REQ-017).
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
//! declared). T-021 builds its byte-identical-stdout property on this.

use camino::Utf8Path;
use patina_core::FileMode;
use patina_core::ResolvedOperation;
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

    for op in &resolved.operations {
        for target in &op.targets {
            render_operation(&mut out, op, target, &engine, vars)?;
        }
    }
    Ok(out)
}

/// Render one `(operation, target)` pair into `out`.
fn render_operation(
    out: &mut String,
    op: &ResolvedOperation,
    target: &Utf8Path,
    engine: &TemplateEngine,
    vars: &Resolver,
) -> Result<(), String> {
    match op.mode {
        FileMode::Symlink | FileMode::SymlinkDir => {
            let current = current_link_target(target);
            emit(out, format_args!("symlink {target}\n"));
            emit(
                out,
                format_args!("  - {}\n", current.as_deref().unwrap_or("(absent)")),
            );
            emit(out, format_args!("  + {}\n", op.source));
        }
        FileMode::Copy | FileMode::CopyTree => {
            let new_content = fs_err::read_to_string(&op.source).unwrap_or_default();
            content_diff(out, "copy", target, &new_content);
        }
        FileMode::TemplateRender => {
            let body = fs_err::read_to_string(&op.source)
                .map_err(|e| format!("failed to read template {}: {e}", op.source))?;
            let rendered = engine
                .render(&body, vars)
                .map_err(|e| format!("failed to render template {}: {e}", op.source))?;
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
