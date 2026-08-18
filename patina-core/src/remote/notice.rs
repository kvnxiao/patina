//! The notify-only pending-update notice and the background-check throttle.
//!
//! `<state>/remotes/notice` holds plain text a shell startup can print with
//! builtins alone, with no `patina` process on the prompt path. It
//! distinguishes two situations:
//!
//! - upstream tips have moved past your pins, so `patina apply --update` is the
//!   next step;
//! - your own dotfiles repository is behind its origin (another machine already
//!   bumped the pins), so `git pull && patina apply` is the next step, since
//!   those changes are already decided and gated.
//!
//! `<state>/remotes/last_check` stamps the last real check so
//! `patina remote check --hook` can self-throttle to at most one per day.
//!
//! See `docs/REMOTE_SOURCES.md` "Shell integration".

use super::RemoteError;
use super::RemoteRepr;
use super::cache;
use crate::config::remote::RemoteName;
use camino::Utf8Path;
use std::collections::BTreeSet;
use std::time::Duration;

/// How long `patina remote check --hook` waits between real checks.
pub const HOOK_THROTTLE: Duration = Duration::from_hours(24);

/// Write the notice, or clear it when `message` is `None`.
///
/// Clearing removes the file rather than truncating it, so the shell snippets'
/// `test -s` guard reads false with no extra logic.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the write or removal fails.
pub fn write_notice(state_dir: &Utf8Path, message: Option<&str>) -> Result<(), RemoteError> {
    let path = cache::notice_path(state_dir);
    match message {
        Some(message) => atomic_write(&path, message.as_bytes()),
        None => cache::remove_any(&path),
    }
}

/// Publish the pending-update state whole: the machine-readable `pending`
/// file from `pending`, and the prose `notice` from `message` when one is
/// given, or rendered from `pending` (cleared when nothing is pending)
/// otherwise.
///
/// Every writer of this state goes through here so the two files can never
/// disagree. `message` is how `remote check` applies the precedence rule that
/// a behind repository outranks pending updates.
///
/// # Errors
///
/// Returns the first [`RemoteError`] from a write or removal.
pub fn publish(
    state_dir: &Utf8Path,
    pending: &[String],
    message: Option<&str>,
) -> Result<(), RemoteError> {
    let refs: Vec<&str> = pending.iter().map(String::as_str).collect();
    let rendered;
    let body = match message {
        Some(body) => Some(body),
        None if refs.is_empty() => None,
        None => {
            rendered = pending_updates_message(&refs);
            Some(rendered.as_str())
        }
    };
    write_pending(state_dir, pending)?;
    write_notice(state_dir, body)
}

