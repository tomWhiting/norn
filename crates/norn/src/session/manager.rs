//! One front door for the session lifecycle: [`SessionManager`].
//!
//! Every way a persisted session comes to life — create, resume, fork,
//! idempotent open-or-resume — goes through the manager and comes back
//! as an [`OpenSession`]: an [`EventStore`] with a write-through,
//! index-registered JSONL sink already installed, the session's
//! [`SessionIndexEntry`], and a [`ReplaySummary`] describing what was
//! recovered from disk. Callers never hand-assemble the
//! create-then-attach or resume-then-attach sequences the pre-Phase-2
//! free functions required.
//!
//! The manager composes the persistence engine, it does not replace it:
//! strict format-2 replay ([`read_session_events`]), retry-safe appends, batched index
//! maintenance with `EventStore::checkpoint`, and resume-time index
//! self-healing are all the Phase 1 contracts, unchanged.
//!
//! # Durability
//!
//! Every opening method takes the [`DurabilityPolicy`] explicitly — the
//! policy is the embedder's call per open, and there is no built-in
//! default.
//!
//! # Idempotency (`open_or_resume`)
//!
//! [`SessionManager::open_or_resume`] is the primitive for retry-safe
//! embedders (e.g. a durable-workflow activity that may run the same
//! logical step several times): the caller supplies the session ID — a
//! deterministic key derived from the unit of work, not a fresh UUID —
//! and the manager either creates the session under exactly that ID or
//! resumes the one a previous attempt left behind. The
//! check-and-insert runs under the inter-process index lock, so two
//! processes racing the same key converge on a single session instead
//! of minting duplicates. A retry therefore continues its predecessor's
//! event history rather than polluting the index with one session per
//! attempt.

mod fork;
mod open;
mod resume_policy;
mod standard;

pub use resume_policy::ResumePolicy;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::provider::ProviderStateIdentity;
use crate::session::store::EventStore;

use super::persistence::index::{
    delete_session_transaction, read_index_with_deadline, resolve_session_with_deadline,
    revalidate_registered_entry, update_registered_entry,
};
use super::persistence::read_session_events_for_entry_with_deadline;
use super::persistence::replay::ReplayArtifacts;
use super::persistence::types::{SessionIndexEntry, SessionPersistError};

/// Caller-supplied metadata recorded in the index entry of a newly
/// created session (also the create arm of
/// [`SessionManager::open_or_resume`] and the new entry minted by
/// [`SessionManager::fork`]).
#[derive(Clone, Debug)]
pub struct CreateSessionOptions {
    /// Model identifier active when the session is created.
    pub model: String,
    /// Working directory the session runs in. The manager records what
    /// it is given — deriving it (e.g. from the process CWD) is the
    /// caller's decision, never an assumed default.
    pub working_dir: String,
    /// Optional human-readable name, resolvable via
    /// [`SessionManager::resolve`].
    pub name: Option<String>,
}

/// What was recovered from disk while opening a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaySummary {
    /// Events recovered from the session file and pre-loaded into the
    /// returned store. `0` for a freshly created session. For a fork
    /// this counts the copied source events plus the appended `Fork`
    /// marker.
    pub replayed_events: usize,
}

/// A session opened through [`SessionManager`]: the ready-to-append
/// store, its index entry, and the replay summary.
#[derive(Debug)]
pub struct OpenSession {
    /// Event store with a write-through, index-registered JSONL sink
    /// installed: every append persists to `{data_dir}/{id}.jsonl` and
    /// keeps the index entry (event count, usage totals, `updated_at`)
    /// in step per the chosen [`DurabilityPolicy`]. Call
    /// [`EventStore::checkpoint`] at turn boundaries to flush deferred
    /// index deltas; resume self-heals anything a crash leaves stale.
    pub store: EventStore,
    /// The session's index entry. On resume this carries the
    /// recomputed (self-healed) event count and usage totals, never
    /// stale values.
    pub entry: SessionIndexEntry,
    /// What was recovered from disk to populate [`Self::store`].
    pub replay: ReplaySummary,
}

