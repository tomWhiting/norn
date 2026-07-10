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
//!
//! A deadline-bound wait polls the non-blocking [`File::try_lock`] and
//! sleeps [`LOCK_POLL_INTERVAL`] between attempts rather than parking a
//! thread in the blocking `File::lock`, so a timed-out acquisition leaves
//! nothing behind — no waiter thread and no open file descriptor blocked
//! in `flock` until the contending holder happens to release.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::permissions::{create_private_dir_all, open_private_lock};
use super::types::SessionPersistError;

/// File name of the index lock inside the session data directory.
const INDEX_LOCK_FILE: &str = "index.lock";

/// How long a deadline-bound acquisition sleeps between non-blocking
/// [`File::try_lock`] attempts.
///
/// This is an internal mechanism detail of the timeout implementation,
/// not a configurable knob or an assumed default for the deadline itself
/// (the deadline is always supplied by the caller). It bounds two things
/// to a single interval: how quickly a freed lock is noticed after the
/// holder releases, and how far past the caller's deadline a timeout can
/// be reported. `5ms` keeps both latencies imperceptible next to the
/// step-level or human-facing deadlines that motivate the feature while
/// costing at most ~200 wakeups/second on a single contended waiter —
/// negligible, and only while the lock is actually contended.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(5);

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
/// its own). With `Some(deadline)`, the wait polls a non-blocking
/// [`File::try_lock`] until it succeeds or the deadline elapses; on
/// expiry the call returns [`SessionPersistError::IndexLockTimeout`] with
/// the index untouched and no waiter thread or blocked descriptor left
/// behind.
pub(crate) fn lock_index(
    data_dir: &Path,
    deadline: Option<Duration>,
) -> Result<IndexLock, SessionPersistError> {
    create_private_dir_all(data_dir)?;
    let path = data_dir.join(INDEX_LOCK_FILE);
    let file = open_private_lock(&path)?;
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
/// `File::lock` has no timeout, so instead of parking a thread in it this
/// polls the non-blocking [`File::try_lock`] on the current thread,
/// sleeping [`LOCK_POLL_INTERVAL`] (clamped so it never overshoots the
/// deadline) between attempts. On expiry the `file` handle is dropped
/// with its descriptor — there is no waiter thread and no blocked `flock`
/// to leak, so a workflow that repeatedly times out behind a wedged
/// holder accumulates nothing.
fn lock_with_deadline(
    file: File,
    path: PathBuf,
    deadline: Duration,
) -> Result<IndexLock, SessionPersistError> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(IndexLock { file }),
            Err(TryLockError::WouldBlock) => {
                let elapsed = started.elapsed();
                let Some(remaining) = deadline.checked_sub(elapsed) else {
                    return Err(SessionPersistError::IndexLockTimeout {
                        path,
                        waited: deadline,
                    });
                };
                std::thread::sleep(remaining.min(LOCK_POLL_INTERVAL));
            }
            Err(TryLockError::Error(error)) => return Err(SessionPersistError::Io(error)),
        }
    }
}
