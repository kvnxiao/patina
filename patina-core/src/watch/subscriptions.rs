//! Computing the watcher's FS subscription set from a committed journal
//! record (REQ-005).
//!
//! The watcher never watches the repository tree recursively. It reads the
//! most recent committed journal record (the `<ts>.COMMIT` written by the
//! last apply, recovered via [`read_latest_commit`]) and from that record
//! alone derives exactly which paths to subscribe to:
//!
//! - every target's canonical **source** ([`ExpectedTarget::source`]) — the
//!   repository path the target was materialized from;
//! - every **content** (copy- or template-mode) target's path — a regular file
//!   Patina owns and must re-hash on change;
//! - the per-machine state directory's `journal/` subdirectory itself, so a new
//!   `.plan` / `.COMMIT` from any apply re-triggers a journal rescan (the
//!   journal-rescan subscription, REQ-005 `<done-when>`).
//!
//! Symlink **target** paths are deliberately *not* subscribed (DEC-008):
//! modifying a symlinked target is modifying its source, which is already
//! watched via the source path above. Only the source side of a symlink
//! entry appears in the set.
//!
//! This module is the pure mapping from record to path set — it does no
//! `notify` wiring. The foreground watcher (T-008) hands the computed set to
//! the debouncer.
//!
//! [`read_latest_commit`]: crate::read_latest_commit

use crate::journal::ApplyRecord;
use crate::journal::ExpectedTarget;
use camino::Utf8Path;
use camino::Utf8PathBuf;

/// The watcher's subscription set, partitioned by what an FS event on each
/// path means so the foreground loop can route a debounced batch without
/// re-deriving the record (T-009).
///
/// - `watched` is the flat path list handed to the debouncer (every source,
///   every content-target, and the journal directory) — identical to what
///   [`compute_subscriptions`] returns.
/// - `sources` is the subset of `watched` that are repository **source** paths:
///   an FS event on one of these is a source edit and drives a re-apply
///   (REQ-006). Content-target events do **not** re-apply — they feed drift
///   detection (T-010) — so routing on `sources` is what stops a re-apply's own
///   content-target rewrite from re-triggering itself in an unbounded loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSet {
    /// Every path the debouncer subscribes to, apply order, de-duplicated.
    pub watched: Vec<Utf8PathBuf>,
    /// The source paths within `watched` whose FS events drive a re-apply.
    pub sources: Vec<Utf8PathBuf>,
}

