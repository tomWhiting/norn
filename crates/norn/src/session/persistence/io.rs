//! JSONL I/O for sessions and the session index (NC-002 R2–R3).

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use super::types::{SessionIndexEntry, SessionPersistError};

/// Return the JSONL file path for `session_id` under `data_dir`.
#[must_use]
pub fn session_file_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join(format!("{session_id}.jsonl"))
}

/// Return the session index file path under `data_dir`.
#[must_use]
pub fn index_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("index.jsonl")
}

/// Read every [`SessionEvent`] from `{data_dir}/{session_id}.jsonl`.
///
/// Returns an empty vector when the file does not exist or is empty.
/// Empty / whitespace-only lines are skipped. A parse failure reports
/// the 1-based line number via [`SessionPersistError::Parse`].
pub fn read_session_events(
    data_dir: &Path,
    session_id: &str,
) -> Result<Vec<SessionEvent>, SessionPersistError> {
    let path = session_file_path(data_dir, session_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: SessionEvent =
            serde_json::from_str(&line).map_err(|source| SessionPersistError::Parse {
                line: idx + 1,
                source,
            })?;
        out.push(event);
    }
    Ok(out)
}

/// Append `events` to `{data_dir}/{session_id}.jsonl` and update the
/// matching index entry's `event_count` and `updated_at`.
///
/// `disabled = true` short-circuits the call with `Ok(())` and performs
/// no filesystem work — this is the `--no-session` path.
///
/// Empty `events` is a no-op. The session JSONL file and its parent
/// directory are created on first write. The index entry MUST already
/// exist; missing entries return [`SessionPersistError::NotFound`].
pub fn append_events(
    data_dir: &Path,
    session_id: &str,
    events: &[SessionEvent],
    disabled: bool,
) -> Result<(), SessionPersistError> {
    if disabled || events.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(data_dir)?;
    let path = session_file_path(data_dir, session_id);
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let mut writer = BufWriter::new(file);
    for event in events {
        serde_json::to_writer(&mut writer, event)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;

    let appended = u64::try_from(events.len()).unwrap_or(u64::MAX);
    let usage_delta = sum_usage_from_events(events);
    update_session_index(data_dir, session_id, appended, &usage_delta)
}

/// Update the session index entry for `session_id` to reflect newly
/// persisted events without touching the session JSONL file.
///
/// This is the index-update half of [`append_events`] with the JSONL
/// write skipped. Use it when events were already durably persisted
/// through a sink (the `JsonlSink` write-through path) and only the
/// index needs to catch up — calling `append_events` in that case
/// double-writes every event and breaks resume.
///
/// `new_event_count` is the number of events landed since the last
/// index update. `usage_delta` is the cumulative token usage for those
/// events; only `input_tokens`, `output_tokens`, and `cache_read_tokens`
/// are recorded — other [`Usage`] fields are ignored because the index
/// schema does not store them.
///
/// When `new_event_count` is zero AND every relevant Usage field is
/// zero, the call is a no-op that returns `Ok(())` without touching the
/// index. The index entry MUST already exist; a missing entry returns
/// [`SessionPersistError::NotFound`].
pub fn update_session_index(
    data_dir: &Path,
    session_id: &str,
    new_event_count: u64,
    usage_delta: &Usage,
) -> Result<(), SessionPersistError> {
    if new_event_count == 0
        && usage_delta.input_tokens == 0
        && usage_delta.output_tokens == 0
        && usage_delta.cache_read_tokens == 0
    {
        return Ok(());
    }
    update_index_entry(data_dir, session_id, |entry| {
        entry.event_count = entry.event_count.saturating_add(new_event_count);
        entry.updated_at = Utc::now();
        entry.total_input_tokens = entry
            .total_input_tokens
            .saturating_add(usage_delta.input_tokens);
        entry.total_output_tokens = entry
            .total_output_tokens
            .saturating_add(usage_delta.output_tokens);
        entry.total_cache_read_tokens = entry
            .total_cache_read_tokens
            .saturating_add(usage_delta.cache_read_tokens);
    })
}

/// Sum `AssistantMessage` usage fields across `events` into a single
/// [`Usage`]. Non-assistant events contribute zero. Only the three
/// fields the session index tracks (`input_tokens`, `output_tokens`,
/// `cache_read_tokens`) are populated; `cache_write_tokens` and
/// `cost_usd` are left at their defaults.
#[must_use]
pub fn sum_usage_from_events(events: &[SessionEvent]) -> Usage {
    let mut total = Usage::default();
    for event in events {
        if let SessionEvent::AssistantMessage { usage, .. } = event {
            total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
            total.cache_read_tokens = total
                .cache_read_tokens
                .saturating_add(usage.cache_read_tokens);
        }
    }
    total
}

/// Read every [`SessionIndexEntry`] from `{data_dir}/index.jsonl`.
///
/// Returns an empty vector when the file does not exist. Empty lines are
/// skipped. Parse failures include the 1-based line number.
pub fn read_index(data_dir: &Path) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let path = index_file_path(data_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionIndexEntry =
            serde_json::from_str(&line).map_err(|source| SessionPersistError::Parse {
                line: idx + 1,
                source,
            })?;
        out.push(entry);
    }
    Ok(out)
}

