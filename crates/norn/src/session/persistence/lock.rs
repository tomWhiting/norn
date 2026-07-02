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
//!
//! Acquisition waits indefinitely by default (the OS lock primitive has
//! no timeout). Callers that must bound the wait — e.g. an embedder
//! that would rather fail a step than stall behind a wedged process —
//! pass an explicit deadline; exceeding it yields the typed
//! [`SessionPersistError::IndexLockTimeout`].

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

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

/// Acquire the exclusive index lock for `data_dir`. Creates the data
/// directory and the lock file on first use.
///
/// With `deadline = None` the call blocks until the lock is available
/// (the historical behaviour — the OS advisory lock has no timeout of
/// its own). With `Some(deadline)`, a wait exceeding the deadline
/// returns [`SessionPersistError::IndexLockTimeout`] and leaves the
/// index untouched; a lock acquired by the abandoned waiter after the
/// timeout is released immediately, so a timed-out call never holds the
/// lock.
pub(crate) fn lock_index(
    data_dir: &Path,
    deadline: Option<Duration>,
) -> Result<IndexLock, SessionPersistError> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(INDEX_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match deadline {
        None => {
            file.lock()?;
            Ok(IndexLock { file })
        }
        Some(deadline) => lock_with_deadline(file, path, deadline),
    }
}

/// Bound the indefinite OS lock wait with `deadline`.
///
/// `File::lock` has no timeout, so the blocking wait runs on a
/// dedicated waiter thread and this caller waits on a channel with the
/// deadline. On timeout the receiver is dropped; if the abandoned
/// waiter later acquires the lock, its `send` fails and it releases the
/// lock immediately (unlock, then handle close), so a timed-out
/// acquisition can never leak a held lock.
fn lock_with_deadline(
    file: File,
    path: PathBuf,
    deadline: Duration,
) -> Result<IndexLock, SessionPersistError> {
    let (sender, receiver) = mpsc::channel::<std::io::Result<File>>();
    let waiter_path = path.clone();
    std::thread::Builder::new()
        .name("norn-index-lock-wait".to_owned())
        .spawn(move || {
            let result = file.lock().map(|()| file);
            if let Err(mpsc::SendError(unclaimed)) = sender.send(result) {
                // The waiter timed out and dropped the receiver.
                match unclaimed {
                    Ok(locked) => {
                        if let Err(error) = locked.unlock() {
                            // Dropping the handle below releases the OS
                            // lock regardless; log for observability.
                            tracing::warn!(
                                path = %waiter_path.display(),
                                %error,
                                "failed to unlock session index lock acquired \
                                 after its waiter timed out; the handle close \
                                 releases it",
                            );
                        }
                        tracing::debug!(
                            path = %waiter_path.display(),
                            "session index lock acquired after its waiter \
                             timed out; released immediately",
                        );
                    }
                    Err(error) => {
                        tracing::debug!(
                            path = %waiter_path.display(),
                            %error,
                            "session index lock wait failed after its waiter \
                             timed out",
                        );
                    }
                }
            }
        })?;
    match receiver.recv_timeout(deadline) {
        Ok(Ok(file)) => Ok(IndexLock { file }),
        Ok(Err(error)) => Err(SessionPersistError::Io(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(SessionPersistError::IndexLockTimeout {
            path,
            waited: deadline,
        }),
        // The waiter thread can only exit by sending; a disconnect
        // without a value means it terminated abnormally.
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SessionPersistError::Io(
            std::io::Error::other("session index lock waiter thread exited without a result"),
        )),
    }
}
