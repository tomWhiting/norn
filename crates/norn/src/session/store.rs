//! Append-only event storage with optional write-through persistence.

use std::collections::HashMap;
use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use crate::error::SessionError;
use crate::provider::usage::Usage;
use crate::session::events::{EventId, SessionEvent};
use crate::session::persistence::SessionPersistError;
use crate::session::persistence::index::{sum_usage_from_events, update_session_index};
use crate::session::persistence::io::{open_session_append, open_session_append_for_entry};
use crate::session::spool::SpoolWriter;

/// Append-only, in-memory event store.
///
/// Events can be appended and retrieved but never deleted, modified, or
/// replaced. Uses `parking_lot::RwLock` for `Send + Sync` without poison
/// handling (satisfies CO4).
///
/// When a [`PersistenceSink`] is installed, every appended event is
/// written through to the sink before it becomes visible in memory, so
/// in-memory state never claims more than what was handed to the OS.
/// Disk I/O happens under the sink's own mutex — never under the shared
/// state lock — so readers are not blocked by slow writes.
pub struct EventStore {
    inner: RwLock<StoreInner>,
    sink: Option<Mutex<Box<dyn PersistenceSink>>>,
    spool: Option<SpoolWriter>,
}

impl std::fmt::Debug for EventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStore")
            .field("len", &self.inner.read().events.len())
            .field("has_sink", &self.sink.is_some())
            .field("has_spool", &self.spool.is_some())
            .finish()
    }
}

/// Receives each event as it is appended to the store.
///
/// Implementations must be `Send` so the store remains `Send + Sync`.
/// `persist` is called under the store's sink mutex with no in-memory
/// state lock held; implementations must not call back into
/// [`EventStore::append`] (it would deadlock on the sink mutex) but may
/// freely read the store.
///
/// A persist error is surfaced from [`EventStore::append`] and the event
/// is **not** added to the in-memory store, so memory never claims more
/// durability than disk has. Retrying the same event after a failure is
/// safe even if the failed attempt actually reached the file: the
/// tolerant reader skips duplicate `EventId` lines, keeping the first
/// occurrence.
pub trait PersistenceSink: Send {
    /// Write one event durably according to the sink's policy.
    ///
    /// # Errors
    ///
    /// Returns the underlying persistence failure; the caller decides
    /// whether to retry or abort.
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError>;

    /// Bring any deferred bookkeeping (e.g. a batched session-index
    /// delta) up to date. The default is a no-op for sinks that defer
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns the underlying persistence failure; deferred state must be
    /// retained so a later `checkpoint` (or drop) can retry it.
    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        Ok(())
    }
}

/// Durability level applied by [`JsonlSink`] after each event write.
///
/// The policy also sets the cadence of session-**index** maintenance for
/// an index-registered sink: the index delta (event count, usage totals,
/// `updated_at`) accumulates in memory and is written — one locked
/// read-modify-rewrite of `index.jsonl`, which always fsyncs its tmp
/// file before the atomic rename — only at the points each variant
/// documents, plus at every explicit
/// [`checkpoint`](EventStore::checkpoint) and when the sink is dropped
/// (clean shutdown). A crash before a pending delta lands leaves the
/// index entry stale; the self-maintenance pass in
/// [`SessionManager::resume`](crate::session::SessionManager::resume)
/// recomputes and repairs it from the event file. The batch path
/// ([`append_events`](crate::session::persistence::append_events))
/// fsyncs per batch and updates the index per batch, unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityPolicy {
    /// Hand each event line to the OS (`write(2)`) without ever issuing
    /// `fsync` on the session file. Survives process crashes; an OS
    /// crash or power loss may lose events still in the page cache.
    /// Index deltas are flushed only at `checkpoint`/drop — no index
    /// rewrite or fsync happens per event. This is the historical
    /// behaviour and the default of every existing constructor.
    Flush,
    /// `fsync` the session file after every event, and bring the index
    /// up to date at the same point. Maximum durability, highest
    /// per-event latency (every event pays an index rewrite + fsync on
    /// top of the session-file fsync).
    FsyncPerEvent,
    /// `fsync` after every `n` events and flush the accumulated index
    /// delta at each such boundary. Bounds loss to at most `n - 1`
    /// events on OS crash; `n` comes from the embedder, never from a
    /// built-in default.
    FsyncEveryEvents(NonZeroU64),
}

