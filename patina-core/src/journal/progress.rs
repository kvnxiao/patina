//! The per-operation progress cursor (REQ-012).
//!
//! As the executor completes each operation it appends one record to
//! `<state>/patina/journal/<ts>.progress`. The cursor is advisory: it is
//! written through to the kernel page cache but is **never** `fsync`-ed
//! per operation. After a crash the last record may lag the real
//! filesystem state by at most one operation, which is why crash
//! recovery (T-011) probes the filesystem rather than trusting the
//! cursor. Skipping the per-op `fsync` is what keeps a 100-operation
//! apply from paying 100 durability syscalls (REQ-012 `<behavior>`).
//!
//! Each record is a fixed-width little-endian `u32` operation index
//! followed by a single completion-marker byte. The fixed width makes a
//! partially-written trailing record (a torn tail after a crash) trivial
//! to detect during recovery: any tail shorter than [`RECORD_LEN`] is
//! discarded.

use super::JournalError;
use super::PROGRESS_SUFFIX;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::io::Write as _;

/// Completion marker byte written after each operation index. A value of
/// [`COMPLETED_MARKER`] means "operation fully completed"; recovery
/// treats any other trailing byte (or a short tail) as not-completed.
const COMPLETED_MARKER: u8 = 1;

/// Width of one progress record: a little-endian `u32` index plus the
/// one-byte completion marker.
pub const RECORD_LEN: usize = core::mem::size_of::<u32>() + 1;

/// Append-only writer for one apply run's progress cursor. Holds the
/// open file handle so each [`record`](ProgressCursor::record) is a bare
/// append with no re-open and, crucially, no `fsync`.
#[derive(Debug)]
pub struct ProgressCursor {
    path: Utf8PathBuf,
    file: fs_err::File,
}

impl ProgressCursor {
    /// Create (truncating any stale file) the progress cursor for
    /// `<dir>/<timestamp>.progress`.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Filesystem`] if the file cannot be created.
    pub fn create(dir: &Utf8Path, timestamp: &str) -> Result<Self, JournalError> {
        let path = dir.join(format!("{timestamp}{PROGRESS_SUFFIX}"));
        let file = fs_err::File::create(&path)?;
        Ok(Self { path, file })
    }

    /// Append a completion record for `op_index`. Deliberately not
    /// `fsync`-ed (REQ-012).
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Filesystem`] if the append fails.
    pub fn record(&mut self, op_index: u32) -> Result<(), JournalError> {
        let [b0, b1, b2, b3] = op_index.to_le_bytes();
        let record: [u8; RECORD_LEN] = [b0, b1, b2, b3, COMPLETED_MARKER];
        self.file.write_all(&record)?;
        Ok(())
    }

    /// The path of this progress cursor.
    #[must_use = "the path locates the progress cursor for recovery"]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Decode the completed operation indices recorded in `bytes`,
    /// discarding any torn trailing record shorter than `RECORD_LEN`.
    /// Used by crash recovery (T-011) to read back a cursor that was
    /// never `fsync`-ed.
    #[must_use = "the decoded indices drive recovery reconciliation"]
    pub fn decode_completed(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(RECORD_LEN)
            // `chunks_exact(RECORD_LEN=5)` only ever yields 5-byte slices, so
            // this slice pattern is irrefutable — mirroring the
            // `let [b0, b1, b2, b3] = ...` idiom in `record` above. The
            // trailing partial chunk (a torn record) is left in
            // `chunks_exact`'s remainder and never produced here.
            .filter_map(|record| match record {
                [b0, b1, b2, b3, COMPLETED_MARKER] => {
                    Some(u32::from_le_bytes([*b0, *b1, *b2, *b3]))
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn encoded(indices: &[u32]) -> Vec<u8> {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8Path::from_path(temp.path()).expect("utf8 temp path");
        let mut cursor = ProgressCursor::create(dir, "ts").expect("create cursor");
        for &i in indices {
            cursor.record(i).expect("record");
        }
        drop(cursor);
        fs_err::read(dir.join("ts.progress")).expect("read progress")
    }

    #[test]
    fn records_decode_in_append_order() {
        let bytes = encoded(&[0, 1, 2]);
        assert_eq!(ProgressCursor::decode_completed(&bytes), vec![0, 1, 2]);
    }

    #[test]
    fn each_record_is_fixed_width() {
        let bytes = encoded(&[7, 42]);
        assert_eq!(bytes.len(), 2 * RECORD_LEN);
    }

    #[test]
    fn torn_trailing_record_is_discarded() {
        let mut bytes = encoded(&[3, 9]);
        // Simulate a crash mid-write of a third record: append a partial
        // tail shorter than one record.
        bytes.extend_from_slice(&[0xFF, 0xFF]);
        assert_eq!(
            ProgressCursor::decode_completed(&bytes),
            vec![3, 9],
            "the torn tail is ignored; only whole records count"
        );
    }
}