/// One coherent owner of a session data directory.
///
/// Construct with [`Self::standard`] for the checked user session store, or
/// [`Self::new`] with an **explicit** directory owned by the embedder. Cheap
/// to clone; holds no open handles itself (each
/// [`OpenSession`]'s sink owns its own file handle).
///
/// # Keep sessions open across steps
///
/// Every open replays the session's **entire** event history from disk
/// (one single-pass traversal producing [`ReplayArtifacts`]). An
/// embedder that re-opens the same session once per workflow step
/// therefore pays O(history) per step — quadratic over the workflow's
/// life. The intended shape is: open once ([`Self::resume`] /
/// [`Self::open_or_resume`]), hold the returned [`OpenSession`] (its
/// [`EventStore`] appends write-through), call
/// [`EventStore::checkpoint`] at step boundaries to flush deferred
/// index deltas, and drop the store only when the workflow is done.
/// Re-opening is for a new process (or after a crash), not for the next
/// step of a live one.
///
/// # Blocking I/O and async executors
///
/// Every method on this type performs blocking file I/O. Index reads take the
/// inter-process session-index lock because they may converge an interrupted
/// publication; mutations hold the same lock across a full index
/// read+rewrite+fsync. Both waits are unbounded unless
/// [`Self::with_index_lock_deadline`] sets a deadline. Callers on an async
/// executor wrap these calls in
/// [`tokio::task::spawn_blocking`] — an open runs once per workflow,
/// so the offload costs nothing recurring — and use
/// [`EventStore::checkpoint_off_executor`] for the per-step index
/// flush, which performs the same offload internally.
#[derive(Clone, Debug)]
pub struct SessionManager {
    data_dir: PathBuf,
    index_lock_deadline: Option<Duration>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum AffinityMode {
    StorageOnly,
    Validate(Option<ProviderStateIdentity>),
}

/// Provider-aware view over one [`SessionManager`].
///
/// This narrow facade keeps credential affinity out of generic creation
/// metadata while ensuring every lifecycle arm validates before returning a
/// managed store or publishing a fork.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionAffinityRequest<'a> {
    pub(super) manager: &'a SessionManager,
    pub(super) identity: Option<ProviderStateIdentity>,
}

