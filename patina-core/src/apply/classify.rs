//! Plan-time classification of one resolved leaf into a [`Disposition`]
//! (REQ-001).
//!
//! Given a resolved leaf `(mode, source, target, rendered-bytes?)`, the
//! classifier reads the live target state and decides whether applying
//! would **create** the target (it is absent), **update** it (present but
//! differs from what Patina would write), or leave it **unchanged** (present
//! and already matches).
//!
//! The "matches" test is the same one `status` uses to classify `Clean`:
//! the symlink and content comparisons route through the shared
//! [`symlink_matches`](crate::status::classify::symlink_matches) and
//! [`content_matches`](crate::status::classify::content_matches) seams, so
//! "Unchanged" coincides exactly with status's "Clean" with no third
//! definition of "matches" (REQ-001).

use crate::config::FileMode;
use crate::journal::Disposition;
use crate::journal::content_hash;
use crate::status::classify::content_matches;
use crate::status::classify::symlink_matches;
use camino::Utf8Path;

/// Classify a resolved leaf against its live target.
///
/// `mode` selects the comparison; `source` is the canonical absolute source
/// path; `target` is the canonical absolute target path. `rendered` carries
/// the freshly rendered template output and is required for (and only for)
/// [`TemplateRender`](FileMode::TemplateRender): the caller renders the
/// `.tmpl` source once and hands the bytes here so a clean re-apply does not
/// render twice within this function.
///
/// Per DEC-007 this classifies a single leaf; tree modes
/// ([`CopyTree`](FileMode::CopyTree), [`SymlinkTree`](FileMode::SymlinkTree))
/// are classified one leaf at a time by the caller, passing the per-leaf
/// `source`/`target`.
///
/// # Errors
///
/// Returns [`ClassifyError::Io`] if a present copy/copy-tree leaf's source
/// cannot be read to hash it. A symlink leaf reads no source (it compares
/// the link target string), and a template leaf compares against the
/// caller-supplied `rendered` bytes, so neither hits this path. An absent
/// or unreadable *target* is never an error — it classifies `Create` or
/// `Update` respectively.
pub(crate) fn classify_leaf(
    mode: FileMode,
    source: &Utf8Path,
    target: &Utf8Path,
    rendered: Option<&[u8]>,
) -> Result<Disposition, ClassifyError> {
    // A target that is not present on disk is always a Create, regardless of
    // mode. `symlink_metadata` (not `metadata`) so a symlink target counts as
    // present even when it dangles — matching how `status::classify` decides
    // existence.
    if fs_err::symlink_metadata(target).is_err() {
        return Ok(Disposition::Create);
    }

    let unchanged = match mode {
        FileMode::Symlink | FileMode::SymlinkDir | FileMode::SymlinkTree => {
            // A symlink leaf is satisfied when the live link points at the
            // desired source. The shared seam compares on the
            // `simplified_str` form.
            symlink_matches(target, source.as_str())
        }
        FileMode::Copy | FileMode::CopyTree => {
            // A copy leaf is satisfied when the target's content hashes equal
            // to the source's. Read and hash the source; the shared seam
            // reads and hashes the target.
            let source_bytes = fs_err::read(source).map_err(|source_err| ClassifyError::Io {
                path: source.to_path_buf(),
                source: source_err,
            })?;
            content_matches(target, &content_hash(&source_bytes))
        }
        FileMode::TemplateRender => {
            // A template leaf is satisfied when the target's bytes equal the
            // freshly rendered output the caller produced. Compare via the
            // shared content seam so the same hash primitive decides it.
            let rendered = rendered.unwrap_or(&[]);
            content_matches(target, &content_hash(rendered))
        }
    };

    Ok(if unchanged {
        Disposition::Unchanged
    } else {
        Disposition::Update
    })
}