/// The current notice, or `None` when there is nothing pending.
///
/// An unreadable or empty notice reads as `None`: this is a notification, and
/// failing a command over it would be worse than staying quiet.
#[must_use = "`patina status` and shell snippets surface this notice"]
pub fn read_notice(state_dir: &Utf8Path) -> Option<String> {
    let text = fs_err::read_to_string(cache::notice_path(state_dir).as_std_path()).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The notice for remotes whose upstream tip has moved past the pin.
///
/// `modules` is expected in a stable order, because the caller iterates the
/// name-ordered lockfile. The file's bytes are therefore a function of which
/// remotes are behind, not of iteration order.
#[must_use = "the message is the notice body"]
pub fn pending_updates_message(modules: &[&str]) -> String {
    let names = modules.join(", ");
    let subject = if modules.len() == 1 {
        "remote"
    } else {
        "remotes"
    };
    format!("patina: {subject} with upstream updates: {names}. Run `patina apply --update`.\n")
}

/// The notice for a dotfiles repository that is behind its own origin.
///
/// The pins another machine already bumped are decided and gated, so the user
/// should pull them rather than re-run the gate here.
#[must_use = "the message is the notice body"]
pub fn repo_behind_message() -> String {
    "patina: your dotfiles repository is behind its origin, which may already carry \
     updated remote pins. Run `git pull && patina apply`.\n"
        .to_owned()
}

/// Record which remotes the last check found behind, one module name per line.
///
/// The `notice` file is prose for a human to read at a shell prompt. This file
/// carries the same fact in a form `patina remote list` and `patina status`
/// can report per-remote without parsing English. An empty list removes the
/// file.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the write or removal fails.
pub fn write_pending(state_dir: &Utf8Path, modules: &[String]) -> Result<(), RemoteError> {
    let path = cache::pending_path(state_dir);
    if modules.is_empty() {
        return cache::remove_any(&path);
    }
    let mut body = modules.join("\n");
    body.push('\n');
    atomic_write(&path, body.as_bytes())
}

/// Whether `pending` names `remote`.
///
/// The file was written by an earlier process from the declaration as it was
/// spelled then, so membership folds rather than matching bytes: a declaration
/// respelled between the check and the report is still the same remote.
#[must_use = "the answer is the per-remote pending state `remote list` reports"]
pub fn is_pending(pending: &BTreeSet<String>, remote: &RemoteName) -> bool {
    pending.iter().any(|name| remote.matches(name))
}

/// Drop `names` from the pending-update state, rewriting the machine-readable
/// `pending` file and the prose `notice` to match, and clearing both when
/// nothing remains.
///
/// Settling a pin updates the notice side. A stale announcement therefore does
/// not outlive the update it requested. Only `remote check` otherwise rewrites
/// these files, and its `--hook` form self-throttles for a day.
///
/// A repo-behind notice is left in place. It outranks pending updates, because
/// the user's next move is `git pull` regardless. Only a `check` can learn
/// whether the repository has caught up.
///
/// # Errors
///
/// Returns a [`RemoteError`] when a write or removal fails.
pub fn settle(state_dir: &Utf8Path, names: &[&RemoteName]) -> Result<(), RemoteError> {
    let mut pending = read_pending(state_dir);
    let before = pending.len();
    pending.retain(|entry| !names.iter().any(|name| name.matches(entry)));
    if pending.len() == before {
        return Ok(());
    }
    let remaining: Vec<String> = pending.into_iter().collect();
    if read_notice(state_dir).is_some_and(|body| body == repo_behind_message().trim()) {
        return write_pending(state_dir, &remaining);
    }
    publish(state_dir, &remaining, None)
}

/// The remotes the last check found behind their upstream.
///
/// An absent or unreadable file reads as "none pending": this is notification
/// state, and a stale read is better than a failed command.
#[must_use = "the set is the per-remote pending state `remote list` reports"]
pub fn read_pending(state_dir: &Utf8Path) -> BTreeSet<String> {
    fs_err::read_to_string(cache::pending_path(state_dir).as_std_path())
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The Unix seconds recorded by the last real background check, if any.
#[must_use = "the stamp drives the hook self-throttle"]
pub fn last_check_epoch(state_dir: &Utf8Path) -> Option<i64> {
    fs_err::read_to_string(cache::last_check_path(state_dir).as_std_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Stamp `epoch` as the moment of the last real background check.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the write fails.
pub fn record_check(state_dir: &Utf8Path, epoch: i64) -> Result<(), RemoteError> {
    atomic_write(
        &cache::last_check_path(state_dir),
        epoch.to_string().as_bytes(),
    )
}

/// Write `bytes` to `path` atomically.
///
/// The shell integration can spawn a `patina remote check` in every session,
/// so several processes may write these files at once while a shell's
/// `test -s` reads one. Staging into a same-directory temporary, then
/// renaming, ensures every such reader sees one whole version.
fn atomic_write(path: &Utf8Path, bytes: &[u8]) -> Result<(), RemoteError> {
    crate::fsx::write_atomic(path, bytes).map_err(|source| {
        RemoteRepr::Cache {
            action: "writing",
            path: path.to_path_buf(),
            source,
        }
        .into()
    })
}

/// Whether a `--hook` check is due: no stamp yet, or the stamp is at least
/// [`HOOK_THROTTLE`] old.
///
/// A stamp in the future (a clock that jumped back) reads as due rather than
/// locking the check out until the clock catches up.
#[must_use = "the answer decides whether the hook does any network work"]
pub fn hook_check_due(last_check: Option<i64>, now_epoch: i64) -> bool {
    let Some(last) = last_check else {
        return true;
    };
    let throttle = i64::try_from(HOOK_THROTTLE.as_secs()).unwrap_or(i64::MAX);
    let elapsed = now_epoch.saturating_sub(last);
    elapsed >= throttle || elapsed < 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    /// A validated name, for the settle fixtures.
    fn name(spelling: &str) -> RemoteName {
        RemoteName::parse(spelling).expect("a legal remote name")
    }

    fn state() -> (TempDir, Utf8PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path())
            .expect("utf8 temp path")
            .to_owned();
        (temp, dir)
    }

    #[test]
    fn a_written_notice_reads_back_and_clearing_removes_the_file() {
        let (_keep, dir) = state();
        write_notice(&dir, Some("patina: hello\n")).expect("write the notice");
        assert_eq!(read_notice(&dir).as_deref(), Some("patina: hello"));
        write_notice(&dir, None).expect("clear the notice");
        assert_eq!(read_notice(&dir), None);
        assert!(
            !cache::notice_path(&dir).exists(),
            "clearing must remove the file so `test -s` reads false"
        );
    }

    #[test]
    fn clearing_an_absent_notice_is_a_no_op() {
        let (_keep, dir) = state();
        write_notice(&dir, None).expect("clearing when nothing is pending is fine");
    }

    #[test]
    fn the_pending_message_names_every_behind_remote() {
        let message = pending_updates_message(&["humanizer", "prompts"]);
        assert!(message.contains("humanizer") && message.contains("prompts"));
        assert!(
            message.contains("patina apply --update"),
            "the pending-updates notice must suggest the update command: {message}"
        );
    }

    #[test]
    fn the_pending_message_agrees_in_number() {
        assert!(pending_updates_message(&["one"]).contains("remote with"));
        assert!(pending_updates_message(&["one", "two"]).contains("remotes with"));
    }

    #[test]
    fn the_repo_behind_message_suggests_pulling_instead() {
        let message = repo_behind_message();
        assert!(
            message.contains("git pull") && !message.contains("--update"),
            "a behind repository must be told to pull, not to re-run the gate: {message}"
        );
    }

    #[test]
    fn the_pending_set_round_trips_and_an_empty_list_clears_it() {
        // `remote list` and `patina status` read this file to report per-remote
        // pending state, so it has to survive the round trip in the order the
        // caller supplied and vanish when nothing is pending.
        let (_keep, dir) = state();
        assert!(read_pending(&dir).is_empty(), "nothing pending initially");

        write_pending(&dir, &["humanizer".to_owned(), "prompts".to_owned()])
            .expect("record the pending set");
        let pending = read_pending(&dir);
        assert!(pending.contains("humanizer") && pending.contains("prompts"));
        assert_eq!(pending.len(), 2);

        write_pending(&dir, &[]).expect("clear the pending set");
        assert!(read_pending(&dir).is_empty());
        assert!(
            !cache::pending_path(&dir).exists(),
            "an empty set must remove the file rather than leave a blank one"
        );
    }

    #[test]
    fn clearing_an_absent_pending_set_is_a_no_op() {
        let (_keep, dir) = state();
        write_pending(&dir, &[]).expect("clearing when nothing is pending is fine");
    }

    #[test]
    fn settling_one_remote_rewrites_the_notice_and_settling_the_last_clears_it() {
        let (_keep, dir) = state();
        write_pending(&dir, &["humanizer".to_owned(), "prompts".to_owned()])
            .expect("record the pending set");
        write_notice(
            &dir,
            Some(&pending_updates_message(&["humanizer", "prompts"])),
        )
        .expect("write the notice");

        settle(&dir, &[&name("humanizer")]).expect("settle one remote");
        assert!(!read_pending(&dir).contains("humanizer"));
        let body = read_notice(&dir).expect("a notice remains for the other remote");
        assert!(
            body.contains("prompts") && !body.contains("humanizer"),
            "the notice must be rewritten to the remaining set: {body}"
        );

        settle(&dir, &[&name("prompts")]).expect("settle the last remote");
        assert!(read_pending(&dir).is_empty());
        assert_eq!(
            read_notice(&dir),
            None,
            "settling the last pending remote must clear the notice"
        );
    }

    #[test]
    fn the_pending_state_answers_to_a_name_respelled_in_case() {
        // The file was written by an earlier `remote check` from whatever
        // spelling the declaration carried then. A respelling must not strand
        // an announcement that outlives the update it asked for.
        let (_keep, dir) = state();
        write_pending(&dir, &["Humanizer".to_owned()]).expect("record the pending set");
        let respelled = name("humanizer");

        assert!(
            is_pending(&read_pending(&dir), &respelled),
            "a folded membership test must hit"
        );
        settle(&dir, &[&respelled]).expect("settle the respelled remote");
        assert!(
            read_pending(&dir).is_empty(),
            "settling under a respelling must clear the entry, not leave it behind"
        );
    }

    #[test]
    fn settling_a_remote_that_was_never_pending_touches_nothing() {
        let (_keep, dir) = state();
        write_pending(&dir, &["humanizer".to_owned()]).expect("record the pending set");
        settle(&dir, &[&name("prompts")]).expect("a no-op settle succeeds");
        assert!(
            read_pending(&dir).contains("humanizer"),
            "an unrelated settle must leave the pending set alone"
        );
    }

    #[test]
    fn a_stamp_round_trips() {
        let (_keep, dir) = state();
        assert_eq!(last_check_epoch(&dir), None);
        record_check(&dir, 1_800_000_000).expect("stamp the check");
        assert_eq!(last_check_epoch(&dir), Some(1_800_000_000));
    }

    #[test]
    fn the_hook_throttle_admits_a_first_check_then_holds_for_one_window() {
        // The window is derived from `HOOK_THROTTLE` rather than re-typed, so the
        // assertion is about the boundary behaviour, not about two copies of the
        // same number agreeing.
        let window = i64::try_from(HOOK_THROTTLE.as_secs()).expect("the throttle fits in i64");
        let now = 1_800_000_000;
        assert!(hook_check_due(None, now), "no stamp means a check is due");
        assert!(
            !hook_check_due(Some(now - window + 1), now),
            "one second short of the window must still be throttled"
        );
        assert!(
            hook_check_due(Some(now - window), now),
            "exactly one window later a check is due again"
        );
    }

    #[test]
    fn a_stamp_in_the_future_does_not_lock_the_check_out() {
        let now = 1_800_000_000;
        assert!(
            hook_check_due(Some(now + 7 * 24 * 60 * 60), now),
            "a clock that jumped back must not disable the check for a week"
        );
    }
}
