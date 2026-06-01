//! Implicit template-render executor (REQ-005).
//!
//! Templating keys off the **source** `.tmpl` suffix (REQ-005): a source
//! file ending in `.tmpl` is rendered through the shared T-008 `MiniJinja`
//! [`Engine`] **exactly once** against the resolved variable context, and
//! the same rendered bytes are written to each declared target. The target
//! is declared as its final, suffix-less path (`source = "gitconfig.tmpl"`,
//! `target = "~/.gitconfig"`), so the executor writes to the target
//! verbatim — it does not strip anything from the target. The materialized
//! object is a regular file, never a symlink. Rendering once (rather than
//! per target) is the REQ-005 guarantee a multi-target `.tmpl` entry must
//! honour.

use super::CompletionRecord;
use super::ExecutorError;
use super::ensure_parent;
use super::with_sharing_violation_retry;
use crate::template::Engine;
use crate::variables::Resolver;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Render a `.tmpl` source once and write the result to each declared
/// target verbatim (REQ-005 declares targets suffix-less).
pub(super) fn render(
    source: &Utf8Path,
    targets: &[Utf8PathBuf],
    engine: &Engine,
    resolver: &Resolver,
) -> Result<Vec<CompletionRecord>, ExecutorError> {
    if !source.exists() {
        return Err(ExecutorError::SourceMissing {
            path: source.to_path_buf(),
        });
    }
    // Templating is keyed off the source suffix; a non-`.tmpl` source
    // never reaches the render executor, but guard it so the executor
    // refuses to render a path the engine should never have classified.
    if !has_tmpl_suffix(source) {
        return Err(ExecutorError::NotATemplate {
            path: source.to_path_buf(),
        });
    }

    let body = fs_err::read_to_string(source).map_err(|err| ExecutorError::Io {
        path: source.to_path_buf(),
        source: err,
    })?;
    // Render exactly once; reuse the bytes for every target.
    let rendered = engine.render(&body, resolver)?;

    let mut records = Vec::with_capacity(targets.len());
    for target in targets {
        ensure_parent(target)?;
        with_sharing_violation_retry(|| fs_err::write(target, rendered.as_bytes())).map_err(
            |err| ExecutorError::Io {
                path: target.clone(),
                source: err,
            },
        )?;
        records.push(CompletionRecord::render(
            source.to_path_buf(),
            target.clone(),
        ));
    }
    Ok(records)
}

/// Whether `source` carries a trailing `.tmpl` (case-insensitive)
/// extension — the marker that makes an entry an implicit template render.
fn has_tmpl_suffix(source: &Utf8Path) -> bool {
    source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tmpl"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variables::Builtins;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("create tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
        (td, canonical)
    }

    fn resolver_with(key: &str, value: &str) -> Resolver {
        Resolver::new(Builtins::for_tests())
            .with_repo_shared([(key, value)])
            .expect("layer accepted")
    }

    #[test]
    fn renders_once_and_writes_each_declared_target() {
        let (_td, dir) = utf8_tempdir();
        // Source carries `.tmpl`; targets are declared suffix-less per REQ-005.
        let source = dir.join("agent.toml.tmpl");
        fs_err::write(&source, b"name = {{ who }}").expect("write template");
        let t1 = dir.join("claude").join("agent.toml");
        let t2 = dir.join("codex").join("agent.toml");

        let records = render(
            &source,
            &[t1.clone(), t2.clone()],
            &Engine::new(),
            &resolver_with("who", "kevin"),
        )
        .expect("render");

        // The executor writes to the declared targets verbatim.
        let written: Vec<&Utf8PathBuf> = records.iter().map(|r| &r.target).collect();
        assert_eq!(written, vec![&t1, &t2]);
        assert_eq!(fs_err::read(&t1).expect("read t1"), b"name = kevin");
        assert_eq!(fs_err::read(&t2).expect("read t2"), b"name = kevin");
        // Output is a regular file, not a symlink.
        assert!(
            !fs_err::symlink_metadata(&t1)
                .expect("t1 metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn undefined_variable_surfaces_template_error() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("gitconfig.tmpl");
        fs_err::write(&source, b"email = {{ patina.profile_email }}").expect("write template");
        let target = dir.join("gitconfig");
        let err = render(
            &source,
            &[target],
            &Engine::new(),
            &Resolver::new(Builtins::for_tests()),
        )
        .expect_err("undefined variable must fail render");
        assert!(matches!(err, ExecutorError::Template(_)));
    }

    #[test]
    fn non_tmpl_source_is_rejected() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("gitconfig");
        fs_err::write(&source, b"name = {{ who }}").expect("write source");
        let target = dir.join("out");
        let err = render(
            &source,
            &[target],
            &Engine::new(),
            &resolver_with("who", "kevin"),
        )
        .expect_err("non-`.tmpl` source must not render");
        assert!(matches!(err, ExecutorError::NotATemplate { .. }));
    }

    #[test]
    fn has_tmpl_suffix_handles_case_and_absence() {
        assert!(has_tmpl_suffix(Utf8Path::new("/x/y.toml.tmpl")));
        assert!(has_tmpl_suffix(Utf8Path::new("/x/y.toml.TMPL")));
        assert!(!has_tmpl_suffix(Utf8Path::new("/x/y.toml")));
    }
}