/// Index registration carried by a [`JsonlSink`]: which index entry to
/// keep in step with persisted events, plus the delta (events persisted
/// and usage accrued since the last successful index write) still
/// waiting to be flushed at the next durability boundary, explicit
/// checkpoint, or drop.
#[derive(Debug)]
struct IndexRegistration {
    data_dir: PathBuf,
    session_id: String,
    pending_events: u64,
    pending_usage: Usage,
    /// Acquisition deadline applied when taking the inter-process index
    /// lock for a delta flush; `None` waits indefinitely.
    lock_deadline: Option<Duration>,
}

impl IndexRegistration {
    /// Fold one freshly persisted event into the pending index delta.
    fn accumulate(&mut self, event: &SessionEvent) {
        self.pending_events = self.pending_events.saturating_add(1);
        let usage = sum_usage_from_events(std::slice::from_ref(event));
        self.pending_usage.input_tokens = self
            .pending_usage
            .input_tokens
            .saturating_add(usage.input_tokens);
        self.pending_usage.output_tokens = self
            .pending_usage
            .output_tokens
            .saturating_add(usage.output_tokens);
        self.pending_usage.cache_read_tokens = self
            .pending_usage
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
    }

    /// Write the pending delta to the session index under the
    /// inter-process lock. A no-op when nothing is pending. On success
    /// the delta resets to zero; on failure it is retained in full so
    /// the next flush attempt (boundary, checkpoint, or drop) retries
    /// it.
    fn flush(&mut self) -> Result<(), SessionPersistError> {
        if self.pending_events == 0 {
            return Ok(());
        }
        update_session_index(
            &self.data_dir,
            &self.session_id,
            self.pending_events,
            &self.pending_usage,
            self.lock_deadline,
        )?;
        self.pending_events = 0;
        self.pending_usage = Usage::default();
        Ok(())
    }
}

/// JSONL file sink — writes each event as one JSON line.
///
/// Lines are assembled fully in memory and written with a single
/// `write_all`, so a failure can tear at most the line being written.
/// Within one sink's lifetime a torn line is remembered
/// (`needs_newline`) and terminated with a lone `\n` before the next
/// write; across process restarts,
/// `open_session_append` heals a torn final line at open time the
/// same way. Either way subsequent events never concatenate onto a
/// partial line, the tolerant reader skips the corrupt line, and every
/// later event still loads (H19, both halves).
///
/// When index-registered (as in every store
/// [`SessionManager`](crate::session::SessionManager) opens), the sink
/// keeps the session's index entry (`event_count`, usage totals,
/// `updated_at`) in step without hand-reconciliation: deltas accumulate
/// in memory and are written under the inter-process index lock at each
/// durability boundary of the configured [`DurabilityPolicy`], at every
/// explicit [`checkpoint`](PersistenceSink::checkpoint), and on drop.
/// An index write failure never fails the event append (the event is
/// already durable); the delta is retained and retried at the next
/// flush point, and
/// [`SessionManager::resume`](crate::session::SessionManager::resume)
/// repairs any staleness a crash leaves behind.
pub struct JsonlSink {
    file: std::fs::File,
    durability: DurabilityPolicy,
    needs_newline: bool,
    events_since_sync: u64,
    index: Option<IndexRegistration>,
}

impl JsonlSink {
    /// Open (or create) the given path in append mode with
    /// [`DurabilityPolicy::Flush`] and no index registration. A version
    /// header line is written when the file is created.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or the header
    /// cannot be written.
    pub fn open(path: &Path) -> Result<Self, SessionPersistError> {
        Self::open_with(path, DurabilityPolicy::Flush)
    }

