use std::collections::HashSet;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use super::{
    SessionIndexEntry, StrictEventFile, StrictIndexFile, StrictStoreError, read_strict_event_file,
    read_strict_index_file,
};
use crate::session::persistence::IndexCounters;
use crate::util::PrivateRoot;

const INDEX_FILE: &str = "index.jsonl";

/// One manifest entry paired with its validated strict timeline.
#[derive(Clone, Debug)]
pub struct ValidatedStrictSession {
    /// Exact index row for the session.
    pub index_entry: SessionIndexEntry,
    /// Losslessly decoded format-2 event timeline.
    pub timeline: StrictEventFile,
}

/// An observationally validated staged strict store.
#[derive(Clone, Debug)]
pub struct ValidatedStrictStore {
    /// Strictly decoded manifest.
    pub index: StrictIndexFile,
    /// Timelines in manifest order.
    pub sessions: Vec<ValidatedStrictSession>,
}

/// Validate a complete staged store without creating, repairing, or chmodding it.
///
/// The validator reads `index.jsonl` and every named strict timeline. Every
/// open is descriptor-relative, refuses links, and uses the observational
/// private-filesystem entry point. Inspect/export-only migration records live
/// in the separately validated migration manifest and never enter this index.
pub fn validate_staged_store(root: &Path) -> Result<ValidatedStrictStore, StrictStoreError> {
    let descriptor_permit = super::super::acquire_private_fs().map_err(|error| {
        StrictStoreError::DescriptorAdmission {
            reason: error.to_string(),
        }
    })?;
    let index_path = root.join(INDEX_FILE);
    let index_file = PrivateRoot::open_read_observational(root, Path::new(INDEX_FILE))
        .map_err(|error| StrictStoreError::io(&index_path, error))?;
    let index = read_strict_index_file(BufReader::new(index_file), &index_path)?;
    let mut paths = HashSet::new();
    let mut sessions = Vec::with_capacity(index.entries.len());
    for (offset, entry) in index.entries.iter().enumerate() {
        let line = offset.saturating_add(2);
        let relative = record_path(entry, line)?;
        if !paths.insert(relative.clone()) {
            return Err(StrictStoreError::DuplicateSessionPath { path: relative });
        }
        let display_path = root.join(&relative);
        let file = PrivateRoot::open_read_observational(root, &relative)
            .map_err(|error| StrictStoreError::io(&display_path, error))?;
        let timeline = read_strict_event_file(BufReader::new(file), &display_path)?;
        crate::session::validate_provider_state_provenance(&timeline.events).map_err(|source| {
            StrictStoreError::InvalidProviderState {
                path: display_path.clone(),
                source,
            }
        })?;
        validate_counts(entry, &timeline, &display_path)?;
        sessions.push(ValidatedStrictSession {
            index_entry: entry.clone(),
            timeline,
        });
    }
    drop(descriptor_permit);
    Ok(ValidatedStrictStore { index, sessions })
}

fn record_path(entry: &SessionIndexEntry, line: usize) -> Result<PathBuf, StrictStoreError> {
    super::super::io::session_file_relative(entry).map_err(|error| {
        StrictStoreError::InvalidIndexEntry {
            line,
            reason: error.to_string(),
        }
    })
}

fn validate_counts(
    entry: &SessionIndexEntry,
    timeline: &StrictEventFile,
    display_path: &Path,
) -> Result<(), StrictStoreError> {
    let counters = IndexCounters::try_from_events(&timeline.events).map_err(|overflow| {
        StrictStoreError::IndexCounterOverflow {
            path: display_path.to_path_buf(),
            field: overflow.field(),
        }
    })?;
    if counters.event_count != entry.event_count {
        return Err(StrictStoreError::EventCountMismatch {
            session_id: entry.id.clone(),
            indexed: entry.event_count,
            actual: counters.event_count,
        });
    }
    if counters.total_input_tokens != entry.total_input_tokens
        || counters.total_output_tokens != entry.total_output_tokens
        || counters.total_cache_read_tokens != entry.total_cache_read_tokens
    {
        return Err(StrictStoreError::UsageMismatch {
            session_id: entry.id.clone(),
        });
    }
    Ok(())
}
