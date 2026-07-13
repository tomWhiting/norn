//! Single-pass tolerant reading of persisted session event streams.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::session::events::{EventId, SessionEvent};
use crate::util::PrivateRoot;

use super::acquire_private_fs;
use super::io::{
    ensure_session_id_not_reserved, ensure_session_id_path_safe, session_file_relative,
};
use super::replay::ReplayArtifacts;
use super::types::{
    SESSION_FORMAT_VERSION, SessionFileHeader, SessionIndexEntry, SessionPersistError,
};

/// Tolerantly read a flat session JSONL file in one pass.
pub fn read_session_events(
    data_dir: &Path,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    ensure_session_id_not_reserved(session_id)?;
    ensure_session_id_path_safe(session_id)?;
    let relative = PathBuf::from(format!("{session_id}.jsonl"));
    read_session_events_at(data_dir, &relative, session_id)
}

/// Read a session file resolved through its registered relative path.
pub fn read_session_events_for_entry(
    data_dir: &Path,
    entry: &SessionIndexEntry,
) -> Result<ReplayArtifacts, SessionPersistError> {
    ensure_session_id_not_reserved(&entry.id)?;
    let relative = session_file_relative(entry)?;
    read_session_events_at(data_dir, &relative, &entry.id)
}

fn read_session_events_at(
    data_dir: &Path,
    relative: &Path,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let _permit = acquire_private_fs()?;
    let root = match PrivateRoot::open(data_dir) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplayArtifacts::default());
        }
        Err(error) => return Err(error.into()),
    };
    let file = match root.open_read(relative) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplayArtifacts::default());
        }
        Err(error) => return Err(error.into()),
    };
    read_session_events_from(BufReader::new(file), session_id)
}

pub(crate) fn read_session_events_from<R: BufRead>(
    reader: R,
    session_id: &str,
) -> Result<ReplayArtifacts, SessionPersistError> {
    let mut artifacts = ReplayArtifacts::default();
    let mut seen_first_content_line = false;
    let mut seen_ids: HashSet<EventId> = HashSet::new();
    for (idx, raw) in reader.split(b'\n').enumerate() {
        let raw = raw?;
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !seen_first_content_line {
            seen_first_content_line = true;
            if let Ok(header) = serde_json::from_slice::<SessionFileHeader>(&raw) {
                artifacts.format_version = Some(header.version);
                if header.version > SESSION_FORMAT_VERSION {
                    tracing::warn!(
                        session_id,
                        file_version = header.version,
                        reader_version = SESSION_FORMAT_VERSION,
                        "session file written by a newer norn; unknown events will be skipped",
                    );
                }
                continue;
            }
        }
        match serde_json::from_slice::<SessionEvent>(&raw) {
            Ok(event) => {
                let id = event.base().id.clone();
                if seen_ids.insert(id.clone()) {
                    artifacts.push(event);
                } else {
                    artifacts.skipped_lines = artifacts.skipped_lines.saturating_add(1);
                    tracing::warn!(
                        session_id,
                        line = idx + 1,
                        event_id = %id,
                        "skipping duplicate session event line; first occurrence kept",
                    );
                }
            }
            Err(error) => {
                artifacts.skipped_lines = artifacts.skipped_lines.saturating_add(1);
                tracing::warn!(
                    session_id,
                    line = idx + 1,
                    %error,
                    "skipping corrupt or unknown session event line",
                );
            }
        }
    }
    if artifacts.skipped_lines > 0 {
        tracing::warn!(
            session_id,
            skipped = artifacts.skipped_lines,
            recovered = artifacts.events.len(),
            "session file contained skipped lines; recovered events were loaded",
        );
    }
    Ok(artifacts)
}