    /// Open (or create) the given path in append mode with an explicit
    /// durability policy and no index registration.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or the header
    /// cannot be written.
    pub fn open_with(
        path: &Path,
        durability: DurabilityPolicy,
    ) -> Result<Self, SessionPersistError> {
        Ok(Self {
            file: open_session_append(path)?,
            durability,
            needs_newline: false,
            events_since_sync: 0,
            index: None,
        })
    }

    /// Open the session file for `entry` under `data_dir` — the entry's
    /// [`rel_path`](crate::session::persistence::SessionIndexEntry::rel_path)
    /// when present (nested child sessions), the flat `{id}.jsonl`
    /// derivation otherwise — and register the sink against that
    /// session's index entry, so `event_count`, usage totals, and
    /// `updated_at` in `index.jsonl` are kept in step (batched per the
    /// [`DurabilityPolicy`], under the inter-process index lock — see
    /// the type-level docs).
    ///
    /// `index_lock_deadline` bounds the inter-process index-lock wait
    /// of every delta flush this sink performs (`None` = wait
    /// indefinitely; exceeding a deadline surfaces
    /// [`SessionPersistError::IndexLockTimeout`] through the usual flush
    /// error paths, with the delta retained for retry).
    ///
    /// # Errors
    ///
    /// Returns [`SessionPersistError::InvalidSessionId`] when the
    /// entry's id is reserved by the persistence layer (it would name a
    /// persistence-owned file such as `index.jsonl`, never session data),
    /// and an error if the session file cannot be opened or the header
    /// cannot be written.
    pub fn open_registered(
        data_dir: &Path,
        entry: &crate::session::persistence::SessionIndexEntry,
        durability: DurabilityPolicy,
        index_lock_deadline: Option<Duration>,
    ) -> Result<Self, SessionPersistError> {
        crate::session::persistence::io::ensure_session_id_not_reserved(&entry.id)?;
        let mut sink = Self {
            file: open_session_append_for_entry(data_dir, entry)?,
            durability,
            needs_newline: false,
            events_since_sync: 0,
            index: None,
        };
        sink.index = Some(IndexRegistration {
            data_dir: data_dir.to_path_buf(),
            session_id: entry.id.clone(),
            pending_events: 0,
            pending_usage: Usage::default(),
            lock_deadline: index_lock_deadline,
        });
        Ok(sink)
    }
}

impl Drop for JsonlSink {
    fn drop(&mut self) {
        if let Some(registration) = &mut self.index
            && let Err(error) = registration.flush()
        {
            tracing::error!(
                session_id = %registration.session_id,
                %error,
                pending_events = registration.pending_events,
                "failed to flush pending session index delta on sink \
                 close; the index entry is stale until the next resume \
                 repairs it",
            );
        }
    }
}

/// Write one already-assembled JSONL line (terminator included),
/// healing a previously torn line first.
///
/// `needs_newline` is the tear flag: when set, a lone `\n` is written
/// before `line` to terminate the partial line a previous failure left,
/// so the corrupt bytes become exactly one skippable line for the
/// tolerant reader. On any write failure the flag is (re)set so the next
/// call heals again before writing.
fn write_event_line<W: Write>(
    writer: &mut W,
    needs_newline: &mut bool,
    line: &[u8],
) -> std::io::Result<()> {
    if *needs_newline {
        writer.write_all(b"\n")?;
        *needs_newline = false;
    }
    if let Err(error) = writer.write_all(line) {
        *needs_newline = true;
        return Err(error);
    }
    Ok(())
}