/// A failure classifying a leaf at plan time (SPEC-0005 REQ-001).
///
/// Surfaced through
/// [`EngineError::Classify`](crate::error::EngineError::Classify) from the plan
/// path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClassifyError {
    /// A copy/copy-tree leaf's source could not be read to hash it. The
    /// engine canonicalizes sources before classification, so this is a
    /// genuine read failure (permissions, a source removed mid-plan) rather
    /// than a path slip.
    #[error("failed to read source {path} for classification: {source}")]
    Io {
        /// The source path whose read failed.
        path: camino::Utf8PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::classify::TargetState;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("create tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        let canonical = path.canonicalize_utf8().expect("canonicalize tempdir");
        (td, canonical)
    }

    /// Create a symbolic link at `link` pointing at `target`, cross-platform.
    fn make_symlink(target: &Utf8Path, link: &Utf8Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("create unix symlink");
        #[cfg(windows)]
        {
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(target, link)
                    .expect("create windows dir symlink");
            } else {
                std::os::windows::fs::symlink_file(target, link)
                    .expect("create windows file symlink");
            }
        }
    }

    // --- symlink family: Create / Update / Unchanged ----------------------

    #[test]
    fn symlink_pointing_at_source_is_unchanged() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let target = dir.join("link");
        make_symlink(&source, &target);

        assert_eq!(
            classify_leaf(FileMode::Symlink, &source, &target, None).expect("classify"),
            Disposition::Unchanged
        );
    }

    #[test]
    fn symlink_pointing_elsewhere_is_update() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let other = dir.join("other");
        fs_err::write(&other, b"other").expect("write other");
        let target = dir.join("link");
        make_symlink(&other, &target);

        assert_eq!(
            classify_leaf(FileMode::Symlink, &source, &target, None).expect("classify"),
            Disposition::Update
        );
    }

    #[test]
    fn symlink_target_absent_is_create() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let target = dir.join("missing-link");

        assert_eq!(
            classify_leaf(FileMode::Symlink, &source, &target, None).expect("classify"),
            Disposition::Create
        );
    }

    #[test]
    fn symlink_target_is_regular_file_is_update() {
        // A regular file where a symlink is desired is present-but-not-a-link:
        // Update, not Unchanged.
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let target = dir.join("target");
        fs_err::write(&target, b"payload").expect("write a real file at the target");

        assert_eq!(
            classify_leaf(FileMode::Symlink, &source, &target, None).expect("classify"),
            Disposition::Update
        );
    }

    // --- copy family: Create / Update / Unchanged -------------------------

    #[test]
    fn copy_matching_bytes_is_unchanged() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"identical").expect("write source");
        let target = dir.join("dst");
        fs_err::write(&target, b"identical").expect("write target");

        assert_eq!(
            classify_leaf(FileMode::Copy, &source, &target, None).expect("classify"),
            Disposition::Unchanged
        );
    }

    #[test]
    fn copy_differing_bytes_is_update() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"new").expect("write source");
        let target = dir.join("dst");
        fs_err::write(&target, b"old").expect("write target");

        assert_eq!(
            classify_leaf(FileMode::Copy, &source, &target, None).expect("classify"),
            Disposition::Update
        );
    }

    #[test]
    fn copy_target_absent_is_create() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"bytes").expect("write source");
        let target = dir.join("absent");

        assert_eq!(
            classify_leaf(FileMode::Copy, &source, &target, None).expect("classify"),
            Disposition::Create
        );
    }

    #[test]
    fn copy_tree_leaf_classifies_like_copy() {
        // A copy-tree leaf is a single regular-file comparison, identical to
        // copy. One leaf matching, one differing.
        let (_td, dir) = utf8_tempdir();
        let src_clean = dir.join("s_clean");
        let dst_clean = dir.join("d_clean");
        fs_err::write(&src_clean, b"same").expect("write");
        fs_err::write(&dst_clean, b"same").expect("write");
        let src_drift = dir.join("s_drift");
        let dst_drift = dir.join("d_drift");
        fs_err::write(&src_drift, b"new").expect("write");
        fs_err::write(&dst_drift, b"old").expect("write");

        assert_eq!(
            classify_leaf(FileMode::CopyTree, &src_clean, &dst_clean, None).expect("classify"),
            Disposition::Unchanged
        );
        assert_eq!(
            classify_leaf(FileMode::CopyTree, &src_drift, &dst_drift, None).expect("classify"),
            Disposition::Update
        );
    }

    #[test]
    fn copy_unreadable_source_surfaces_io_error() {
        // The target is present (so we pass the Create short-circuit) but the
        // source cannot be read: the classifier surfaces the typed error
        // rather than mis-classifying.
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("does-not-exist");
        let target = dir.join("dst");
        fs_err::write(&target, b"present").expect("write target");

        let err = classify_leaf(FileMode::Copy, &source, &target, None)
            .expect_err("missing source must error");
        assert!(matches!(err, ClassifyError::Io { .. }));
    }

    // --- template: Create / Update / Unchanged ----------------------------

    #[test]
    fn template_matching_rendered_output_is_unchanged() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("conf.tmpl");
        let target = dir.join("conf");
        let rendered = b"name = kevin";
        fs_err::write(&target, rendered).expect("write target");

        assert_eq!(
            classify_leaf(FileMode::TemplateRender, &source, &target, Some(rendered))
                .expect("classify"),
            Disposition::Unchanged
        );
    }

    #[test]
    fn template_differing_output_is_update() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("conf.tmpl");
        let target = dir.join("conf");
        fs_err::write(&target, b"name = old").expect("write target");

        assert_eq!(
            classify_leaf(
                FileMode::TemplateRender,
                &source,
                &target,
                Some(b"name = new")
            )
            .expect("classify"),
            Disposition::Update
        );
    }

    #[test]
    fn template_target_absent_is_create() {
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("conf.tmpl");
        let target = dir.join("absent");

        assert_eq!(
            classify_leaf(
                FileMode::TemplateRender,
                &source,
                &target,
                Some(b"rendered")
            )
            .expect("classify"),
            Disposition::Create
        );
    }

    // --- the shared-primitive tie: status Clean <=> Unchanged -------------

    #[test]
    fn state_status_calls_clean_classifies_unchanged_symlink() {
        // Drive both `status::classify` and the plan-time classifier on the
        // exact same live symlink state and assert they agree: Clean <=>
        // Unchanged, with no second definition of "matches" (REQ-001).
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"payload").expect("write source");
        let target = dir.join("link");
        make_symlink(&source, &target);

        // status side: build the ExpectedTarget status would have recorded.
        let expected = crate::journal::ExpectedTarget::Symlink {
            target: target.as_str().to_owned(),
            link_target: source.as_str().to_owned(),
            entry: 0,
            disposition: Disposition::Create,
        };
        assert_eq!(
            crate::status::classify::classify(&expected, true),
            TargetState::Clean
        );
        // plan side: same live state classifies Unchanged.
        assert_eq!(
            classify_leaf(FileMode::Symlink, &source, &target, None).expect("classify"),
            Disposition::Unchanged
        );
    }

    #[test]
    fn state_status_calls_clean_classifies_unchanged_content() {
        // Same tie for the content comparison (copy/template share the seam).
        let (_td, dir) = utf8_tempdir();
        let source = dir.join("src");
        fs_err::write(&source, b"identical").expect("write source");
        let target = dir.join("dst");
        fs_err::write(&target, b"identical").expect("write target");

        let expected = crate::journal::ExpectedTarget::Content {
            target: target.as_str().to_owned(),
            source: source.as_str().to_owned(),
            hash: content_hash(b"identical"),
            entry: 0,
            disposition: Disposition::Update,
        };
        assert_eq!(
            crate::status::classify::classify(&expected, true),
            TargetState::Clean
        );
        assert_eq!(
            classify_leaf(FileMode::Copy, &source, &target, None).expect("classify"),
            Disposition::Unchanged
        );
    }
}
