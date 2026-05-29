//! Per-target classification into CLEAN / DRIFTED / MISSING / ORPHANED
//! (REQ-018).
//!
//! [`classify`] is the pure decision function: given the recorded
//! expectation for one target and whether the *current* repository plan
//! still manages that target, it reads the live filesystem and returns the
//! [`TargetState`]. Keeping it free of IO orchestration lets the status
//! module ([`super`]) own the journal read and the current-plan
//! computation while this function owns the four-way comparison the SPEC's
//! `<done-when>` enumerates.

use crate::journal::ExpectedTarget;
use crate::journal::fingerprint_bytes;
use crate::journal::read_symlink_target;
use camino::Utf8Path;

/// The classification of one managed target against the last apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// Target exists and matches the recorded expectation.
    Clean,
    /// Target exists but its content / link target differs from expected.
    Drifted,
    /// Target was applied but no longer exists on disk.
    Missing,
    /// Target exists on disk but the current plan no longer manages it.
    Orphaned,
}

impl TargetState {
    /// The lower-case word for this state in human and JSON output. The
    /// label is part of the status surface (REQ-018's `files[].state` and
    /// the human rows), so it is defined once here.
    #[must_use = "the label is the value emitted in status output"]
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Drifted => "drifted",
            Self::Missing => "missing",
            Self::Orphaned => "orphaned",
        }
    }
}

/// Classify one recorded target against the live filesystem.
///
/// `still_managed` is `true` when the freshly-computed current plan still
/// manages this target's path. A target the current plan has dropped is
/// ORPHANED while it still exists on disk; once it is gone there is
/// nothing left to report, so it classifies MISSING — but the status
/// module filters dropped-and-absent targets out before display, since a
/// no-longer-managed, no-longer-present target is simply done.
///
/// When the target is still managed, the comparison is the expectation's:
/// a symlink must still point at the recorded link target; a content file
/// must still fingerprint to the recorded value.
#[must_use = "the classification is the per-target status result"]
pub fn classify(expected: &ExpectedTarget, still_managed: bool) -> TargetState {
    let target = Utf8Path::new(expected.target());
    let exists = fs_err::symlink_metadata(target).is_ok();

    if !still_managed {
        // The current repo plan no longer produces this target. If it is
        // still on disk it is an orphan of a removed entry; if it is gone
        // there is nothing to report.
        return if exists {
            TargetState::Orphaned
        } else {
            TargetState::Missing
        };
    }

    if !exists {
        return TargetState::Missing;
    }

    match expected {
        ExpectedTarget::Symlink { link_target, .. } => {
            match read_symlink_target(target) {
                // Compare on the verbatim-stripped form: the recorded link
                // target and the on-disk link may differ only by a Windows
                // `\\?\` prefix for the same destination.
                Some(actual)
                    if super::strip_verbatim_str(&actual)
                        == super::strip_verbatim_str(link_target) =>
                {
                    TargetState::Clean
                }
                // Present but not a link to the recorded source (a link to
                // somewhere else, or replaced by a regular file): drift.
                _ => TargetState::Drifted,
            }
        }
        ExpectedTarget::Content { fingerprint, .. } => match fs_err::read(target) {
            Ok(bytes) if fingerprint_bytes(&bytes) == *fingerprint => TargetState::Clean,
            // Unreadable or fingerprint mismatch: the content changed.
            _ => TargetState::Drifted,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    fn utf8_tempdir() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("tempdir path is utf-8");
        (td, path)
    }

    #[test]
    fn content_match_is_clean_mismatch_is_drifted() {
        let (_td, dir) = utf8_tempdir();
        let target = dir.join("file");
        fs_err::write(&target, b"payload").expect("write");
        let expected = ExpectedTarget::Content {
            target: target.as_str().to_owned(),
            fingerprint: fingerprint_bytes(b"payload"),
        };
        assert_eq!(classify(&expected, true), TargetState::Clean);

        fs_err::write(&target, b"payload-edited").expect("edit");
        assert_eq!(classify(&expected, true), TargetState::Drifted);
    }

    #[test]
    fn deleted_target_is_missing() {
        let (_td, dir) = utf8_tempdir();
        let expected = ExpectedTarget::Content {
            target: dir.join("gone").as_str().to_owned(),
            fingerprint: 0,
        };
        assert_eq!(classify(&expected, true), TargetState::Missing);
    }

    #[test]
    fn unmanaged_but_present_is_orphaned() {
        let (_td, dir) = utf8_tempdir();
        let target = dir.join("old");
        fs_err::write(&target, b"x").expect("write");
        let expected = ExpectedTarget::Content {
            target: target.as_str().to_owned(),
            fingerprint: 0,
        };
        // still_managed = false: the current plan dropped this entry.
        assert_eq!(classify(&expected, false), TargetState::Orphaned);
    }

    #[test]
    fn symlink_to_wrong_target_is_drifted() {
        let expected = ExpectedTarget::Symlink {
            target: "/t/link".to_owned(),
            link_target: "/repo/src".to_owned(),
        };
        // A non-existent path is Missing regardless of link expectation.
        assert_eq!(classify(&expected, true), TargetState::Missing);
    }

    #[test]
    fn label_words_are_stable() {
        assert_eq!(TargetState::Clean.label(), "clean");
        assert_eq!(TargetState::Drifted.label(), "drifted");
        assert_eq!(TargetState::Missing.label(), "missing");
        assert_eq!(TargetState::Orphaned.label(), "orphaned");
    }
}
