//! Source validation before child admission; store and spool I/O stay off the async executor.

use std::sync::Arc;
use uuid::Uuid;

use crate::error::ToolError;
use crate::session::store::EventStore;
use crate::session_view::ViewSource;

pub(super) async fn capture(store: Arc<EventStore>, child: Uuid) -> Result<ViewSource, ToolError> {
    tokio::task::spawn_blocking(move || {
        store.history_reader().map(|reader| reader.source().clone())
    })
    .await
    .map_err(|error| ToolError::ExecutionFailed {
        reason: format!("child {child} result source validation task failed: {error}"),
    })?
    .map_err(|error| ToolError::ExecutionFailed {
        reason: format!("child {child} result source binding failed: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unbound_child_source_is_refused_with_its_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let child = Uuid::new_v4();
        let error = capture(Arc::new(EventStore::new()), child)
            .await
            .err()
            .ok_or("unbound child unexpectedly acquired source provenance")?;
        assert!(error.to_string().contains(&child.to_string()));
        assert!(error.to_string().contains("source binding failed"));
        Ok(())
    }
}