impl SessionManager {
    /// Create a manager over `data_dir`. The directory (and the index
    /// inside it) is created lazily on first write. Index-lock waits
    /// are unbounded until [`Self::with_index_lock_deadline`] sets a
    /// deadline.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            index_lock_deadline: None,
        }
    }

    /// Set the acquisition deadline this manager (and every sink it
    /// opens) applies when taking the inter-process session-index lock.
    ///
    /// `None` — the constructor's value — waits indefinitely, exactly
    /// the OS lock primitive's own behaviour. `Some(deadline)` bounds
    /// the wait: an operation that cannot take the lock within the
    /// deadline fails with [`SessionPersistError::IndexLockTimeout`]
    /// instead of stalling behind a wedged lock holder, with the index
    /// untouched.
    #[must_use]
    pub fn with_index_lock_deadline(mut self, deadline: Option<Duration>) -> Self {
        self.index_lock_deadline = deadline;
        self
    }

    /// The data directory this manager owns.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Open sessions through a provider-state affinity check.
    ///
    /// `None` may create or use an unbound session, but cannot reopen a row
    /// already bound to an identity.
    pub(crate) fn open_with_affinity(
        &self,
        identity: Option<ProviderStateIdentity>,
    ) -> SessionAffinityRequest<'_> {
        SessionAffinityRequest {
            manager: self,
            identity,
        }
    }

    /// The configured inter-process index-lock acquisition deadline
    /// (`None` = wait indefinitely). Exposed so the child-branching
    /// authority ([`crate::session::SessionBinding`]) applies the same
    /// bound to the index operations it performs on this manager's data
    /// directory.
    #[must_use]
    pub fn index_lock_deadline(&self) -> Option<Duration> {
        self.index_lock_deadline
    }

    /// List every session in the strictly validated format-2 index, in file
    /// order.
    ///
    /// # Errors
    ///
    /// Fails when index recovery or reading fails, including a configured
    /// [`SessionPersistError::IndexLockTimeout`].
    pub fn list(&self) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
        read_index_with_deadline(&self.data_dir, self.index_lock_deadline)
    }

    /// Resolve `id_or_name` (empty = most recently updated, exact ID,
    /// exact name, or unique >= 8-character ID prefix) to its index
    /// entry without opening the session.
    ///
    /// # Errors
    ///
    /// [`SessionPersistError::NotFound`] /
    /// [`SessionPersistError::AmbiguousPrefix`], index recovery errors, or a
    /// configured [`SessionPersistError::IndexLockTimeout`].
    pub fn resolve(&self, id_or_name: &str) -> Result<SessionIndexEntry, SessionPersistError> {
        resolve_session_with_deadline(&self.data_dir, id_or_name, self.index_lock_deadline)
    }

    /// Read a session's events without opening it for appending (the export /
    /// inspection path): resolve `id_or_name`, then strictly read the format-2
    /// event file in one pass. Returns the resolved entry and derived replay
    /// artifacts without re-walking the history.
    ///
    /// # Errors
    ///
    /// Resolution, I/O, or strict format errors. Active format-2 histories
    /// fail closed on invalid records; only a validated torn final record may
    /// be repaired by the timeline transaction before this read completes.
    pub fn read_events(
        &self,
        id_or_name: &str,
    ) -> Result<(SessionIndexEntry, ReplayArtifacts), SessionPersistError> {
        let resolved =
            resolve_session_with_deadline(&self.data_dir, id_or_name, self.index_lock_deadline)?;
        let artifacts = read_session_events_for_entry_with_deadline(
            &self.data_dir,
            &resolved,
            self.index_lock_deadline,
        )?;
        let entry =
            revalidate_registered_entry(&self.data_dir, &resolved, self.index_lock_deadline)?;
        Ok((entry, artifacts))
    }

    /// Rename a session: resolve `id_or_name`, then set (or clear with
    /// `None`) the human-readable name on its index entry under the
    /// inter-process lock. Returns the updated entry.
    ///
    /// # Errors
    ///
    /// Resolution errors and index-rewrite failures.
    pub fn rename(
        &self,
        id_or_name: &str,
        name: Option<String>,
    ) -> Result<SessionIndexEntry, SessionPersistError> {
        let entry =
            resolve_session_with_deadline(&self.data_dir, id_or_name, self.index_lock_deadline)?;
        self.rename_entry(&entry, name)
    }

    fn rename_entry(
        &self,
        entry: &SessionIndexEntry,
        name: Option<String>,
    ) -> Result<SessionIndexEntry, SessionPersistError> {
        update_registered_entry(&self.data_dir, entry, self.index_lock_deadline, move |e| {
            e.name = name;
        })
    }

    /// Delete a session subtree. Under the inter-process index lock this
    /// resolves `id_or_name`, durably journals the exact generations being
    /// removed, and atomically publishes the index without those rows before
    /// reclaiming timelines and root-owned artifacts. Returns the selected
    /// entry after both logical deletion and physical cleanup complete.
    ///
    /// Deleting either a root or child includes every transitive descendant.
    /// Deleting a root also removes its `{id}/` sibling directory (the
    /// `children/` timelines, fetched artifacts, and `spool/` payloads minted
    /// under it). A durable journal makes interrupted cleanup retryable on the
    /// next index operation without making the logically deleted rows visible
    /// again.
    ///
    /// Timeline cleanup is descriptor-bounded but has no implicit lock
    /// timeout: after index publication this call waits for each current
    /// timeline owner before unlinking that timeline. The index-lock deadline
    /// configures index acquisition, not these already-committed cleanup
    /// waits.
    ///
    /// **Spool caveat:** [`Self::fork`] copies the source's events —
    /// including `ToolResult.spool_ref` values, which stay anchored at
    /// the SOURCE root's `{id}/spool/` directory — without copying the
    /// spool payloads. Deleting the source root therefore orphans those
    /// references in the fork: resolving one afterwards degrades TYPED
    /// ([`read_spooled_output`](crate::session::spool::read_spooled_output)
    /// returns the missing-file persist error, never a panic), and the
    /// fork's inline bounded projection remains intact.
    ///
    /// # Errors
    ///
    /// Resolution, journal, or index failures before the atomic rename leave
    /// the selected subtree logically present. If the rename succeeds but its
    /// directory sync fails, [`SessionPersistError::IndexCommitIndeterminate`]
    /// reports that the visible outcome is not yet crash-durable and the
    /// deletion journal remains for recovery. Once durable index publication
    /// succeeds, a cleanup failure returns
    /// [`SessionPersistError::DeletionCleanupPending`]: the subtree is already
    /// logically absent, its durable journal remains, and a later recovered
    /// index operation retries physical cleanup.
    pub fn delete(&self, id_or_name: &str) -> Result<SessionIndexEntry, SessionPersistError> {
        delete_session_transaction(&self.data_dir, id_or_name, self.index_lock_deadline)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "manager/tests.rs"]
mod tests;
