//! Append-only event storage with optional write-through persistence.

use std::collections::HashMap;
use std::io::Write;

use parking_lot::{Mutex, RwLock};

use crate::error::SessionError;
use crate::session::events::{EventId, SessionEvent};

/// Append-only, in-memory event store.
///
/// Events can be appended and retrieved but never deleted, modified, or
/// replaced. Uses `parking_lot::RwLock` for `Send + Sync` without poison
/// handling (satisfies CO4).
///
/// When a [`PersistenceSink`] is installed, every appended event is
/// written through to the sink before the method returns. A mid-process
/// crash still loses at most the event being appended — all prior events
/// are durable.
pub struct EventStore {
    inner: RwLock<StoreInner>,
    sink: Option<Mutex<Box<dyn PersistenceSink>>>,
}

impl std::fmt::Debug for EventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStore")
            .field("len", &self.inner.read().events.len())
            .field("has_sink", &self.sink.is_some())
            .finish()
    }
}

/// Receives each event as it is appended to the store.
///
/// Implementations must be `Send` so the store remains `Send + Sync`.
/// Errors are logged but do not prevent the in-memory append — the store
/// is the source of truth for the running session; the sink is a
/// durability layer.
pub trait PersistenceSink: Send {
    /// Write one event. Called while the store's write lock is held, so
    /// implementations must not call back into the store.
    fn persist(&mut self, event: &SessionEvent);
}

/// JSONL file sink — writes each event as a JSON line and flushes.
pub struct JsonlSink {
    writer: std::io::BufWriter<std::fs::File>,
}

impl JsonlSink {
    /// Open (or create) the given path in append mode.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be opened.
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: std::io::BufWriter::new(file),
        })
    }
}

impl PersistenceSink for JsonlSink {
    fn persist(&mut self, event: &SessionEvent) {
        if let Err(e) = serde_json::to_writer(&mut self.writer, event) {
            tracing::error!("session write-through serialization error: {e}");
            return;
        }
        if let Err(e) = self.writer.write_all(b"\n") {
            tracing::error!("session write-through newline error: {e}");
            return;
        }
        if let Err(e) = self.writer.flush() {
            tracing::error!("session write-through flush error: {e}");
        }
    }
}

#[derive(Debug)]
struct StoreInner {
    events: Vec<SessionEvent>,
    index: HashMap<EventId, usize>,
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
        }
    }

    /// Append an event. Returns its [`EventId`].
    ///
    /// When a persistence sink is installed, the event is written through
    /// to the sink before this method returns. Sink errors are logged but
    /// do not block the append.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EventAppendFailed`] if the event ID already
    /// exists in the store.
    pub fn append(&self, event: SessionEvent) -> Result<EventId, SessionError> {
        let id = event.base().id.clone();
        let mut inner = self.inner.write();
        if inner.index.contains_key(&id) {
            return Err(SessionError::EventAppendFailed {
                reason: format!("duplicate event ID: {id}"),
            });
        }
        if let Some(sink) = &self.sink {
            sink.lock().persist(&event);
        }
        let pos = inner.events.len();
        inner.events.push(event);
        inner.index.insert(id.clone(), pos);
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

    /// Return the ID of the most recently appended event, if any.
    #[must_use]
    pub fn last_event_id(&self) -> Option<EventId> {
        self.inner.read().events.last().map(|e| e.base().id.clone())
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
}
