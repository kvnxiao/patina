//! Implicit template-render executor (REQ-005).
//!
//! A `.tmpl` source is rendered through the shared T-008 `MiniJinja`
//! [`Engine`] **exactly once** against the resolved variable context, and
//! the same rendered bytes are written to each target. The target path is
//! the declared target with the `.tmpl` suffix stripped; the materialized
//! object is a regular file, never a symlink. Rendering once (rather than
//! per target) is the REQ-005 guarantee a multi-target `.tmpl` entry must
//! honour.

use super::CompletionRecord;
use super::ExecutorError;
use super::ensure_parent;
use crate::template::Engine;
use crate::variables::Resolver;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// Render a `.tmpl` source once and write the result to each target with
/// the `.tmpl` suffix stripped.
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

    let body = fs_err::read_to_string(source).map_err(|err| ExecutorError::Io {
        path: source.to_path_buf(),
        source: err,
    })?;
    // Render exactly once; reuse the bytes for every target.
    let rendered = engine.render(&body, resolver)?;

    let mut records = Vec::with_capacity(targets.len());
    for target in targets {
        let output = strip_tmpl_suffix(target).ok_or_else(|| ExecutorError::NotATemplate {
            path: target.to_path_buf(),
        })?;
        ensure_parent(&output)?;
        fs_err::write(&output, rendered.as_bytes()).map_err(|err| ExecutorError::Io {
            path: output.clone(),
            source: err,
        })?;
        records.push(CompletionRecord::render(source.to_path_buf(), output));
    }
    Ok(records)
}

/// Strip a trailing `.tmpl` (case-insensitive) extension from `target`,
/// returning the suffix-less path. Returns `None` when the path does not
/// end in `.tmpl`, so the executor refuses to overwrite a non-template
/// path rather than collide source and output.
fn strip_tmpl_suffix(target: &Utf8Path) -> Option<Utf8PathBuf> {
    let ext = target.extension()?;
    if !ext.eq_ignore_ascii_case("tmpl") {
        return None;
    }
    Some(target.with_extension(""))
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
    fn renders_once_and_writes_each_target_without_suffix() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("agent.toml.tmpl");
        fs_err::write(&source, b"name = {{ who }}").expect("write template");
        let t1 = dir.join("claude").join("agent.toml.tmpl");
        let t2 = dir.join("codex").join("agent.toml.tmpl");

        let records = render(
            &source,
            &[t1.clone(), t2.clone()],
            &Engine::new(),
            &resolver_with("who", "kevin"),
        )
        .expect("render");

        assert_eq!(records.len(), 2);
        let out1 = dir.join("claude").join("agent.toml");
        let out2 = dir.join("codex").join("agent.toml");
        assert_eq!(fs_err::read(&out1).expect("read out1"), b"name = kevin");
        assert_eq!(fs_err::read(&out2).expect("read out2"), b"name = kevin");
        // The `.tmpl` paths themselves must not exist at the targets.
        assert!(!t1.exists());
        assert!(!t2.exists());
        // Output is a regular file, not a symlink.
        assert!(
            !fs_err::symlink_metadata(&out1)
                .expect("out1 metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn undefined_variable_surfaces_template_error() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("gitconfig.tmpl");
        fs_err::write(&source, b"email = {{ patina.profile_email }}").expect("write template");
        let target = dir.join("gitconfig.tmpl");
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
    fn strip_tmpl_suffix_handles_case_and_absence() {
        assert_eq!(
            strip_tmpl_suffix(Utf8Path::new("/x/y.toml.tmpl")),
            Some(Utf8PathBuf::from("/x/y.toml"))
        );
        assert_eq!(
            strip_tmpl_suffix(Utf8Path::new("/x/y.toml.TMPL")),
            Some(Utf8PathBuf::from("/x/y.toml"))
        );
        assert_eq!(strip_tmpl_suffix(Utf8Path::new("/x/y.toml")), None);
    }
}