/// Atomically rewrite `{data_dir}/index.jsonl` with `entries`.
///
/// Writes go to a unique `index.jsonl.tmp.UUID` file first, are flushed
/// and `fsync`-ed, then renamed over the canonical path. On any failure
/// between write and rename the tmp file is removed before the original
/// error is propagated, so a partial write never leaves a stale `.tmp`
/// behind.
pub fn write_index_atomic(
    data_dir: &Path,
    entries: &[SessionIndexEntry],
) -> Result<(), SessionPersistError> {
    fs::create_dir_all(data_dir)?;
    let final_path = index_file_path(data_dir);
    let tmp_path = data_dir.join(format!("index.jsonl.tmp.{}", Uuid::new_v4()));

    if let Err(err) = write_jsonl_atomic(&tmp_path, entries) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp_path, &final_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(SessionPersistError::Io(err));
    }
    Ok(())
}

fn write_jsonl_atomic<T: Serialize>(
    tmp_path: &Path,
    rows: &[T],
) -> Result<(), SessionPersistError> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(tmp_path)?;
    let mut writer = BufWriter::new(file);
    for row in rows {
        serde_json::to_writer(&mut writer, row)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    Ok(())
}

/// Append `entry` to the session index using direct append mode.
pub fn append_index_entry(
    data_dir: &Path,
    entry: SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    fs::create_dir_all(data_dir)?;
    let path = index_file_path(data_dir);
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, &entry)?;
    drop(entry);
    writer.write_all(b"\n")?;
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    Ok(())
}

/// Mutate the index entry matching `session_id` via `mutator`, then
/// rewrite the index atomically. Returns [`SessionPersistError::NotFound`]
/// when no entry matches.
pub fn update_index_entry(
    data_dir: &Path,
    session_id: &str,
    mutator: impl FnOnce(&mut SessionIndexEntry),
) -> Result<(), SessionPersistError> {
    let mut entries = read_index(data_dir)?;
    let pos = entries
        .iter()
        .position(|e| e.id == session_id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: session_id.to_owned(),
        })?;
    mutator(&mut entries[pos]);
    write_index_atomic(data_dir, &entries)
}

/// Resolve a user-supplied identifier (empty = latest, full ID, name, or
/// >=8-character ID prefix) against the index.
pub fn resolve_session(
    data_dir: &Path,
    input: &str,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let entries = read_index(data_dir)?;
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return entries
            .into_iter()
            .max_by_key(|e| e.updated_at)
            .ok_or_else(|| SessionPersistError::NotFound {
                input: "<no sessions>".to_owned(),
            });
    }

    if let Some(entry) = entries.iter().find(|e| e.id == trimmed) {
        return Ok(entry.clone());
    }
    if let Some(entry) = entries.iter().find(|e| e.name.as_deref() == Some(trimmed)) {
        return Ok(entry.clone());
    }

    if trimmed.len() < 8 {
        return Err(SessionPersistError::NotFound {
            input: trimmed.to_owned(),
        });
    }

    let matches: Vec<&SessionIndexEntry> = entries
        .iter()
        .filter(|e| e.id.starts_with(trimmed))
        .collect();
    match matches.as_slice() {
        [] => Err(SessionPersistError::NotFound {
            input: trimmed.to_owned(),
        }),
        [only] => Ok((*only).clone()),
        many => Err(SessionPersistError::AmbiguousPrefix {
            prefix: trimmed.to_owned(),
            matches: many.iter().map(|e| e.id.clone()).collect(),
        }),
    }
}

/// Remove the entry with `session_id` from the index. Returns
/// [`SessionPersistError::NotFound`] when no entry matches.
pub fn remove_index_entry(data_dir: &Path, session_id: &str) -> Result<(), SessionPersistError> {
    let mut entries = read_index(data_dir)?;
    let pos = entries
        .iter()
        .position(|e| e.id == session_id)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: session_id.to_owned(),
        })?;
    entries.remove(pos);
    write_index_atomic(data_dir, &entries)
}