impl PersistenceSink for JsonlSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        write_event_line(&mut self.file, &mut self.needs_newline, &line)?;
        let at_durability_boundary = match self.durability {
            DurabilityPolicy::Flush => false,
            DurabilityPolicy::FsyncPerEvent => {
                self.file.sync_all()?;
                true
            }
            DurabilityPolicy::FsyncEveryEvents(n) => {
                self.events_since_sync = self.events_since_sync.saturating_add(1);
                if self.events_since_sync >= n.get() {
                    self.file.sync_all()?;
                    self.events_since_sync = 0;
                    true
                } else {
                    false
                }
            }
        };
        if let Some(registration) = &mut self.index {
            registration.accumulate(event);
            if at_durability_boundary && let Err(error) = registration.flush() {
                // The event IS durable at this point: failing the append
                // would drop it from memory and invite a duplicate-line
                // retry. Keep the delta pending (the next boundary,
                // checkpoint, or drop retries it) and shout.
                tracing::error!(
                    session_id = %registration.session_id,
                    %error,
                    pending_events = registration.pending_events,
                    "event persisted durably but the session index \
                     update failed; delta retained for retry, index \
                     stale until then (or until resume repairs it)",
                );
            }
        }
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        if let Some(registration) = &mut self.index {
            registration.flush()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct StoreInner {
    events: Vec<SessionEvent>,
    index: HashMap<EventId, usize>,
}

impl StoreInner {
    fn push(&mut self, id: EventId, event: SessionEvent) {
        let pos = self.events.len();
        self.events.push(event);
        self.index.insert(id, pos);
    }
}

impl EventStore {
    /// Create an empty event store with no persistence sink.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(StoreInner {
                events: Vec::new(),
                index: HashMap::new(),
            }),
            sink: None,
            spool: None,
        }
    }

    /// Create an event store with a write-through persistence sink.
    pub fn with_sink(sink: Box<dyn PersistenceSink>) -> Self {
        Self {
            inner: RwLock::new(StoreInner {
                events: Vec::new(),
                index: HashMap::new(),
            }),
            sink: Some(Mutex::new(sink)),
            spool: None,
        }
    }

    /// Create an event store with a sink and pre-populated events.
    ///
    /// The events are loaded into the in-memory store without being
    /// written to the sink (they are already persisted from a prior
    /// session). Only future appends go through the sink.
    pub fn with_sink_and_events(sink: Box<dyn PersistenceSink>, events: Vec<SessionEvent>) -> Self {
        let mut index = HashMap::with_capacity(events.len());
        for (pos, event) in events.iter().enumerate() {
            index.insert(event.base().id.clone(), pos);
        }
        Self {
            inner: RwLock::new(StoreInner { events, index }),
            sink: Some(Mutex::new(sink)),
            spool: None,
        }
    }

    /// Attach a [`SpoolWriter`] so oversized tool outputs appended to
    /// this store keep their full payload durably (session-fidelity
    /// Gap 5). Called by
    /// [`SessionManager`](crate::session::SessionManager) on every store
    /// it opens, before the store is shared. A store without a spool has
    /// no session directory to spool into (sink-less stores, embedder
    /// sinks opened outside the manager); appenders of oversized output
    /// must not silently pretend otherwise — [`Self::spool`] makes the
    /// absence observable.
    pub fn attach_spool(&mut self, spool: SpoolWriter) {
        self.spool = Some(spool);
    }

    /// The attached full-output spool, when this store has one.
    #[must_use]
    pub fn spool(&self) -> Option<&SpoolWriter> {
        self.spool.as_ref()
    }

    /// Append an event. Returns its [`EventId`].
    ///
    /// When a persistence sink is installed, the event is written
    /// through to the sink **before** it is added to the in-memory
    /// store. Disk I/O runs under the sink mutex only, so concurrent
    /// readers of the in-memory state are never blocked by a slow
    /// write; concurrent appends serialise on the sink (file order must
    /// match memory order).
    ///
    /// # Errors
    ///
    /// * [`SessionError::EventAppendFailed`] if the event ID already
    ///   exists in the store.
    /// * [`SessionError::StorageError`] if the sink fails to persist the
    ///   event. The event is **not** in the in-memory store in that
    ///   case, so retrying the same event is safe: the sink guarantees a
    ///   partial line from the failure is never continued onto, and if
    ///   the failed attempt did reach the file, the duplicate line the
    ///   retry leaves is skipped by the tolerant reader on resume.
    pub fn append(&self, event: SessionEvent) -> Result<EventId, SessionError> {
        let id = event.base().id.clone();
        if let Some(sink) = &self.sink {
            // The sink mutex is the append serialiser: holding it across
            // the duplicate check, the disk write, and the in-memory push
            // keeps file order identical to memory order without holding
            // the state lock during I/O.
            let mut sink_guard = sink.lock();
            if self.inner.read().index.contains_key(&id) {
                return Err(SessionError::EventAppendFailed {
                    reason: format!("duplicate event ID: {id}"),
                });
            }
            sink_guard.persist(&event).map_err(SessionError::from)?;
            self.inner.write().push(id.clone(), event);
        } else {
            let mut inner = self.inner.write();
            if inner.index.contains_key(&id) {
                return Err(SessionError::EventAppendFailed {
                    reason: format!("duplicate event ID: {id}"),
                });
            }
            inner.push(id.clone(), event);
        }
        Ok(id)
    }

    /// Retrieve an event by ID. Returns a clone.
    #[must_use]
    pub fn get(&self, id: &EventId) -> Option<SessionEvent> {
        let inner = self.inner.read();
        inner.index.get(id).map(|&pos| inner.events[pos].clone())
    }

    /// Return all events in insertion order.
    #[must_use]
    pub fn events(&self) -> Vec<SessionEvent> {
        self.inner.read().events.clone()
    }

    /// Run `f` over the events in insertion order without cloning them.
    ///
    /// The read lock is held for the duration of `f`, so callers that only
    /// need to inspect (not retain) event bodies avoid copying the whole
    /// history — unlike [`Self::events`]. `f` must not call back into the
    /// store, which would deadlock on the held read lock.
    pub fn with_events<R>(&self, f: impl FnOnce(&[SessionEvent]) -> R) -> R {
        f(&self.inner.read().events)
    }

    /// Return up to `count` most recent events in insertion order.
    ///
    /// This clones only the returned tail window, unlike [`Self::events`],
    /// so callers that only need recent visible context do not pay to copy
    /// large session histories.
    #[must_use]
    pub fn last_events(&self, count: usize) -> Vec<SessionEvent> {
        let inner = self.inner.read();
        let start = inner.events.len().saturating_sub(count);
        inner.events[start..].to_vec()
    }

    /// Return the number of stored events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().events.len()
    }

    /// Return `true` if the store contains no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.read().events.is_empty()
    }

    /// Flush any bookkeeping the persistence sink has deferred (e.g. a
    /// batched session-index delta under
    /// [`DurabilityPolicy::Flush`]). A no-op when no sink is installed.
    /// Embedders call this at turn boundaries so session listings stay
    /// fresh without paying an index rewrite per event.
    ///
    /// For an index-registered sink this performs blocking file I/O:
    /// the inter-process index lock is taken (an unbounded wait unless
    /// the opening [`SessionManager`](crate::session::SessionManager)
    /// configured a deadline) and held across a full index
    /// read+rewrite+fsync. Callers on an async executor use
    /// [`Self::checkpoint_off_executor`] instead so that critical
    /// section never stalls an executor thread.
    ///
    /// # Errors
    ///
    /// [`SessionError::StorageError`] when the sink's checkpoint fails.
    /// The sink retains its deferred state, so a later `checkpoint` (or
    /// dropping the store) retries it; already-persisted events are
    /// unaffected.
    pub fn checkpoint(&self) -> Result<(), SessionError> {
        if let Some(sink) = &self.sink {
            sink.lock().checkpoint().map_err(SessionError::from)?;
        }
        Ok(())
    }

    /// [`Self::checkpoint`], with the blocking critical section (index
    /// lock acquisition + read + rewrite + fsync) moved off the async
    /// executor onto Tokio's blocking pool.
    ///
    /// This is the step-boundary flush for async embedders (see the
    /// keep-sessions-open guidance on
    /// [`SessionManager`](crate::session::SessionManager)): an executor
    /// thread never waits on the inter-process index lock or pays the
    /// index rewrite. Error semantics are identical to
    /// [`Self::checkpoint`] — a failure leaves the deferred delta
    /// retained for retry. Must be awaited within a Tokio runtime
    /// context ([`tokio::task::spawn_blocking`] requires one).
    ///
    /// # Errors
    ///
    /// [`SessionError::StorageError`] when the sink's checkpoint fails,
    /// or when the blocking task itself dies before reporting (a panic
    /// in a custom [`PersistenceSink`] implementation, or runtime
    /// shutdown cancelling the task) — in that case whether the flush
    /// landed is unknown, and the next checkpoint or the resume-time
    /// index repair reconciles it.
    pub async fn checkpoint_off_executor(self: Arc<Self>) -> Result<(), SessionError> {
        tokio::task::spawn_blocking(move || self.checkpoint())
            .await
            .map_err(|e| SessionError::StorageError {
                reason: format!("session checkpoint task failed before reporting: {e}"),
            })?
    }

    /// Return the ID of the most recently appended event, if any.
    #[must_use]
    pub fn last_event_id(&self) -> Option<EventId> {
        self.inner.read().events.last().map(|e| e.base().id.clone())
    }

    /// Consume the store and return its events in insertion order
    /// without cloning. Any installed sink is dropped (closing its
    /// file handle); use this when rebuilding a store around the same
    /// events with a different sink.
    #[must_use]
    pub fn into_events(self) -> Vec<SessionEvent> {
        self.inner.into_inner().events
    }
}

