Vet sub-agents append to `journal/VET.md` during their own runs, before
the caller decides whether to keep or revert code changes. A rollback must
therefore undo code edits without erasing the VET audit trail.

Use a stash only as a code snapshot. Do not `git stash pop`: an untracked
or dirty VET journal can be restored from the stale stash copy and clobber
blocks appended after the snapshot. Restore code from `stash@{0}` with a
journal-excluding checkout, clean added code files with the same exclusion,
then drop the snapshot.
