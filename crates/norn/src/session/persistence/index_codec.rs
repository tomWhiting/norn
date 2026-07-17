use std::ffi::OsStr;
use std::io::{BufReader, BufWriter, Write as _};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::super::io::{ensure_session_id_path_safe, session_file_relative};
use super::super::strict::{StrictFormatHeader, read_strict_index_file, validate_index_entries};
use super::super::types::{SessionIndexEntry, SessionPersistError};
use crate::util::{PrivateEntryKind, PrivateRoot};

pub(super) const INDEX_FILE_NAME: &str = "index.jsonl";
const INDEX_LOCK_FILE_NAME: &str = "index.lock";

/// Return the session index file path under `data_dir`.
#[must_use]
#[cfg(test)]
pub(crate) fn index_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(INDEX_FILE_NAME)
}

pub(super) fn read_index_in(
    root: &PrivateRoot,
) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let relative = Path::new(INDEX_FILE_NAME);
    let file = match root.open_read(relative) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return missing_index_result(root);
        }
        Err(error) => return Err(error.into()),
    };
    let path = root.display_path(relative);
    Ok(read_strict_index_file(BufReader::new(file), &path)?.entries)
}

fn missing_index_result(root: &PrivateRoot) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let entries = root.read_dir(Path::new(""))?;
    let fresh = entries.iter().all(|entry| {
        entry.kind == PrivateEntryKind::File
            && (entry.name.as_os_str() == OsStr::new(INDEX_LOCK_FILE_NAME)
                || super::publication::is_publication_artifact_name(entry.name.as_os_str()))
    });
    if fresh {
        return Ok(Vec::new());
    }
    Err(SessionPersistError::MissingIndex {
        path: root.path().to_path_buf(),
    })
}

/// Atomically replace the complete active index while holding its process lock.
#[cfg(test)]
pub(crate) fn write_index_atomic(
    data_dir: &Path,
    entries: &[SessionIndexEntry],
) -> Result<(), SessionPersistError> {
    let lock = super::lock_recovered_index(data_dir, None)?;
    let _current = read_index_in(lock.root())?;
    write_index_atomic_in(lock.root(), entries)
}

pub(super) fn write_index_atomic_in(
    root: &PrivateRoot,
    entries: &[SessionIndexEntry],
) -> Result<(), SessionPersistError> {
    for entry in entries {
        validate_entry_path(entry)?;
    }
    validate_index_entries(entries)?;
    let final_path = Path::new(INDEX_FILE_NAME);
    let tmp_path = super::index_artifacts::new_index_temp_path();

    if let Err(error) = write_temp(root, &tmp_path, entries) {
        remove_tmp_after_failure(root, &tmp_path);
        return Err(error);
    }
    if let Err(error) = root.rename(&tmp_path, final_path) {
        remove_tmp_after_failure(root, &tmp_path);
        return Err(error.into());
    }
    root.sync_dir(Path::new("")).map_err(|source| {
        SessionPersistError::IndexCommitIndeterminate {
            path: root.display_path(final_path),
            source,
        }
    })?;
    Ok(())
}

pub(super) fn validate_entry_path(entry: &SessionIndexEntry) -> Result<(), SessionPersistError> {
    session_file_relative(entry)?;
    if let Some(parent_id) = entry.parent_id.as_deref() {
        ensure_session_id_path_safe(parent_id)?;
    }
    Ok(())
}

fn write_temp(
    root: &PrivateRoot,
    tmp_path: &Path,
    entries: &[SessionIndexEntry],
) -> Result<(), SessionPersistError> {
    let file = root.create_new(tmp_path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, &StrictFormatHeader::current())?;
    writer.write_all(b"\n")?;
    for entry in entries {
        serde_json::to_writer(&mut writer, entry)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    Ok(())
}

fn remove_tmp_after_failure(root: &PrivateRoot, tmp_path: &Path) {
    if let Err(cleanup_error) = root.remove_file(tmp_path)
        && cleanup_error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            tmp_path = %root.display_path(tmp_path).display(),
            %cleanup_error,
            "failed to remove temporary session index after an atomic rewrite failure",
        );
    }
}