impl Default for EventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::session::events::EventBase;

    fn user_msg(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    #[test]
    fn append_and_retrieve_by_id() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            let id = store.append(user_msg(&format!("msg {i}"))).expect("append");
            ids.push(id);
        }
        assert_eq!(store.len(), 5);
        for id in &ids {
            assert!(store.get(id).is_some());
        }
    }

    #[test]
    fn events_in_insertion_order() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            let id = store.append(user_msg(&format!("msg {i}"))).expect("append");
            ids.push(id);
        }
        let events = store.events();
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.base().id, ids[i]);
        }
    }

    #[test]
    fn last_events_returns_tail_in_insertion_order() {
        let store = EventStore::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            let id = store.append(user_msg(&format!("msg {i}"))).expect("append");
            ids.push(id);
        }

        let events = store.last_events(2);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].base().id, ids[3]);
        assert_eq!(events[1].base().id, ids[4]);
    }

    #[test]
    fn last_events_count_above_len_returns_all_events() {
        let store = EventStore::new();
        store.append(user_msg("first")).expect("append first");
        store.append(user_msg("second")).expect("append second");

        let events = store.last_events(10);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            SessionEvent::UserMessage { content, .. } if content == "first"
        ));
        assert!(matches!(
            &events[1],
            SessionEvent::UserMessage { content, .. } if content == "second"
        ));
    }

    #[test]
    fn last_events_zero_returns_empty_window() {
        let store = EventStore::new();
        store.append(user_msg("first")).expect("append");

        assert!(store.last_events(0).is_empty());
    }

    #[test]
    fn duplicate_id_rejected() {
        let store = EventStore::new();
        let event = user_msg("hello");
        let id = event.base().id.clone();
        store.append(event).expect("first append");

        let dup = SessionEvent::UserMessage {
            base: EventBase {
                id,
                parent_id: None,
                timestamp: chrono::Utc::now(),
            },
            content: "dup".to_owned(),
        };
        assert!(store.append(dup).is_err());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let store = EventStore::new();
        assert!(store.get(&EventId::new()).is_none());
    }

    #[test]
    fn is_empty_initial() {
        let store = EventStore::new();
        assert!(store.is_empty());
        store.append(user_msg("a")).expect("append");
        assert!(!store.is_empty());
    }

    /// A sink that fails a configurable number of times, then succeeds.
    struct FlakySink {
        failures_left: u32,
        persisted: Vec<String>,
    }

    impl PersistenceSink for FlakySink {
        fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
            if self.failures_left > 0 {
                self.failures_left -= 1;
                return Err(SessionPersistError::Io(std::io::Error::other(
                    "simulated persist failure",
                )));
            }
            self.persisted
                .push(serde_json::to_string(event).expect("serialize"));
            Ok(())
        }
    }

    /// Regression for H19's write side: a sink failure must surface as a
    /// typed error from `append`, the event must not enter the in-memory
    /// store, and an immediate retry of the SAME event must succeed
    /// (no duplicate-ID trap).
    #[test]
    fn sink_failure_surfaces_typed_error_and_retry_is_safe() {
        let store = EventStore::with_sink(Box::new(FlakySink {
            failures_left: 1,
            persisted: Vec::new(),
        }));
        let event = user_msg("important");

        let err = store.append(event.clone()).expect_err("sink must fail");
        assert!(
            matches!(err, SessionError::StorageError { .. }),
            "expected StorageError, got {err:?}",
        );
        assert_eq!(
            store.len(),
            0,
            "failed persist must not leave the event in memory",
        );

        let id = store.append(event).expect("retry succeeds");
        assert_eq!(store.len(), 1);
        assert!(store.get(&id).is_some());
    }

    /// A sink whose `checkpoint` fails a configurable number of times,
    /// then succeeds — models a transient index-flush failure.
    struct FlakyCheckpointSink {
        checkpoint_failures_left: u32,
        checkpoints_succeeded: u32,
    }

    impl PersistenceSink for FlakyCheckpointSink {
        fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
            Ok(())
        }

        fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
            if self.checkpoint_failures_left > 0 {
                self.checkpoint_failures_left -= 1;
                return Err(SessionPersistError::Io(std::io::Error::other(
                    "simulated checkpoint failure",
                )));
            }
            self.checkpoints_succeeded += 1;
            Ok(())
        }
    }

    /// No silent in-memory degradation on the checkpoint path either: a
    /// failing sink checkpoint must surface as a typed `StorageError`
    /// (never `Ok`), already-persisted events must be unaffected, and a
    /// retry must reach the sink again.
    #[test]
    fn checkpoint_failure_surfaces_typed_error_and_retry_reaches_sink() {
        let store = EventStore::with_sink(Box::new(FlakyCheckpointSink {
            checkpoint_failures_left: 1,
            checkpoints_succeeded: 0,
        }));
        store.append(user_msg("kept")).expect("append succeeds");

        let err = store.checkpoint().expect_err("first checkpoint must fail");
        assert!(
            matches!(err, SessionError::StorageError { .. }),
            "expected StorageError, got {err:?}",
        );
        assert_eq!(store.len(), 1, "persisted events are unaffected");

        store.checkpoint().expect("retry succeeds");
    }

    /// A sink that records which thread its `checkpoint` ran on.
    struct ThreadRecordingSink {
        checkpoint_thread: std::sync::Arc<parking_lot::Mutex<Option<std::thread::ThreadId>>>,
    }

    impl PersistenceSink for ThreadRecordingSink {
        fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
            Ok(())
        }

        fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
            *self.checkpoint_thread.lock() = Some(std::thread::current().id());
            Ok(())
        }
    }

    /// R2's off-executor guarantee: `checkpoint_off_executor` must run
    /// the sink's critical section on the blocking pool, never on the
    /// executor thread. On a current-thread runtime every task polls on
    /// the test thread, so the sink observing a DIFFERENT thread proves
    /// the offload.
    #[tokio::test]
    async fn checkpoint_off_executor_runs_critical_section_off_the_executor_thread() {
        let checkpoint_thread = std::sync::Arc::new(parking_lot::Mutex::new(None));
        let store = std::sync::Arc::new(EventStore::with_sink(Box::new(ThreadRecordingSink {
            checkpoint_thread: std::sync::Arc::clone(&checkpoint_thread),
        })));
        store.append(user_msg("step")).expect("append");

        std::sync::Arc::clone(&store)
            .checkpoint_off_executor()
            .await
            .expect("checkpoint succeeds");

        let recorded = checkpoint_thread.lock().expect("sink checkpoint ran");
        assert_ne!(
            recorded,
            std::thread::current().id(),
            "the checkpoint critical section must not run on the executor thread",
        );
    }

    /// Failure path parity with the sync `checkpoint`: a failing sink
    /// checkpoint surfaces the typed `StorageError` through the
    /// off-executor path, the delta stays retained, and a retry reaches
    /// the sink again.
    #[tokio::test]
    async fn checkpoint_off_executor_surfaces_typed_error_and_retry_reaches_sink() {
        let store = std::sync::Arc::new(EventStore::with_sink(Box::new(FlakyCheckpointSink {
            checkpoint_failures_left: 1,
            checkpoints_succeeded: 0,
        })));
        store.append(user_msg("kept")).expect("append succeeds");

        let err = std::sync::Arc::clone(&store)
            .checkpoint_off_executor()
            .await
            .expect_err("first checkpoint must fail");
        assert!(
            matches!(err, SessionError::StorageError { .. }),
            "expected StorageError, got {err:?}",
        );
        assert_eq!(store.len(), 1, "persisted events are unaffected");

        std::sync::Arc::clone(&store)
            .checkpoint_off_executor()
            .await
            .expect("retry succeeds");
    }

    /// A writer that fails after writing a fixed number of bytes, then
    /// writes normally — simulates ENOSPC mid-line.
    struct TornWriter {
        bytes_before_failure: usize,
        written: Vec<u8>,
        failed: bool,
    }

    impl Write for TornWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if !self.failed && self.written.len() + buf.len() > self.bytes_before_failure {
                let room = self.bytes_before_failure - self.written.len();
                self.written.extend_from_slice(&buf[..room]);
                self.failed = true;
                return Err(std::io::Error::other("disk full"));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Regression for H19's torn-line corruption: after a partial write,
    /// the next line must NOT be concatenated onto the torn bytes — the
    /// tear is terminated with a newline so the corrupt prefix is exactly
    /// one skippable line and the next line parses cleanly.
    #[test]
    fn torn_line_is_terminated_not_continued() {
        let mut writer = TornWriter {
            bytes_before_failure: 5,
            written: Vec::new(),
            failed: false,
        };
        let mut needs_newline = false;

        let first = b"{\"type\":\"first\"}\n";
        let err = write_event_line(&mut writer, &mut needs_newline, first);
        assert!(err.is_err(), "first write must tear");
        assert!(needs_newline, "tear must be remembered");

        let second = b"{\"second\":true}\n";
        write_event_line(&mut writer, &mut needs_newline, second).expect("second write succeeds");
        assert!(!needs_newline);

        let content = String::from_utf8(writer.written).expect("utf8");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "torn bytes and new line must be separate");
        assert!(
            serde_json::from_str::<serde_json::Value>(lines[0]).is_err(),
            "torn prefix is corrupt (skippable)",
        );
        let parsed: serde_json::Value =
            serde_json::from_str(lines[1]).expect("second line must parse cleanly");
        assert_eq!(parsed["second"], true);
    }

    /// A sink that blocks inside `persist` until released, to prove disk
    /// I/O no longer runs under the in-memory state lock.
    struct BlockingSink {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl PersistenceSink for BlockingSink {
        fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
            self.entered.send(()).expect("notify entered");
            self.release.recv().expect("wait for release");
            Ok(())
        }
    }

    /// Regression for the executor-thread stall: while a slow sink write
    /// is in flight, readers of the in-memory state must not block.
    #[test]
    fn slow_sink_write_does_not_block_readers() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = std::sync::Arc::new(EventStore::with_sink(Box::new(BlockingSink {
            entered: entered_tx,
            release: release_rx,
        })));

        let appender = {
            let store = std::sync::Arc::clone(&store);
            std::thread::spawn(move || {
                store.append(user_msg("slow")).expect("append");
            })
        };

        // Wait until the sink is mid-write (holding the sink mutex).
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("sink entered");

        // Reads must complete while the write is still blocked.
        let reader = {
            let store = std::sync::Arc::clone(&store);
            std::thread::spawn(move || (store.len(), store.is_empty(), store.events().len()))
        };
        let (len, empty, events_len) = reader.join().expect("reader must not deadlock");
        assert_eq!(len, 0, "event is not visible until persisted");
        assert!(empty);
        assert_eq!(events_len, 0);

        release_tx.send(()).expect("release sink");
        appender.join().expect("appender finishes");
        assert_eq!(store.len(), 1);
    }
}
