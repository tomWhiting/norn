//! Advisory inter-process locking for the session index (H18).
//!
//! Every mutation of `index.jsonl` — the `O_APPEND` create path and the
//! read-modify-rewrite update/remove path — must hold the exclusive
//! advisory lock on `{data_dir}/index.lock` for its whole critical
//! section. Without it, a concurrent create from another process can be
//! permanently dropped when a read-modify-rewrite renames a stale
//! snapshot over the index, making that session unlistable and
//! unresumable.
//!
//! The lock is a separate file (not the index itself) so the atomic
//! rename-over of `index.jsonl` never replaces the inode the lock is
//! held on. Locking uses `std::fs::File::lock` (OS advisory locking —
//! `flock` on Unix), which excludes both other processes and other
//! threads of this process, because each acquisition opens its own file
//! description.

use std::fs::{self, File, OpenOptions};
use std::path::Path;

use super::types::SessionPersistError;

/// File name of the index lock inside the session data directory.
const INDEX_LOCK_FILE: &str = "index.lock";

/// An exclusive advisory lock over the session index.
///
/// Held for the duration of one index mutation (append or
/// read-modify-rewrite). Released on drop; an unlock failure is logged
/// (the OS also releases the lock when the file handle closes).
#[derive(Debug)]
pub(crate) struct IndexLock {
    file: File,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            // Closing the handle releases the OS lock regardless, so this
            // is observability only — never a correctness hole.
            tracing::warn!(%error, "failed to explicitly unlock session index lock");
        }
    }
}

/// Acquire the exclusive index lock for `data_dir`, blocking until it is
/// available. Creates the data directory and the lock file on first use.
pub(crate) fn lock_index(data_dir: &Path) -> Result<IndexLock, SessionPersistError> {
    fs::create_dir_all(data_dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join(INDEX_LOCK_FILE))?;
    file.lock()?;
    Ok(IndexLock { file })
}