/// Compute the watcher's partitioned [`WatchSet`] from a committed record.
///
/// `watched` is exactly [`compute_subscriptions`]'s output (so the debouncer
/// behaviour is unchanged); `sources` additionally captures which of those
/// paths are repository source paths, so the foreground loop can tell a source
/// edit (re-apply) from a content-target edit (drift, T-010) and a
/// journal-directory event (rescan).
///
/// # Arguments
///
/// * `record` - the most recent committed [`ApplyRecord`].
/// * `state_dir` - the resolved per-machine state directory.
#[must_use = "the watch set must be handed to the debouncer and the classifier to take effect"]
pub fn compute_watch_set(record: &ApplyRecord, state_dir: &Utf8Path) -> WatchSet {
    let watched = compute_subscriptions(record, state_dir);
    let mut sources: Vec<Utf8PathBuf> = Vec::with_capacity(record.targets.len());
    for target in &record.targets {
        let source = Utf8PathBuf::from(target.source());
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    WatchSet { watched, sources }
}

/// Compute the ordered, de-duplicated set of paths the watcher subscribes to
/// for the given committed journal record and resolved per-machine state
/// directory.
///
/// The returned vector preserves apply order: for each recorded target, its
/// source path appears (and, for a content target, the target path follows),
/// then the `<state>/patina/journal/` directory is appended last. Duplicate
/// paths — e.g. two entries sharing one source — collapse to their first
/// occurrence. Symlink target paths never appear (DEC-008).
///
/// The computed set is emitted as a `tracing` info event
/// (`watch_subscriptions`, target `patina_core`) carrying the entry count and
/// the tab-joined paths so the foreground watcher (T-008) and CHK-009 can
/// inspect it from the log.
///
/// # Arguments
///
/// * `record` - the most recent committed [`ApplyRecord`]
///   ([`crate::read_latest_commit`]).
/// * `state_dir` - the resolved per-machine state directory (`<state>/patina`,
///   [`crate::state_dir::resolve`]); its `journal/` subdirectory is the
///   journal-rescan subscription.
///
/// # Examples
///
/// ```
/// use camino::Utf8Path;
/// use patina_core::journal::{ApplyRecord, ExpectedTarget, LastApply};
/// use patina_core::watch::subscriptions::compute_subscriptions;
///
/// let record = ApplyRecord::new(
///     LastApply { at: "2026-05-31T00:00:00Z".into(), user: "u".into(), host: "h".into() },
///     vec![ExpectedTarget::Symlink {
///         target: "/home/u/.vimrc".into(),
///         link_target: "/repo/vim/vimrc".into(),
///         entry: 0,
///     }],
/// );
/// let subs = compute_subscriptions(&record, Utf8Path::new("/state/patina"));
/// assert_eq!(subs, vec![
///     Utf8Path::new("/repo/vim/vimrc"),
///     Utf8Path::new("/state/patina/journal"),
/// ]);
/// ```
#[must_use = "the subscription set must be handed to the debouncer to take effect"]
pub fn compute_subscriptions(record: &ApplyRecord, state_dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut subscriptions: Vec<Utf8PathBuf> = Vec::with_capacity(record.targets.len() + 1);
    let push_unique = |path: Utf8PathBuf, set: &mut Vec<Utf8PathBuf>| {
        if !set.contains(&path) {
            set.push(path);
        }
    };

    for target in &record.targets {
        push_unique(Utf8PathBuf::from(target.source()), &mut subscriptions);
        // Only content (copy / rendered-template) targets are watched on the
        // target side; a symlink target is covered by its source (DEC-008).
        if let ExpectedTarget::Content { target, .. } = target {
            push_unique(Utf8PathBuf::from(target), &mut subscriptions);
        }
    }

    // The journal-rescan subscription: a new `.plan`/`.COMMIT` here re-reads
    // the latest commit and recomputes this set (REQ-005 `<done-when>`).
    push_unique(state_dir.join("journal"), &mut subscriptions);

    tracing::info!(
        target: "patina_core",
        count = subscriptions.len(),
        paths = %subscriptions.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("\t"),
        "watch_subscriptions"
    );

    subscriptions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::LastApply;

    fn last_apply() -> LastApply {
        LastApply {
            at: "2026-05-31T00:00:00Z".into(),
            user: "u".into(),
            host: "h".into(),
        }
    }

    fn symlink(target: &str, link_target: &str, entry: u32) -> ExpectedTarget {
        ExpectedTarget::Symlink {
            target: target.into(),
            link_target: link_target.into(),
            entry,
        }
    }

    fn content(target: &str, source: &str, entry: u32) -> ExpectedTarget {
        ExpectedTarget::Content {
            target: target.into(),
            source: source.into(),
            hash: [0u8; 32],
            entry,
        }
    }

    /// CHK-009: two symlink targets and one content target yield exactly the
    /// three source paths, the one content target path, and the journal
    /// directory — five entries — and contain neither symlink target path.
    #[test]
    fn two_symlinks_one_content_yields_five_subscriptions() {
        let record = ApplyRecord::new(
            last_apply(),
            vec![
                symlink("/home/u/.vimrc", "/repo/vim/vimrc", 0),
                symlink("/home/u/.zshrc", "/repo/zsh/zshrc", 1),
                content("/home/u/.gitconfig", "/repo/git/gitconfig", 2),
            ],
        );

        let subs = compute_subscriptions(&record, Utf8Path::new("/state/patina"));

        assert_eq!(
            subs,
            vec![
                Utf8PathBuf::from("/repo/vim/vimrc"),
                Utf8PathBuf::from("/repo/zsh/zshrc"),
                Utf8PathBuf::from("/repo/git/gitconfig"),
                Utf8PathBuf::from("/home/u/.gitconfig"),
                Utf8PathBuf::from("/state/patina/journal"),
            ]
        );
        // Neither symlink target path is present (DEC-008).
        assert!(!subs.contains(&Utf8PathBuf::from("/home/u/.vimrc")));
        assert!(!subs.contains(&Utf8PathBuf::from("/home/u/.zshrc")));
    }

    /// A record whose only target is a symlink subscribes to the source and
    /// the journal directory, but not the symlink target path.
    #[test]
    fn lone_symlink_omits_target_path() {
        let record = ApplyRecord::new(
            last_apply(),
            vec![symlink("/home/u/.vimrc", "/repo/vim/vimrc", 0)],
        );

        let subs = compute_subscriptions(&record, Utf8Path::new("/state/patina"));

        assert_eq!(
            subs,
            vec![
                Utf8PathBuf::from("/repo/vim/vimrc"),
                Utf8PathBuf::from("/state/patina/journal"),
            ]
        );
        assert!(!subs.contains(&Utf8PathBuf::from("/home/u/.vimrc")));
    }

    /// A content target contributes both its source and its own target path.
    #[test]
    fn content_target_subscribes_both_source_and_target() {
        let record = ApplyRecord::new(
            last_apply(),
            vec![content("/home/u/.gitconfig", "/repo/git/gitconfig", 0)],
        );

        let subs = compute_subscriptions(&record, Utf8Path::new("/state/patina"));

        assert_eq!(
            subs,
            vec![
                Utf8PathBuf::from("/repo/git/gitconfig"),
                Utf8PathBuf::from("/home/u/.gitconfig"),
                Utf8PathBuf::from("/state/patina/journal"),
            ]
        );
    }

    /// Two entries sharing one source path collapse to a single subscription;
    /// order is preserved by first occurrence.
    #[test]
    fn duplicate_source_paths_collapse() {
        let record = ApplyRecord::new(
            last_apply(),
            vec![
                symlink("/home/u/.bashrc", "/repo/shared/rc", 0),
                symlink("/home/u/.zshrc", "/repo/shared/rc", 1),
            ],
        );

        let subs = compute_subscriptions(&record, Utf8Path::new("/state/patina"));

        assert_eq!(
            subs,
            vec![
                Utf8PathBuf::from("/repo/shared/rc"),
                Utf8Path::new("/state/patina/journal").to_path_buf(),
            ]
        );
    }

    /// `compute_watch_set` partitions the flat set into the debouncer's
    /// `watched` list (unchanged) and the `sources` whose events drive a
    /// re-apply. A content target's own path is watched but is NOT a source, so
    /// it does not appear in `sources` — that is what keeps a re-apply's own
    /// target rewrite from re-triggering itself (T-009 loop prevention).
    #[test]
    fn watch_set_separates_sources_from_content_targets() {
        let record = ApplyRecord::new(
            last_apply(),
            vec![
                symlink("/home/u/.vimrc", "/repo/vim/vimrc", 0),
                content("/home/u/.gitconfig", "/repo/git/gitconfig", 1),
            ],
        );

        let set = compute_watch_set(&record, Utf8Path::new("/state/patina"));

        // `watched` matches the flat subscription set exactly.
        assert_eq!(
            set.watched,
            compute_subscriptions(&record, Utf8Path::new("/state/patina"))
        );
        // `sources` holds the two repository source paths and nothing else.
        assert_eq!(
            set.sources,
            vec![
                Utf8PathBuf::from("/repo/vim/vimrc"),
                Utf8PathBuf::from("/repo/git/gitconfig"),
            ]
        );
        // The content target's own path is watched (for drift) but is not a
        // source, so editing it does not drive a re-apply.
        assert!(
            set.watched
                .contains(&Utf8PathBuf::from("/home/u/.gitconfig"))
        );
        assert!(
            !set.sources
                .contains(&Utf8PathBuf::from("/home/u/.gitconfig"))
        );
    }

    /// An empty record still yields the journal-rescan subscription so the
    /// watcher rescans on the next apply.
    #[test]
    fn empty_record_yields_only_journal_dir() {
        let record = ApplyRecord::new(last_apply(), Vec::new());

        let subs = compute_subscriptions(&record, Utf8Path::new("/state/patina"));

        assert_eq!(subs, vec![Utf8PathBuf::from("/state/patina/journal")]);
    }
}
