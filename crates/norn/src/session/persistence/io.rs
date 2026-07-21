//! Session JSONL path validation and retry-safe batch append.
//!
//! Timeline creation and crash recovery live in [`super::timeline_file`];
//! index maintenance lives in [`super::index`].

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use crate::session::events::SessionEvent;
use crate::util::validate_private_component;

#[cfg(test)]
use super::index::append_events_transaction;
use super::strict::visit_strict_event_file;
use super::strict_runtime::map_strict_error;
use super::types::{SessionIndexEntry, SessionPersistError};

pub(crate) use super::timeline_file::{ExistingEventInspection, ExistingEventState};
#[cfg(test)]
pub(crate) use super::timeline_file::{open_session_append, open_session_append_bound};

#[cfg(test)]
pub(crate) use super::event_reader::read_session_events;
pub use super::event_reader::read_session_events_for_entry;
#[cfg(test)]
pub(crate) use super::event_reader::read_session_events_from;

/// Return the flat JSONL file path for `session_id` under `data_dir`.
#[cfg(test)]
pub(crate) fn session_file_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join(format!("{session_id}.jsonl"))
}

/// Resolve an index entry's validated timeline path for tests.
#[cfg(test)]
pub(crate) fn resolved_session_file_path(data_dir: &Path, entry: &SessionIndexEntry) -> PathBuf {
    entry.rel_path.as_ref().map_or_else(
        || session_file_path(data_dir, &entry.id),
        |relative| data_dir.join(relative),
    )
}

/// Name stems reserved for persistence-owned files.
pub const RESERVED_SESSION_ID_STEMS: &[&str] = &["index"];

/// Return whether an identifier collides with a persistence-owned name family.
#[must_use]
pub fn is_reserved_session_id(id: &str) -> bool {
    RESERVED_SESSION_ID_STEMS.iter().any(|stem| {
        let Some(head) = id.get(..stem.len()) else {
            return false;
        };
        let rest = &id[stem.len()..];
        head.eq_ignore_ascii_case(stem) && (rest.is_empty() || rest.starts_with('.'))
    })
}

pub(crate) fn ensure_session_id_not_reserved(id: &str) -> Result<(), SessionPersistError> {
    if is_reserved_session_id(id) {
        return Err(SessionPersistError::InvalidSessionId {
            id: id.to_owned(),
            reason: format!(
                "collides with persistence-owned name stems: {}",
                RESERVED_SESSION_ID_STEMS.join(", "),
            ),
        });
    }
    Ok(())
}

