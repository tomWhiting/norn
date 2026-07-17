use super::strict::StrictStoreError;
use super::types::SessionPersistError;

pub(super) fn map_strict_error(error: StrictStoreError) -> SessionPersistError {
    match error {
        StrictStoreError::Io { source, .. } => SessionPersistError::from(source),
        other => SessionPersistError::InvalidTimeline(Box::new(other)),
    }
}
