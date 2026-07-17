use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use uuid::Version;

use super::{ResumeFidelity, SessionIndexEntry, SessionRecordOrigin, StrictStoreError};
use crate::session::persistence::SESSION_FORMAT_VERSION;

pub(super) fn validate_index_entry(
    entry: &SessionIndexEntry,
    line: usize,
) -> Result<(), StrictStoreError> {
    if entry.format_version != SESSION_FORMAT_VERSION {
        return invalid_index(
            line,
            &format!(
                "format_version must be {SESSION_FORMAT_VERSION}, found {}",
                entry.format_version
            ),
        );
    }
    if entry.generation.get_version() != Some(Version::Random) {
        return invalid_index(line, "generation must be a random UUID v4");
    }
    super::super::io::ensure_session_id_path_safe(&entry.id).map_err(|error| {
        StrictStoreError::InvalidIndexEntry {
            line,
            reason: error.to_string(),
        }
    })?;
    if !is_normalized_absolute_path(&entry.working_dir) {
        return invalid_index(line, "working_dir must be an absolute normalized path");
    }
    if matches!(entry.fidelity, ResumeFidelity::InspectOnly) {
        return invalid_index(
            line,
            "inspect-only sources belong in migration-manifest.json, not the active strict index",
        );
    }
    super::super::io::session_file_relative(entry).map_err(|error| {
        StrictStoreError::InvalidIndexEntry {
            line,
            reason: error.to_string(),
        }
    })?;
    if entry.rel_path.is_some() != entry.parent_id.is_some() {
        return invalid_index(
            line,
            "timeline rel_path and parent_id must both be present for a child or absent for a root",
        );
    }
    if let Some(parent_id) = entry.parent_id.as_deref() {
        super::super::io::ensure_session_id_path_safe(parent_id).map_err(|error| {
            StrictStoreError::InvalidIndexEntry {
                line,
                reason: error.to_string(),
            }
        })?;
    }
    validate_origin(entry, line)
}

pub(crate) fn validate_index_entries(
    entries: &[SessionIndexEntry],
) -> Result<(), StrictStoreError> {
    let mut first_lines = HashMap::new();
    for (offset, entry) in entries.iter().enumerate() {
        let line = offset.saturating_add(2);
        validate_index_entry(entry, line)?;
        if let Some(first_line) = first_lines.insert(entry.id.clone(), line) {
            return Err(StrictStoreError::DuplicateSessionId {
                id: entry.id.clone(),
                first_line,
                line,
            });
        }
    }
    validate_index_relationships(entries)
}

fn validate_origin(entry: &SessionIndexEntry, line: usize) -> Result<(), StrictStoreError> {
    let SessionRecordOrigin::MigratedLegacy {
        source_format,
        source_sha256,
    } = &entry.origin
    else {
        return Ok(());
    };
    if *source_format >= SESSION_FORMAT_VERSION {
        return invalid_index(
            line,
            &format!("migrated legacy source format must predate {SESSION_FORMAT_VERSION}"),
        );
    }
    if source_sha256.len() != 64
        || !source_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return invalid_index(
            line,
            "migrated legacy source_sha256 must be 64 lowercase hexadecimal digits",
        );
    }
    Ok(())
}

pub(super) fn validate_index_relationships(
    entries: &[SessionIndexEntry],
) -> Result<(), StrictStoreError> {
    let by_id = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut paths = HashSet::new();
    for (offset, entry) in entries.iter().enumerate() {
        let line = offset.saturating_add(2);
        let relative = super::super::io::session_file_relative(entry).map_err(|error| {
            StrictStoreError::InvalidIndexEntry {
                line,
                reason: error.to_string(),
            }
        })?;
        if !paths.insert(relative.clone()) {
            return Err(StrictStoreError::DuplicateSessionPath { path: relative });
        }
        if let Some(parent_id) = entry.parent_id.as_deref() {
            if parent_id == entry.id {
                return invalid_index(line, "a session cannot be its own parent");
            }
            if !by_id.contains_key(parent_id) {
                return invalid_index(line, "parent_id is not present in the strict index");
            }
        }
    }

    let mut states = vec![RelationshipState::Unvisited; entries.len()];
    for (offset, entry) in entries.iter().enumerate() {
        let root = resolve_root(offset, entries, &by_id, &mut states)?;
        let Some(relative) = entry.rel_path.as_deref() else {
            continue;
        };
        if first_component(relative) != Some(entries[root].id.as_str()) {
            return invalid_index(
                offset.saturating_add(2),
                "child rel_path must be rooted under its ultimate root session id",
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum RelationshipState {
    Unvisited,
    Visiting,
    Complete(usize),
}

fn resolve_root(
    start: usize,
    entries: &[SessionIndexEntry],
    by_id: &HashMap<&str, usize>,
    states: &mut [RelationshipState],
) -> Result<usize, StrictStoreError> {
    if let RelationshipState::Complete(root) = states[start] {
        return Ok(root);
    }
    let mut path = Vec::new();
    let mut current = start;
    let root = loop {
        match states[current] {
            RelationshipState::Complete(root) => break root,
            RelationshipState::Visiting => {
                return invalid_index(start.saturating_add(2), "parent_id chain contains a cycle");
            }
            RelationshipState::Unvisited => {
                states[current] = RelationshipState::Visiting;
                path.push(current);
            }
        }
        let Some(parent_id) = entries[current].parent_id.as_deref() else {
            break current;
        };
        let Some(parent) = by_id.get(parent_id).copied() else {
            return invalid_index(
                start.saturating_add(2),
                "parent_id is not present in the strict index",
            );
        };
        current = parent;
    };
    for index in path {
        states[index] = RelationshipState::Complete(root);
    }
    Ok(root)
}

fn first_component(relative: &str) -> Option<&str> {
    Path::new(relative)
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
}

fn invalid_index<T>(line: usize, reason: &str) -> Result<T, StrictStoreError> {
    Err(StrictStoreError::InvalidIndexEntry {
        line,
        reason: reason.to_owned(),
    })
}

fn is_normalized_absolute_path(value: &str) -> bool {
    if value.chars().any(char::is_control) {
        return false;
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return false;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
            Component::CurDir | Component::ParentDir => return false,
        }
    }
    normalized.as_os_str() == path.as_os_str()
}
