//! Session lifecycle operations (NC-002 R4–R6).

use std::path::Path;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::{DurabilityPolicy, EventStore, JsonlSink};
use chrono::Utc;
use uuid::Uuid;

use super::index::{
    append_index_entry, resolve_session, sum_usage_from_events, update_index_entry,
};
use super::io::{append_events, read_session_events};
use super::types::{SESSION_FORMAT_VERSION, SessionIndexEntry, SessionPersistError, SessionStatus};

/// Create a fresh session: generate a UUID v7 ID, append an index entry
/// with `event_count = 0`, `format_version = SESSION_FORMAT_VERSION`,
/// and the supplied metadata, and return the entry.
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
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
    };
    append_index_entry(data_dir, &entry)?;
    Ok(entry)
}

/// Resume a persisted session: resolve the identifier, tolerantly read
/// its event file, replay every recovered event into a fresh
/// [`EventStore`], and return the store plus the raw event vector (so
/// the caller can rebuild provider message history) and the resolved
/// index entry.
///
/// Corrupt or unknown event lines (e.g. a final line torn by `ENOSPC` or
/// `kill -9`) and duplicate-`EventId` lines (crash-retry artifacts) are
/// skipped with a warning rather than failing the resume — see
/// [`read_session_events`].
///
/// Resume is also the index's self-maintenance point: `event_count` and
/// the usage totals of the resolved entry are recomputed from the
/// recovered events, and a drifted entry (crash before a deferred index
/// delta landed, an index update that failed after a durable append, or
/// duplicate lines that were double-counted) is repaired in
/// `index.jsonl`. A failed repair is logged and never fails the resume;
/// the returned entry always carries the recomputed values.
pub fn resume_session(
    data_dir: &Path,
    id_or_name: &str,
) -> Result<(EventStore, Vec<SessionEvent>, SessionIndexEntry), SessionPersistError> {
    let entry = resolve_session(data_dir, id_or_name)?;
    let events = read_session_events(data_dir, &entry.id)?.events;
    let entry = reconcile_index_entry(data_dir, entry, &events);
    let store = EventStore::new();
    for event in &events {
        store.append(event.clone())?;
    }
    Ok((store, events, entry))
}

/// Compare `entry`'s `event_count` and usage totals against the events
/// actually recovered from the session file and repair the index entry
/// when they drifted (the crash-staleness window the batched index
/// maintenance accepts by design). Returns the entry with the
/// recomputed values; a failed repair write is logged at error level
/// and the recomputed (in-memory) values are still returned so the
/// caller never sees stale numbers.
fn reconcile_index_entry(
    data_dir: &Path,
    entry: SessionIndexEntry,
    events: &[SessionEvent],
) -> SessionIndexEntry {
    let actual_count = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let actual_usage = sum_usage_from_events(events);
    if entry.event_count == actual_count
        && entry.total_input_tokens == actual_usage.input_tokens
        && entry.total_output_tokens == actual_usage.output_tokens
        && entry.total_cache_read_tokens == actual_usage.cache_read_tokens
    {
        return entry;
    }
    tracing::warn!(
        session_id = %entry.id,
        indexed_count = entry.event_count,
        actual_count,
        "session index entry drifted from the event file (crash before \
         a deferred index delta landed, or a failed index update after \
         a durable append); repairing",
    );
    let mut repaired = entry;
    repaired.event_count = actual_count;
    repaired.total_input_tokens = actual_usage.input_tokens;
    repaired.total_output_tokens = actual_usage.output_tokens;
    repaired.total_cache_read_tokens = actual_usage.cache_read_tokens;
    if let Err(error) = update_index_entry(data_dir, &repaired.id, |e| {
        e.event_count = actual_count;
        e.total_input_tokens = actual_usage.input_tokens;
        e.total_output_tokens = actual_usage.output_tokens;
        e.total_cache_read_tokens = actual_usage.cache_read_tokens;
    }) {
        tracing::error!(
            session_id = %repaired.id,
            %error,
            "failed to persist the repaired session index entry; resume \
             continues with the recomputed values",
        );
    }
    repaired
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
    let source_events = read_session_events(data_dir, &source.id)?.events;
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

/// Install a write-through, index-registered
/// [`JsonlSink`] on an [`EventStore`] so every appended event is
/// immediately persisted to the session JSONL file, with the session's
/// index entry (`event_count`, usage totals, `updated_at`) maintained
/// by the sink — embedders never reconcile the index by hand.
///
/// `durability` controls post-write durability per event **and** the
/// index-maintenance cadence (see [`DurabilityPolicy`]): index deltas
/// accumulate in memory and are written under the inter-process index
/// lock at each durability boundary, at every
/// [`EventStore::checkpoint`] call (the per-turn hook embedders should
/// use under [`DurabilityPolicy::Flush`], which has no per-event
/// boundary), and when the store/sink is dropped. Staleness from a
/// crash in between is repaired by [`resume_session`].
///
/// Pass [`DurabilityPolicy::Flush`] for the historical behaviour (no
/// fsync of the session file on the live path).
///
/// # Errors
///
/// Returns the underlying error when the session file cannot be opened
/// — persistence is never silently degraded to memory-only.
pub fn attach_sink(
    store: EventStore,
    data_dir: &Path,
    session_id: &str,
    durability: DurabilityPolicy,
) -> Result<EventStore, SessionPersistError> {
    let sink = JsonlSink::open_registered(data_dir, session_id, durability)?;
    Ok(EventStore::with_sink_and_events(
        Box::new(sink),
        store.into_events(),
    ))
}
