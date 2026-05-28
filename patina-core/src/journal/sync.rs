//! The durability-syscall abstraction behind the plan journal.
//!
//! [`Syncer`] names the two `fsync`-family operations the journal needs:
//! flushing a file's contents to stable storage and flushing a
//! directory's entries. Production code uses [`OsSyncer`], which calls
//! through to `File::sync_all`. The crash-recovery suite (T-011)
//! substitutes a recording fake that counts calls per path so it can
//! assert the REQ-012 invariant: exactly one fsync each on the plan
//! file, the journal directory, and the commit sentinel, and zero on the
//! progress cursor.

use camino::Utf8Path;

/// Abstraction over the journal's durability syscalls so tests can count
/// `fsync` calls without touching real hardware (REQ-012 `<behavior>`).
///
/// Implementors must make a returned `Ok(())` mean the bytes (or
/// directory entries) are durable on stable storage.
pub trait Syncer {
    /// `fsync` the file at `path`, flushing its contents to stable
    /// storage.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the file cannot be
    /// opened or the sync fails.
    fn sync_file(&self, path: &Utf8Path) -> Result<(), std::io::Error>;

    /// `fsync` the directory at `path`, flushing its entries (the
    /// name-to-inode bindings created by a preceding write) to stable
    /// storage.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the directory cannot
    /// be opened or the sync fails.
    fn sync_dir(&self, path: &Utf8Path) -> Result<(), std::io::Error>;
}

/// The production [`Syncer`]: issues real `fsync` calls via
/// [`std::fs::File::sync_all`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OsSyncer;

impl Syncer for OsSyncer {
    fn sync_file(&self, path: &Utf8Path) -> Result<(), std::io::Error> {
        // Open for write (without truncating) rather than read-only:
        // Windows `FlushFileBuffers` requires a handle with write access
        // (`GENERIC_WRITE`) and rejects a read-only handle with
        // `ERROR_ACCESS_DENIED`. `write(true)` on its own opens an
        // existing file in place without clearing its contents.
        let file = fs_err::OpenOptions::new().write(true).open(path)?;
        file.sync_all()
    }

    fn sync_dir(&self, path: &Utf8Path) -> Result<(), std::io::Error> {
        // Opening a directory read-only and calling `sync_all` flushes
        // its entries on POSIX. On Windows a directory handle cannot be
        // opened with the std file API and `sync_all` is not meaningful
        // for directories, so the directory write is already durable via
        // the file sync; treat a failure-to-open as a no-op there.
        match fs_err::File::open(path) {
            Ok(dir) => match dir.sync_all() {
                Ok(()) => Ok(()),
                // Windows: directory handles reject FlushFileBuffers.
                Err(err) if is_unsupported_dir_sync(&err) => Ok(()),
                Err(err) => Err(err),
            },
            Err(err) if is_unsupported_dir_sync(&err) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// Whether an error from a directory `fsync` reflects a platform that
/// does not support syncing directory handles (Windows), as opposed to a
/// genuine I/O failure that must propagate.
fn is_unsupported_dir_sync(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
    ) || err.raw_os_error() == Some(ACCESS_DENIED_WINDOWS)
}

/// Windows `ERROR_ACCESS_DENIED`. Opening a directory with the std file
/// API surfaces this on Windows.
const ACCESS_DENIED_WINDOWS: i32 = 5;
