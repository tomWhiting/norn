//! Append-only event storage with optional write-through persistence.

mod append_batch;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::error::SessionError;
use crate::provider::ProviderStateIdentity;
use crate::session::events::{EventId, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::persistence::SessionPersistError;
use crate::session::provider_affinity::{ManagedProviderAffinity, ProviderAffinity};
use crate::session::response_audio::ResponseAudioStore;
use crate::session::spool::SpoolWriter;

pub use super::jsonl_sink::{DurabilityPolicy, JsonlSink};

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
    response_audio: Option<ResponseAudioStore>,
    provider_affinity: ProviderAffinity,
}

impl std::fmt::Debug for EventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStore")
            .field("len", &self.inner.read().events.len())
            .field("has_sink", &self.sink.is_some())
            .field("has_spool", &self.spool.is_some())
            .field("has_response_audio", &self.response_audio.is_some())
            .field(
                "provider_state_identity_present",
                &self.provider_affinity.identity().is_some(),
            )
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
/// durability than disk has. A sink that can report an ambiguous write
/// failure must reconcile an exact durable retry without adding a duplicate
/// event; strict session readers reject duplicate `EventId` rows.
pub trait PersistenceSink: Send {
    /// Write one event durably according to the sink's policy.
    ///
    /// # Errors
    ///
    /// Returns the underlying persistence failure; the caller decides
    /// whether to retry or abort.
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError>;

    /// Write one event group without allowing another writer targeting the
    /// same durable stream to interleave rows.
    ///
    /// Shared-stream sinks must override this method and retain their external
    /// serialization authority for the complete group. The default accepts an
    /// empty or single-row group only; looping over `persist` would advertise a
    /// guarantee an arbitrary sink cannot provide.
    ///
    /// # Errors
    ///
    /// Returns the underlying persistence failure. A failure may leave an
    /// exact durable prefix, but memory must remain unchanged until a complete
    /// retry succeeds. Reopening an incomplete semantic prefix fails closed;
    /// it does not silently repair or discard the prefix.
    fn persist_batch(&mut self, events: &[SessionEvent]) -> Result<(), SessionPersistError> {
        match events {
            [] => Ok(()),
            [event] => self.persist(event),
            [first, ..] => Err(SessionPersistError::EventAppendConflict {
                event_id: first.base().id.to_string(),
                reason: "the persistence sink does not support atomic event groups",
            }),
        }
    }

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

    /// Update the provider-state identity expected by subsequent appends.
    ///
    /// Registered session sinks use this as an append-time compare-and-swap
    /// guard. Generic embedder sinks may ignore it because their affinity is
    /// enforced by the owning in-memory [`EventStore`].
    fn set_provider_state_identity(&mut self, _identity: Option<ProviderStateIdentity>) {}
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

    fn provider_identity_adoption_parent(&self) -> Option<EventId> {
        let previous = self.events.last()?;
        if matches!(
            previous,
            SessionEvent::ProviderEpochBoundary {
                reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
                ..
            }
        ) {
            return None;
        }
        Some(previous.base().id.clone())
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
            response_audio: None,
            provider_affinity: ProviderAffinity::sinkless(),
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
            response_audio: None,
            provider_affinity: ProviderAffinity::sinkless(),
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
            response_audio: None,
            provider_affinity: ProviderAffinity::sinkless(),
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

    /// Attach the generation-bound response-audio artifact authority for this
    /// managed timeline. Sink-less/embedder stores remain explicitly absent.
    pub fn attach_response_audio(&mut self, store: ResponseAudioStore) {
        self.response_audio = Some(store);
    }

    /// The response-audio authority attached to this managed timeline.
    #[must_use]
    pub fn response_audio(&self) -> Option<&ResponseAudioStore> {
        self.response_audio.as_ref()
    }

    /// Attach the durable provider-state affinity authority for this managed
    /// session. Called by [`SessionManager`](crate::session::SessionManager)
    /// before the store is shared.
    pub(super) fn attach_provider_affinity(&mut self, authority: ManagedProviderAffinity) {
        self.provider_affinity = ProviderAffinity::managed(authority);
    }

    /// The opaque provider identity currently bound to this store, if any.
    #[must_use]
    pub fn provider_state_identity(&self) -> Option<ProviderStateIdentity> {
        self.provider_affinity.identity()
    }

    /// Bind this store to `requested` exactly once or validate the existing
    /// provider-state identity.
    ///
    /// Managed stores perform the transition under the recovered
    /// inter-process index lock and exact generation check. Sink-less stores
    /// retain the same one-time binding in memory. `None` is accepted only
    /// while the store remains unbound; it cannot bypass an existing binding.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence error when the identity differs, is absent
    /// for an already-bound store, or cannot be committed durably.
    pub fn validate_or_bind_provider_state_identity(
        &self,
        requested: Option<ProviderStateIdentity>,
    ) -> Result<(), SessionPersistError> {
        // The sink lock is the store's append-order authority. Hold it while a
        // managed late adoption writes its boundary directly, then mirror that
        // already-durable event into memory before another local append or
        // provider request can observe the store.
        if let Some(sink) = &self.sink {
            let mut sink = sink.lock();
            let adoption_parent = self.inner.read().provider_identity_adoption_parent();
            let boundary = self.provider_affinity.validate_or_bind(
                requested,
                adoption_parent.as_ref(),
                |event| {
                    sink.persist(event)?;
                    let id = event.base().id.clone();
                    self.inner.write().push(id, event.clone());
                    Ok(())
                },
            )?;
            sink.set_provider_state_identity(self.provider_affinity.identity());
            if let Some(event) = boundary {
                let id = event.base().id.clone();
                let mut inner = self.inner.write();
                if !inner.index.contains_key(&id) {
                    inner.push(id, event);
                }
            }
        } else {
            // The write guard is the append-order authority for an in-memory
            // store. It keeps first adoption and concurrent appends in one
            // order while the provider-affinity transition is committed.
            let mut inner = self.inner.write();
            let adoption_parent = inner.provider_identity_adoption_parent();
            let boundary = self.provider_affinity.validate_or_bind(
                requested,
                adoption_parent.as_ref(),
                |event| {
                    let id = event.base().id.clone();
                    inner.push(id, event.clone());
                    Ok(())
                },
            )?;
            if let Some(event) = boundary {
                let id = event.base().id.clone();
                if !inner.index.contains_key(&id) {
                    inner.push(id, event);
                }
            }
        }
        Ok(())
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
    ///   case. Retry safety is a sink invariant: partial rows cannot be
    ///   continued, and an exact event already made durable by an ambiguous
    ///   attempt must be recognised rather than appended twice.
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
mod tests;
