//! The notify-only pending-update notice and the background-check throttle.
//!
//! `<state>/remotes/notice` holds plain text a shell startup can print with
//! builtins alone, with no `patina` process on the prompt path. It
//! distinguishes two situations:
//!
//! - upstream tips have moved past your pins, so `patina apply --update` is the
//!   next step;
//! - your own dotfiles repository is behind its origin (another machine already
//!   bumped the pins), so `git pull && patina apply` is, since those changes
//!   are already decided and gated.
//!
//! `<state>/remotes/last_check` stamps the last real check so
//! `patina remote check --hook` can self-throttle to at most one per day.
//!
//! See `docs/REMOTE_SOURCES.md` "Shell integration".

use super::RemoteError;
use super::RemoteRepr;
use super::cache;
use camino::Utf8Path;
use camino::Utf8PathBuf;
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
        None => remove_if_present(&path),
    }
}

/// The current notice, or `None` when there is nothing pending.
///
/// An unreadable or empty notice reads as `None`: this is a notification, and
/// failing a command over it would be worse than staying quiet.
#[must_use = "the notice is what `patina status` and the shell snippets surface"]
pub fn read_notice(state_dir: &Utf8Path) -> Option<String> {
    let text = fs_err::read_to_string(cache::notice_path(state_dir).as_std_path()).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The notice for remotes whose upstream tip has moved past the pin.
///
/// `modules` is expected in a stable order (the caller iterates the lockfile,
/// which is name-ordered), so the file's bytes are a function of which remotes
/// are behind and not of iteration order.
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
/// The `notice` file is prose for a human to read at a shell prompt; this is
/// the same fact in a form `patina remote list` and `patina status` can report
/// per-remote without parsing English. An empty list removes the file.
///
/// # Errors
///
/// Returns a [`RemoteError`] when the write or removal fails.
pub fn write_pending(state_dir: &Utf8Path, modules: &[String]) -> Result<(), RemoteError> {
    let path = cache::pending_path(state_dir);
    if modules.is_empty() {
        return remove_if_present(&path);
    }
    let mut body = modules.join("\n");
    body.push('\n');
    atomic_write(&path, body.as_bytes())
}

/// The remotes the last check found behind their upstream.
///
/// An absent or unreadable file reads as "none pending": this is notification
/// state, and a stale read is better than a failed command.
#[must_use = "the set is the per-remote pending state `remote list` reports"]
pub fn read_pending(state_dir: &Utf8Path) -> std::collections::BTreeSet<String> {
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

/// Create `path`'s parent directory when it is missing.
fn ensure_parent(path: &Utf8Path) -> Result<(), RemoteError> {
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent.as_std_path()).map_err(|source| RemoteRepr::Cache {
            action: "creating",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// Remove `path`, treating an already-absent file as success.
fn remove_if_present(path: &Utf8Path) -> Result<(), RemoteError> {
    match fs_err::remove_file(path.as_std_path()) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RemoteRepr::Cache {
            action: "removing",
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

/// Write `bytes` to `path` through a per-process temporary and an atomic
/// rename.
///
/// The shell integration can spawn a `patina remote check` in every session,
/// so several may write these files concurrently. A rename swaps the file in
/// whole, so a concurrent reader (the shell's `test -s`, `patina status`) sees
/// either the old file or the new one, never a half-written line. The temporary
/// carries the writer's pid, so two writers never collide on one scratch name.
fn atomic_write(path: &Utf8Path, bytes: &[u8]) -> Result<(), RemoteError> {
    ensure_parent(path)?;
    let tmp = Utf8PathBuf::from(format!("{path}.{}.tmp", std::process::id()));
    fs_err::write(tmp.as_std_path(), bytes).map_err(|source| RemoteRepr::Cache {
        action: "writing",
        path: tmp.clone(),
        source,
    })?;
    fs_err::rename(tmp.as_std_path(), path.as_std_path()).map_err(|source| {
        RemoteRepr::Cache {
            action: "renaming into",
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
