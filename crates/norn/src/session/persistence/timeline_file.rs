//! Strict timeline creation, validation, and bounded crash-tail recovery.

use std::fs::File;
#[cfg(test)]
use std::io::Write as _;
use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde::Deserialize as _;

use crate::util::{PrivateFileIdentity, PrivateRoot};

use super::IndexCounters;
#[cfg(test)]
use super::strict::StrictFormatHeader;
use super::strict::{
    StrictStoreError, read_strict_event_file, validate_strict_event_file, visit_strict_event_file,
};
use super::strict_runtime::map_strict_error;
#[cfg(test)]
use super::timeline_lock::{LockedTimelineFile, TimelineTransaction};
use super::types::SessionPersistError;

const REVERSE_SCAN_CHUNK_BYTES: usize = 8 * 1024;

/// How an event about to be appended relates to the validated durable history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExistingEventState {
    /// The event id is absent and the event can be appended.
    Absent,
    /// The exact event is already the final durable row.
    ExactTail,
    /// The exact event exists but is not the final row.
    ExactNotTail,
    /// The event id exists with different content.
    ConflictingId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExistingEventInspection {
    pub(crate) state: ExistingEventState,
    pub(crate) counters: IndexCounters,
}

#[cfg(test)]
pub(crate) fn open_session_append(path: &Path) -> Result<LockedTimelineFile, SessionPersistError> {
    let (data_dir, relative) = direct_timeline_location(path)?;
    let transaction = TimelineTransaction::create(data_dir, relative)?;
    let file = open_session_append_under(transaction.root(), relative)?;
    Ok(LockedTimelineFile::new(file, transaction))
}

#[cfg(test)]
pub(crate) fn open_session_append_bound(
    path: &Path,
    identity: PrivateFileIdentity,
    candidate_id: &str,
    candidate_line: &[u8],
) -> Result<(LockedTimelineFile, ExistingEventInspection), SessionPersistError> {
    let (data_dir, relative) = direct_timeline_location(path)?;
    let transaction = TimelineTransaction::open(data_dir, relative)?;
    let (file, inspection) = open_session_append_bound_under(
        transaction.root(),
        relative,
        identity,
        candidate_id,
        candidate_line,
    )?;
    Ok((LockedTimelineFile::new(file, transaction), inspection))
}

pub(super) fn open_session_append_bound_under(
    root: &PrivateRoot,
    relative: &Path,
    identity: PrivateFileIdentity,
    candidate_id: &str,
    candidate_line: &[u8],
) -> Result<(File, ExistingEventInspection), SessionPersistError> {
    let mut file = root.open_read_append(relative)?;
    identity.verify(&file)?;
    let inspection = recover_tail_then_inspect(
        &mut file,
        &root.display_path(relative),
        candidate_id,
        candidate_line,
    )?;
    Ok((file, inspection))
}

#[cfg(test)]
pub(super) fn open_session_append_under(
    root: &PrivateRoot,
    relative: &Path,
) -> Result<File, SessionPersistError> {
    if let Some(parent) = relative.parent() {
        root.create_dir_all(parent)?;
    }
    match open_existing_for_append(root, relative) {
        Ok(file) => return Ok(file),
        Err(SessionPersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match stamp_header_atomically(root, relative) {
        Ok(()) => Ok(root.open_read_append(relative)?),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing_for_append(root, relative)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn open_existing_for_append(
    root: &PrivateRoot,
    relative: &Path,
) -> Result<File, SessionPersistError> {
    let mut file = root.open_read_append(relative)?;
    recover_tail_then_validate(&mut file, &root.display_path(relative))?;
    Ok(file)
}

pub(super) fn open_existing_timeline(
    root: &PrivateRoot,
    relative: &Path,
) -> Result<Vec<crate::session::events::SessionEvent>, SessionPersistError> {
    let mut file = root.open_read_append(relative)?;
    recover_incomplete_tail(&mut file, &root.display_path(relative))?;
    file.seek(SeekFrom::Start(0))?;
    let timeline = read_strict_event_file(BufReader::new(&mut file), &root.display_path(relative))
        .map_err(map_strict_error)?;
    Ok(timeline.events)
}

#[cfg(test)]
fn direct_timeline_location(path: &Path) -> Result<(&Path, &Path), SessionPersistError> {
    let parent = absolute_parent(path)?;
    let relative = final_component(path)?;
    Ok((parent, relative))
}

#[cfg(test)]
fn absolute_parent(path: &Path) -> Result<&Path, SessionPersistError> {
    if !path.is_absolute() {
        return Err(SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file path must be absolute",
        )));
    }
    path.parent().ok_or_else(|| {
        SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file must have an absolute parent directory",
        ))
    })
}

#[cfg(test)]
fn final_component(path: &Path) -> Result<&Path, SessionPersistError> {
    path.file_name().map(Path::new).ok_or_else(|| {
        SessionPersistError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session file path has no final component",
        ))
    })
}

fn recover_tail_then_validate(
    file: &mut File,
    display_path: &Path,
) -> Result<(), SessionPersistError> {
    recover_incomplete_tail(file, display_path)?;
    file.seek(SeekFrom::Start(0))?;
    validate_strict_event_file(BufReader::new(file), display_path)
        .map(drop)
        .map_err(map_strict_error)
}

