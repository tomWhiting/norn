//! Fail-closed reading of active format-2 session timelines.

#[cfg(test)]
use std::io::BufRead;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::io::ensure_session_id_not_reserved;
#[cfg(test)]
use super::io::ensure_session_id_path_safe;
use super::replay::ReplayArtifacts;
use super::strict::STRICT_SESSION_FORMAT_VERSION;
#[cfg(test)]
use super::strict::read_strict_event_file;
use super::strict_runtime::map_strict_error;
use super::timeline_file::open_existing_timeline;
#[cfg(test)]
use super::timeline_lock::TimelineTransaction;
use super::types::{SessionIndexEntry, SessionPersistError};

/// Strictly read a flat format-2 session timeline.
#[cfg(test)]
pub(crate) fn read_session_events(
    data_dir: &Path,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    ensure_session_id_not_reserved(session_id)?;
    ensure_session_id_path_safe(session_id)?;
    let relative = PathBuf::from(format!("{session_id}.jsonl"));
    read_session_events_at(data_dir, &relative, None)
}

/// Strictly read a format-2 timeline resolved through its registered path.
pub fn read_session_events_for_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
) -> Result<ReplayArtifacts, SessionPersistError> {
    read_session_events_for_entry_with_deadline(data_dir, entry, None)
}

pub(crate) fn read_session_events_for_entry_with_deadline(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    lock_deadline: Option<std::time::Duration>,
) -> Result<ReplayArtifacts, SessionPersistError> {
    ensure_session_id_not_reserved(&entry.id)?;
    super::index::with_registered_timeline(data_dir, entry, lock_deadline, |root, relative| {
        let events = match open_existing_timeline(root, relative) {
            Ok(events) => events,
            Err(SessionPersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return missing_timeline(Some(&entry.id));
            }
            Err(error) => return Err(error),
        };
        replay_from_events(events, &root.display_path(relative))
    })
}

#[cfg(test)]
fn read_session_events_at(
    data_dir: &Path,
    relative: &Path,
    registered_id: Option<&str>,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let transaction = match TimelineTransaction::open(data_dir, relative) {
        Ok(transaction) => transaction,
        Err(SessionPersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return missing_timeline(registered_id);
        }
        Err(error) => return Err(error),
    };
    let events = match open_existing_timeline(transaction.root(), relative) {
        Ok(events) => events,
        Err(SessionPersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return missing_timeline(registered_id);
        }
        Err(error) => return Err(error),
    };
    drop(transaction);
    replay_from_events(events, &data_dir.join(relative))
}

fn replay_from_events(
    events: Vec<crate::session::events::SessionEvent>,
    display_path: &Path,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let mut artifacts =
        ReplayArtifacts::from_strict_events(events, display_path).map_err(map_strict_error)?;
    crate::session::validate_provider_state_provenance(&artifacts.events)?;
    artifacts.format_version = Some(STRICT_SESSION_FORMAT_VERSION);
    Ok(artifacts)
}

fn missing_timeline(registered_id: Option<&str>) -> Result<ReplayArtifacts, SessionPersistError> {
    match registered_id {
        Some(id) => Err(SessionPersistError::NotFound {
            input: id.to_owned(),
        }),
        None => Ok(ReplayArtifacts::default()),
    }
}

#[cfg(test)]
pub(crate) fn read_session_events_from<R: BufRead>(
    reader: R,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let display_path = PathBuf::from(format!("{session_id}.jsonl"));
    read_session_events_from_path(reader, &display_path)
}

#[cfg(test)]
fn read_session_events_from_path<R: BufRead>(
    reader: R,
    display_path: &Path,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let timeline = read_strict_event_file(reader, display_path).map_err(map_strict_error)?;
    let mut artifacts = ReplayArtifacts::from_strict_events(timeline.events, display_path)
        .map_err(map_strict_error)?;
    artifacts.format_version = Some(STRICT_SESSION_FORMAT_VERSION);
    Ok(artifacts)
}
