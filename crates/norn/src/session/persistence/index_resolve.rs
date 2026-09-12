//! Session identifiers resolve globally; names can be constrained to an explicit project.

use std::fs;
use std::path::Path;
use std::time::Duration;

use super::super::acquire_private_fs;
use super::super::io::ensure_session_id_path_safe;
use super::super::types::{SessionIndexEntry, SessionPersistError};
use super::read_index_with_deadline;

/// Resolve a user-supplied identifier (empty = latest, full ID, name, or
/// at least eight-character ID prefix) against the active strict index.
pub fn resolve_session(
    data_dir: &Path,
    input: &str,
) -> Result<SessionIndexEntry, SessionPersistError> {
    resolve_session_with_deadline(data_dir, input, None)
}

/// Deadline-aware session resolution used by [`crate::session::SessionManager`].
pub(crate) fn resolve_session_with_deadline(
    data_dir: &Path,
    input: &str,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let entry = resolve_in_entries(read_index_with_deadline(data_dir, lock_deadline)?, input)?;
    ensure_session_id_path_safe(&entry.id)?;
    Ok(entry)
}

/// Resolve the most recently updated session whose indexed working directory
/// matches `working_dir`.
pub fn resolve_latest_session_in_working_dir(
    data_dir: &Path,
    working_dir: &Path,
) -> Result<SessionIndexEntry, SessionPersistError> {
    resolve_latest_session_in_working_dir_with_deadline(data_dir, working_dir, None)
}

/// Deadline-aware working-directory resolution used by
/// [`crate::session::SessionManager`].
pub(crate) fn resolve_latest_session_in_working_dir_with_deadline(
    data_dir: &Path,
    working_dir: &Path,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let entries = read_index_with_deadline(data_dir, lock_deadline)?;
    let entry = resolve_latest_in_working_dir_entries(entries, working_dir)?;
    ensure_session_id_path_safe(&entry.id)?;
    Ok(entry)
}

fn resolve_latest_in_working_dir_entries(
    entries: Vec<SessionIndexEntry>,
    working_dir: &Path,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let _permit = acquire_private_fs()?;
    let canonical_working_dir = fs::canonicalize(working_dir).ok();
    entries
        .into_iter()
        .filter(|entry| {
            working_dir_matches(
                &entry.working_dir,
                working_dir,
                canonical_working_dir.as_deref(),
            )
        })
        .max_by_key(|entry| entry.updated_at)
        .ok_or_else(|| SessionPersistError::NotFound {
            input: format!(
                "<no sessions in working directory {}>",
                working_dir.display()
            ),
        })
}

fn working_dir_matches(
    stored: &str,
    working_dir: &Path,
    canonical_working_dir: Option<&Path>,
) -> bool {
    let stored_path = Path::new(stored);
    if stored_path == working_dir {
        return true;
    }

    if let Some(canonical_working_dir) = canonical_working_dir
        && let Ok(canonical_stored) = fs::canonicalize(stored_path)
    {
        return canonical_stored == canonical_working_dir;
    }

    false
}

/// Resolve names within an explicit project; IDs and prefixes remain global.
pub(crate) fn resolve_session_in_working_dir_with_deadline(
    data_dir: &Path,
    input: &str,
    working_dir: &Path,
    lock_deadline: Option<Duration>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let entry = resolve_in_entries_with_scope(
        read_index_with_deadline(data_dir, lock_deadline)?,
        input,
        Some(working_dir),
    )?;
    ensure_session_id_path_safe(&entry.id)?;
    Ok(entry)
}

pub(super) fn resolve_in_entries(
    entries: Vec<SessionIndexEntry>,
    input: &str,
) -> Result<SessionIndexEntry, SessionPersistError> {
    resolve_in_entries_with_scope(entries, input, None)
}

fn resolve_in_entries_with_scope(
    entries: Vec<SessionIndexEntry>,
    input: &str,
    name_scope: Option<&Path>,
) -> Result<SessionIndexEntry, SessionPersistError> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        if let Some(scope) = name_scope {
            return resolve_latest_in_working_dir_entries(entries, scope);
        }
        return entries
            .into_iter()
            .max_by_key(|entry| entry.updated_at)
            .ok_or_else(|| SessionPersistError::NotFound {
                input: "<no sessions>".to_owned(),
            });
    }

    if let Some(entry) = entries.iter().find(|entry| entry.id == trimmed) {
        return Ok(entry.clone());
    }
    let mut names = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.name.as_deref() == Some(trimmed))
    {
        if let Some(scope) = name_scope
            && !name_directory_matches(Path::new(&entry.working_dir), scope)?
        {
            continue;
        }
        names.push(entry);
    }
    match names.as_slice() {
        [] => {}
        [only] => return Ok((*only).clone()),
        many => {
            return Err(SessionPersistError::AmbiguousName {
                name: trimmed.to_owned(),
                matches: many.iter().map(|entry| entry.id.clone()).collect(),
            });
        }
    }

    if trimmed.len() < 8 {
        return Err(SessionPersistError::NotFound {
            input: trimmed.to_owned(),
        });
    }

    let matches = entries
        .iter()
        .filter(|entry| entry.id.starts_with(trimmed))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(SessionPersistError::NotFound {
            input: trimmed.to_owned(),
        }),
        [only] => Ok((*only).clone()),
        many => Err(SessionPersistError::AmbiguousPrefix {
            prefix: trimmed.to_owned(),
            matches: many.iter().map(|entry| entry.id.clone()).collect(),
        }),
    }
}

fn name_directory_matches(stored: &Path, requested: &Path) -> Result<bool, SessionPersistError> {
    if stored == requested {
        return Ok(true);
    }
    let permit = acquire_private_fs()?;
    let left = canonical_name_directory(stored)?;
    let right = canonical_name_directory(requested)?;
    drop(permit);
    Ok(matches!((left, right), (Some(left), Some(right)) if left == right))
}

fn canonical_name_directory(
    path: &Path,
) -> Result<Option<std::path::PathBuf>, SessionPersistError> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SessionPersistError::NameScopeDirectory {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
#[path = "index_name_tests.rs"]
mod name_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::persistence::{
        ResumeFidelity, SESSION_FORMAT_VERSION, SessionRecordOrigin, SessionStatus,
    };
    use chrono::Utc;

    pub(super) fn entry(id: &str, name: Option<&str>, updated_seconds: i64) -> SessionIndexEntry {
        let timestamp = Utc::now() + chrono::TimeDelta::seconds(updated_seconds);
        SessionIndexEntry {
            id: id.to_owned(),
            generation: uuid::Uuid::new_v4(),
            name: name.map(str::to_owned),
            model: "gpt-test".to_owned(),
            working_dir: "/work".to_owned(),
            created_at: timestamp,
            updated_at: timestamp,
            event_count: 0,
            status: SessionStatus::Active,
            format_version: SESSION_FORMAT_VERSION,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            rel_path: None,
            parent_id: None,
            fidelity: ResumeFidelity::Canonical,
            origin: SessionRecordOrigin::Native,
            provider_state_identity: None,
        }
    }

    #[test]
    fn resolution_preserves_full_name_prefix_and_latest_rules()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = entry("12345678-first", Some("first"), 1);
        let latest = entry("87654321-latest", Some("latest"), 2);
        let entries = vec![first.clone(), latest.clone()];

        assert_eq!(resolve_in_entries(entries.clone(), "")?.id, latest.id);
        assert_eq!(resolve_in_entries(entries.clone(), "first")?.id, first.id);
        assert_eq!(resolve_in_entries(entries, "87654321")?.id, latest.id);
        Ok(())
    }
}
