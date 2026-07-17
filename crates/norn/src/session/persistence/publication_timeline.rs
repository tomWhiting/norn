use std::io::{BufReader, BufWriter, Write as _};
use std::path::Path;

use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::util::PrivateRoot;

use super::super::super::strict::{StrictFormatHeader, visit_strict_event_file};
use super::publication_hash::HashingReader;
use super::publication_recovery::remove_owned_after_failure;
use super::publication_timeline_error::map_publication_timeline_error;
use super::{SessionIndexEntry, SessionPersistError};

#[derive(Debug)]
pub(super) struct TimelineFacts {
    pub(super) bytes: u64,
    pub(super) sha256: String,
    pub(super) event_count: u64,
    pub(super) usage: Usage,
}

pub(super) fn write_timeline_stage(
    root: &PrivateRoot,
    stage_path: &Path,
    events: &[SessionEvent],
    session_id: &str,
) -> Result<TimelineFacts, SessionPersistError> {
    let result = (|| {
        let file = root.create_new(stage_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &StrictFormatHeader::current())?;
        writer.write_all(b"\n")?;
        for event in events {
            serde_json::to_writer(&mut writer, event)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        let file = writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
        root.sync_dir(Path::new(""))?;
        inspect_timeline(root, stage_path, session_id)
    })();
    if result.is_err() {
        remove_owned_after_failure(root, stage_path);
    }
    result
}

pub(super) fn inspect_if_present(
    root: &PrivateRoot,
    path: &Path,
    session_id: &str,
) -> Result<Option<TimelineFacts>, SessionPersistError> {
    match inspect_timeline(root, path, session_id) {
        Ok(facts) => Ok(Some(facts)),
        Err(SessionPersistError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn inspect_timeline(
    root: &PrivateRoot,
    path: &Path,
    session_id: &str,
) -> Result<TimelineFacts, SessionPersistError> {
    let file = root.open_read(path)?;
    let initial_length = file.metadata()?.len();
    let mut reader = HashingReader::new(file);
    let (_, _, counters) =
        visit_strict_event_file(BufReader::new(&mut reader), &root.display_path(path), drop)
            .map_err(|error| map_publication_timeline_error(error, session_id))?;
    let usage = counters.tracked_usage();
    let final_length = reader.metadata_len()?;
    if initial_length != final_length || reader.bytes_read() != final_length {
        return Err(std::io::Error::other("timeline changed while it was being validated").into());
    }
    Ok(TimelineFacts {
        bytes: final_length,
        sha256: reader.finish_sha256(),
        event_count: counters.event_count,
        usage,
    })
}

pub(super) fn apply_timeline_facts(entry: &mut SessionIndexEntry, facts: &TimelineFacts) {
    entry.event_count = facts.event_count;
    entry.total_input_tokens = facts.usage.input_tokens;
    entry.total_output_tokens = facts.usage.output_tokens;
    entry.total_cache_read_tokens = facts.usage.cache_read_tokens;
}
