//! Exact ownership and crash cleanup for atomic index-rewrite artifacts.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::super::types::SessionPersistError;
use crate::util::{PrivateEntryKind, PrivateRoot};

pub(super) const INDEX_TEMP_STEM: &str = "index.jsonl.tmp";
pub(super) const INDEX_TEMP_PREFIX: &str = "index.jsonl.tmp.";

pub(super) fn new_index_temp_path() -> PathBuf {
    PathBuf::from(format!(
        "{INDEX_TEMP_PREFIX}{}",
        Uuid::new_v4().hyphenated()
    ))
}

pub(super) fn discard_temporary_indexes(root: &PrivateRoot) -> Result<(), SessionPersistError> {
    let mut owned = Vec::new();
    for entry in root.read_dir(Path::new(""))? {
        let name = entry.name.to_string_lossy();
        if name.as_ref() != INDEX_TEMP_STEM && !name.starts_with(INDEX_TEMP_PREFIX) {
            continue;
        }
        if index_temp_id(&entry.name).is_none() {
            return Err(conflict(
                name.as_ref(),
                "the index temporary name is not an exact canonical UUID artifact",
            ));
        }
        if entry.kind != PrivateEntryKind::File {
            return Err(conflict(
                name.as_ref(),
                "the owned index temporary name is not a regular file",
            ));
        }
        owned.push(PathBuf::from(entry.name));
    }
    for path in &owned {
        root.remove_file(path)?;
    }
    if !owned.is_empty() {
        root.sync_dir(Path::new(""))?;
    }
    Ok(())
}

fn index_temp_id(name: &OsStr) -> Option<String> {
    let value = name.to_str()?;
    let raw = value.strip_prefix(INDEX_TEMP_PREFIX)?;
    let parsed = Uuid::parse_str(raw).ok()?;
    let canonical = parsed.hyphenated().to_string();
    (canonical == raw).then_some(canonical)
}

fn conflict(name: &str, reason: &'static str) -> SessionPersistError {
    SessionPersistError::IndexArtifactConflict {
        name: name.to_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_index_temporary_name_requires_canonical_uuid() {
        let id = Uuid::new_v4().hyphenated().to_string();
        assert_eq!(
            index_temp_id(OsStr::new(&format!("{INDEX_TEMP_PREFIX}{id}"))),
            Some(id)
        );
        assert!(index_temp_id(OsStr::new("index.jsonl.tmp.stale")).is_none());
        assert!(index_temp_id(OsStr::new("index.jsonl.tmp.")).is_none());
    }
}