pub(crate) fn ensure_session_id_path_safe(id: &str) -> Result<(), SessionPersistError> {
    ensure_session_id_not_reserved(id)?;
    let invalid = |reason: &str| SessionPersistError::InvalidSessionId {
        id: id.to_owned(),
        reason: reason.to_owned(),
    };
    validate_private_component(id, "session id").map_err(|error| invalid(&error.to_string()))?;
    let Some(first) = id.chars().next() else {
        return Err(invalid("must not be empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(invalid("must start with an ASCII letter or digit"));
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(invalid(
            "may contain only ASCII letters, digits, '-', '_', and '.'",
        ));
    }
    Ok(())
}

pub(crate) fn session_file_relative(
    entry: &SessionIndexEntry,
) -> Result<PathBuf, SessionPersistError> {
    ensure_session_id_path_safe(&entry.id)?;
    let Some(relative) = entry.rel_path.as_deref() else {
        return Ok(PathBuf::from(format!("{}.jsonl", entry.id)));
    };
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let valid = matches!(components.as_slice(), [
        std::path::Component::Normal(root),
        std::path::Component::Normal(children),
        std::path::Component::Normal(file),
    ] if children == &std::ffi::OsStr::new("children")
        && path_component_is_safe(root)
        && path_component_is_safe(file)
        && Path::new(file).extension().is_some_and(|extension| extension == "jsonl"));
    if !valid {
        return Err(SessionPersistError::InvalidSessionId {
            id: entry.id.clone(),
            reason: "indexed rel_path must have the safe '<root>/children/<file>.jsonl' shape"
                .to_owned(),
        });
    }
    Ok(PathBuf::from(relative))
}

fn path_component_is_safe(component: &std::ffi::OsStr) -> bool {
    component
        .to_str()
        .is_some_and(|value| validate_private_component(value, "session path component").is_ok())
}

/// Append one durable batch after validating the existing strict timeline.
///
/// An error before the write leaves the file unchanged. If a prior attempt
/// wrote a complete prefix of this exact batch before reporting failure, a
/// retry recognises that suffix and writes only the remaining events. Index
/// maintenance remains best-effort after the timeline is durable.
#[cfg(test)]
pub(crate) fn append_events(
    data_dir: &Path,
    session_id: &str,
    events: &[SessionEvent],
    disabled: bool,
) -> Result<(), SessionPersistError> {
    if disabled || events.is_empty() {
        return Ok(());
    }
    append_events_transaction(data_dir, session_id, events)
}

pub(crate) fn serialize_events(events: &[SessionEvent]) -> Result<Vec<u8>, SessionPersistError> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

#[cfg(test)]
pub(super) fn retry_prefix_len(
    existing: &[SessionEvent],
    requested: &[SessionEvent],
) -> Result<usize, SessionPersistError> {
    let mut planner = RetryPlanner::new(requested)?;
    for event in existing {
        planner.observe(event);
    }
    planner.finish()
}

pub(crate) fn retry_prefix_from_file(
    file: &mut File,
    display_path: &Path,
    requested: &[SessionEvent],
) -> Result<TimelineAppendFacts, SessionPersistError> {
    let mut planner = RetryPlanner::new(requested)?;
    let mut tail = None;
    file.seek(SeekFrom::Start(0))?;
    let (_, _, counters) = visit_strict_event_file(BufReader::new(file), display_path, |event| {
        planner.observe(&event);
        tail = Some(event);
    })
    .map_err(map_strict_error)?;
    Ok(TimelineAppendFacts {
        retry_prefix: planner.finish()?,
        counters,
        tail,
    })
}

pub(crate) fn strict_events_from_file(
    file: &mut File,
    display_path: &Path,
) -> Result<Vec<SessionEvent>, SessionPersistError> {
    let mut events = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    visit_strict_event_file(BufReader::new(file), display_path, |event| {
        events.push(event);
    })
    .map_err(map_strict_error)?;
    Ok(events)
}

pub(crate) struct TimelineAppendFacts {
    pub(crate) retry_prefix: usize,
    pub(crate) counters: super::IndexCounters,
    pub(crate) tail: Option<SessionEvent>,
}

struct RetryPlanner {
    requested_ids: HashSet<String>,
    requested_values: Vec<serde_json::Value>,
    first_requested_id: Option<String>,
    matched: usize,
    matching: bool,
    conflict: Option<&'static str>,
}

impl RetryPlanner {
    fn new(requested: &[SessionEvent]) -> Result<Self, SessionPersistError> {
        crate::session::validate_new_response_publication_batches(requested)?;
        let mut requested_ids = HashSet::new();
        let mut requested_values = Vec::with_capacity(requested.len());
        for event in requested {
            if !requested_ids.insert(event.base().id.to_string()) {
                return Err(invalid_data("append batch contains a duplicate event id"));
            }
            requested_values.push(serde_json::to_value(event)?);
        }
        Ok(Self {
            requested_ids,
            requested_values,
            first_requested_id: requested.first().map(|event| event.base().id.to_string()),
            matched: 0,
            matching: false,
            conflict: None,
        })
    }

    fn observe(&mut self, event: &SessionEvent) {
        if self.conflict.is_some() {
            return;
        }
        let event_id = event.base().id.as_str();
        if !self.matching {
            if self.first_requested_id.as_deref() == Some(event_id) {
                self.matching = true;
            } else if self.requested_ids.contains(event_id) {
                self.conflict = Some("append batch reuses an event id outside a retryable suffix");
                return;
            } else {
                return;
            }
        }
        let Some(expected) = self.requested_values.get(self.matched) else {
            self.conflict =
                Some("append batch matches an earlier event but not the timeline suffix");
            return;
        };
        if serde_json::to_value(event).ok().as_ref() != Some(expected) {
            self.conflict = Some("append retry changed an event that is already durable");
            return;
        }
        match self.matched.checked_add(1) {
            Some(matched) => self.matched = matched,
            None => self.conflict = Some("append retry prefix length is not representable"),
        }
    }

    fn finish(self) -> Result<usize, SessionPersistError> {
        match self.conflict {
            Some(reason) => Err(invalid_data(reason)),
            None => Ok(self.matched),
        }
    }
}

fn invalid_data(reason: &str) -> SessionPersistError {
    SessionPersistError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, reason))
}
