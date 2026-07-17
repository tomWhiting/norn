use std::path::Path;

use crate::session::persistence::types::{SessionIndexEntry, SessionPersistError};

pub(super) fn path_occupied(entry: &SessionIndexEntry, path: &Path) -> SessionPersistError {
    entry.rel_path.as_ref().map_or_else(
        || SessionPersistError::IdExists {
            id: entry.id.clone(),
        },
        |_| SessionPersistError::ChildPathOccupied {
            rel_path: path.to_string_lossy().into_owned(),
        },
    )
}

pub(super) fn conflict(id: &str, reason: &'static str) -> SessionPersistError {
    SessionPersistError::PublicationConflict {
        id: id.to_owned(),
        reason,
    }
}