fn recover_tail_then_inspect(
    file: &mut File,
    display_path: &Path,
    candidate_id: &str,
    candidate_line: &[u8],
) -> Result<ExistingEventInspection, SessionPersistError> {
    recover_incomplete_tail(file, display_path)?;
    file.seek(SeekFrom::Start(0))?;
    let mut ordinal = 0_usize;
    let mut matched = None;
    let mut encoding_error = None;
    let (_, event_count, counters) =
        visit_strict_event_file(BufReader::new(&mut *file), display_path, |event| {
            ordinal = ordinal.saturating_add(1);
            if event.base().id.as_str() != candidate_id {
                return;
            }
            match serde_json::to_vec(&event) {
                Ok(mut encoded) => {
                    encoded.push(b'\n');
                    matched = Some((ordinal, encoded == candidate_line));
                }
                Err(error) => encoding_error = Some(error),
            }
        })
        .map_err(map_strict_error)?;
    if let Some(error) = encoding_error {
        return Err(SessionPersistError::Serde(error));
    }
    let state = match matched {
        None => ExistingEventState::Absent,
        Some((_, false)) => ExistingEventState::ConflictingId,
        Some((position, true)) if position == event_count => ExistingEventState::ExactTail,
        Some((_, true)) => ExistingEventState::ExactNotTail,
    };
    Ok(ExistingEventInspection { state, counters })
}

fn recover_incomplete_tail(
    file: &mut File,
    display_path: &Path,
) -> Result<(), SessionPersistError> {
    let length = file.metadata()?.len();
    if length == 0 || final_byte_is_newline(file, length)? {
        return Ok(());
    }
    let frame_start = find_final_frame_start(file, length)?;
    let retained_events = validate_prefix(file, frame_start, display_path)?;
    let line = retained_events.saturating_add(2);
    let tail_error = decode_unterminated_frame(file, frame_start, length);
    match tail_error {
        Err(error) if error.is_eof() => {
            file.set_len(frame_start)?;
            file.sync_all()?;
            tracing::warn!(
                path = %display_path.display(),
                line,
                removed_bytes = length.saturating_sub(frame_start),
                "discarded a provably incomplete final session event row",
            );
            Ok(())
        }
        Err(error) => Err(map_strict_error(StrictStoreError::InvalidJson {
            path: display_path.to_path_buf(),
            line,
            reason: error.to_string(),
        })),
        Ok(()) => Err(map_strict_error(StrictStoreError::TornTail {
            path: display_path.to_path_buf(),
            line,
        })),
    }
}

fn final_byte_is_newline(file: &mut File, length: u64) -> std::io::Result<bool> {
    file.seek(SeekFrom::Start(length.saturating_sub(1)))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

fn find_final_frame_start(file: &mut File, length: u64) -> std::io::Result<u64> {
    let mut cursor = length;
    let mut chunk = [0_u8; REVERSE_SCAN_CHUNK_BYTES];
    let chunk_capacity = u64::try_from(chunk.len()).map_err(std::io::Error::other)?;
    while cursor > 0 {
        let read_len =
            usize::try_from(cursor.min(chunk_capacity)).map_err(std::io::Error::other)?;
        cursor = cursor.saturating_sub(u64::try_from(read_len).map_err(std::io::Error::other)?);
        file.seek(SeekFrom::Start(cursor))?;
        file.read_exact(&mut chunk[..read_len])?;
        if let Some(position) = chunk[..read_len].iter().rposition(|byte| *byte == b'\n') {
            return cursor
                .checked_add(u64::try_from(position).map_err(std::io::Error::other)?)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| std::io::Error::other("session frame offset overflow"));
        }
    }
    Ok(0)
}

fn validate_prefix(
    file: &mut File,
    prefix_len: u64,
    display_path: &Path,
) -> Result<usize, SessionPersistError> {
    file.seek(SeekFrom::Start(0))?;
    validate_strict_event_file(BufReader::new(file.take(prefix_len)), display_path)
        .map_err(map_strict_error)
}

fn decode_unterminated_frame(
    file: &mut File,
    start: u64,
    length: u64,
) -> Result<(), serde_json::Error> {
    file.seek(SeekFrom::Start(start))
        .map_err(serde_json::Error::io)?;
    let frame_len = length.saturating_sub(start);
    let mut deserializer = serde_json::Deserializer::from_reader(file.take(frame_len));
    serde_json::Value::deserialize(&mut deserializer)?;
    deserializer.end()
}

#[cfg(test)]
fn stamp_header_atomically(root: &PrivateRoot, path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session file path has no valid final component",
            )
        })?;
    let temporary = parent.join(format!("{file_name}.tmp.{}", uuid::Uuid::new_v4()));
    let mut header =
        serde_json::to_vec(&StrictFormatHeader::current()).map_err(std::io::Error::other)?;
    header.push(b'\n');

    let write_result = (|| -> std::io::Result<()> {
        let mut file = root.create_new(&temporary)?;
        file.write_all(&header)?;
        file.sync_all()
    })();
    let publish_result = write_result.and_then(|()| root.publish_new(&temporary, path));
    if let Err(error) = root.remove_file(&temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %root.display_path(&temporary).display(),
            %error,
            "failed to remove an inert session-header temporary file",
        );
    }
    publish_result.and_then(|()| root.sync_dir(parent))
}
