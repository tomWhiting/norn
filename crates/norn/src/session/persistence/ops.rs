//! Session lifecycle operations (NC-002 R4–R6).

use std::path::Path;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::{EventStore, JsonlSink};
use chrono::Utc;
use uuid::Uuid;

use super::io::{
    append_events, append_index_entry, read_session_events, resolve_session, session_file_path,
};
use super::types::{SessionIndexEntry, SessionPersistError, SessionStatus};

/// Create a fresh session: generate a UUID v7 ID, append an index entry
/// with `event_count = 0` and the supplied metadata, and return the entry.
pub fn create_session(
    data_dir: &Path,
    model: String,
    working_dir: String,
    name: Option<String>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let now = Utc::now();
    let entry = SessionIndexEntry {
        id: Uuid::now_v7().to_string(),
        name,
        model,
        working_dir,
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
    };
    append_index_entry(data_dir, entry.clone())?;
    Ok(entry)
}

/// Resume a persisted session: resolve the identifier, replay every
/// event into a fresh [`EventStore`], and return the store plus the raw
/// event vector (so the caller can rebuild provider message history) and
/// the resolved index entry.
pub fn resume_session(
    data_dir: &Path,
    id_or_name: &str,
) -> Result<(EventStore, Vec<SessionEvent>, SessionIndexEntry), SessionPersistError> {
    let entry = resolve_session(data_dir, id_or_name)?;
    let events = read_session_events(data_dir, &entry.id)?;
    let store = EventStore::new();
    for event in &events {
        store.append(event.clone())?;
    }
    Ok((store, events, entry))
}

/// Fork an existing session: copy every source event into a brand new
/// session, then append a `Fork` event whose `source_event_id` is the
/// last event's ID and whose `forked_session_id` is the new session ID.
///
/// `model` and `working_dir` populate the new index entry — callers
/// typically pass the source session's values but may override (e.g.
/// when `--working-dir` is supplied on the CLI).
///
/// Returns the new index entry, an [`EventStore`] pre-populated with the
/// copied events plus the appended `Fork` event, and the raw event
/// sequence so the caller can rebuild provider message history.
pub fn fork_session(
    data_dir: &Path,
    id_or_name: &str,
    model: String,
    working_dir: String,
) -> Result<(SessionIndexEntry, EventStore, Vec<SessionEvent>), SessionPersistError> {
    let source = resolve_session(data_dir, id_or_name)?;
    let source_events = read_session_events(data_dir, &source.id)?;
    let last_event = source_events
        .last()
        .ok_or_else(|| SessionPersistError::EmptySource {
            id: source.id.clone(),
        })?;
    let last_event_id = last_event.base().id.clone();

    let new_entry = create_session(data_dir, model, working_dir, None)?;
    let new_id = new_entry.id;

    append_events(data_dir, &new_id, &source_events, false)?;

    let fork_event = SessionEvent::Fork {
        base: EventBase::new(Some(last_event_id.clone())),
        source_event_id: last_event_id,
        forked_session_id: new_id.clone(),
    };
    append_events(data_dir, &new_id, std::slice::from_ref(&fork_event), false)?;

    let mut all_events = source_events;
    all_events.push(fork_event);
    let store = EventStore::new();
    for event in &all_events {
        store.append(event.clone())?;
    }

    let final_entry = resolve_session(data_dir, &new_id)?;
    Ok((final_entry, store, all_events))
}

/// Install a write-through [`JsonlSink`] on an [`EventStore`] so every
/// appended event is immediately persisted to the session JSONL file.
///
/// On I/O failure the store is returned unchanged (events stay in memory
/// but are not persisted). A `tracing::error!` is emitted so the caller
/// knows persistence is degraded.
pub fn attach_sink(store: EventStore, data_dir: &Path, session_id: &str) -> EventStore {
    let path = session_file_path(data_dir, session_id);
    match JsonlSink::open(&path) {
        Ok(sink) => EventStore::with_sink_and_events(Box::new(sink), store.events()),
        Err(e) => {
            tracing::error!("failed to open session sink at {}: {e}", path.display());
            store
        }
    }
}
