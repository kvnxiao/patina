//! Count-based backup retention: keep the newest N apply cycles, GC the
//! rest.

use super::BackupError;
use camino::Utf8Path;

/// Remove every backup subdirectory under `backups_dir` except the newest
/// `keep`, returning the names of the directories that were removed in
/// chronological (lexical) order.
///
/// Backup subdirectories are named by the apply timestamp, whose textual
/// form sorts lexically into chronological order, so "newest `keep`" is
/// the lexical tail of the sorted directory names. Only directories are
/// considered; stray files directly under `backups_dir` are ignored.
///
/// Call this *after* an apply's `COMMIT` sentinel is durable. A failed
/// apply — one that never committed — simply does not call this, so its
/// historical backups are left intact.
///
/// A missing `backups_dir`, or one holding `keep` or fewer subdirectories,
/// removes nothing and returns an empty list.
///
/// # Errors
///
/// Returns [`BackupError::Filesystem`] if the directory cannot be read or
/// a stale subdirectory cannot be removed.
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use patina_core::backups::{gc_retain, RETENTION_COUNT};
///
/// let backups = Utf8Path::new("/state/patina/backups");
/// let removed = gc_retain(backups, RETENTION_COUNT)?;
/// println!("pruned {} stale backup cycle(s)", removed.len());
/// # Ok::<(), patina_core::backups::BackupError>(())
/// ```
pub fn gc_retain(
    backups_dir: impl AsRef<Utf8Path>,
    keep: usize,
) -> Result<Vec<String>, BackupError> {
    let backups_dir = backups_dir.as_ref();

    if !backups_dir.exists() {
        // No backup tree yet — nothing to prune.
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in fs_err::read_dir(backups_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            // Only timestamped cycle directories are retention candidates.
            continue;
        }
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }

    // Lex sort == chronological order for the timestamp directory names,
    // so the trailing `keep` entries are the newest cycles to retain.
    names.sort();

    let cutoff = names.len().saturating_sub(keep);
    let mut removed = Vec::with_capacity(cutoff);
    for name in names.into_iter().take(cutoff) {
        fs_err::remove_dir_all(backups_dir.join(&name))?;
        removed.push(name);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    struct Fixture {
        _temp: TempDir,
        backups: Utf8PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = TempDir::new().expect("tempdir");
        let backups = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .join("backups");
        fs_err::create_dir_all(&backups).expect("create backups dir");
        Fixture {
            _temp: temp,
            backups,
        }
    }

    /// Create `count` timestamped subdirectories whose names sort
    /// chronologically (`00000`..`count`), each holding one marker file so
    /// removal exercises the recursive path.
    fn seed(backups: &Utf8Path, count: usize) -> Vec<String> {
        let mut names = Vec::with_capacity(count);
        for i in 0..count {
            let name = format!("2026{i:05}");
            let dir = backups.join(&name);
            fs_err::create_dir_all(&dir).expect("mkdir cycle");
            fs_err::write(dir.join("marker"), b"x").expect("write marker");
            names.push(name);
        }
        names
    }

    fn surviving_dirs(backups: &Utf8Path) -> Vec<String> {
        let mut got: Vec<String> = fs_err::read_dir(backups)
            .expect("read backups")
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        got.sort();
        got
    }

    #[test]
    fn prunes_oldest_keeping_the_newest_keep() {
        let f = fixture();
        let names = seed(&f.backups, 15);

        let removed = gc_retain(&f.backups, 10).expect("retain newest ten");

        // The five oldest are removed, the ten newest survive.
        let (oldest_five, newest_ten) = names.split_at(5);
        assert_eq!(removed, oldest_five);
        assert_eq!(surviving_dirs(&f.backups), newest_ten);
    }

    #[test]
    fn fewer_than_keep_removes_nothing() {
        let f = fixture();
        let names = seed(&f.backups, 3);

        let removed = gc_retain(&f.backups, 10).expect("retain");

        assert!(removed.is_empty(), "nothing to prune below the keep count");
        assert_eq!(surviving_dirs(&f.backups), names);
    }

    #[test]
    fn exactly_keep_removes_nothing() {
        let f = fixture();
        let names = seed(&f.backups, 10);

        let removed = gc_retain(&f.backups, 10).expect("retain");

        assert!(removed.is_empty());
        assert_eq!(surviving_dirs(&f.backups), names);
    }

    #[test]
    fn stray_files_are_not_retention_candidates() {
        let f = fixture();
        let names = seed(&f.backups, 12);
        // A non-directory entry directly under backups/ must be ignored by
        // the candidate scan rather than counted toward `keep` or removed.
        fs_err::write(f.backups.join("README"), b"not a cycle").expect("write stray");

        let removed = gc_retain(&f.backups, 10).expect("retain");

        let (oldest_two, _) = names.split_at(2);
        assert_eq!(removed, oldest_two);
        assert!(
            f.backups.join("README").exists(),
            "a stray file must survive retention untouched"
        );
    }

    #[test]
    fn missing_backups_dir_is_a_clean_no_op() {
        let temp = TempDir::new().expect("tempdir");
        let nope = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .join("nope");
        let removed = gc_retain(&nope, 10).expect("missing dir is a no-op");
        assert!(removed.is_empty());
    }
}
