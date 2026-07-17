use super::super::super::strict::StrictStoreError;
use super::super::super::strict_runtime::map_strict_error;
use super::SessionPersistError;

pub(super) fn map_publication_timeline_error(
    error: StrictStoreError,
    session_id: &str,
) -> SessionPersistError {
    match error {
        StrictStoreError::IndexCounterOverflow { field, .. } => {
            SessionPersistError::IndexCounterOverflow {
                id: session_id.to_owned(),
                field,
            }
        }
        other => map_strict_error(other),
    }
}
