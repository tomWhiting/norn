//! Durable failure receipts preserve compaction spend without inventing a committed summary.

use super::helpers::append_and_notify;
use crate::error::{CompactionFailure, CompactionFailureReason, SessionError};
use crate::integration::hooks::HookRegistry;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

pub(super) async fn record_failure(
    store: &EventStore,
    hooks: Option<&HookRegistry>,
    failure: &CompactionFailure,
) -> Result<(), SessionError> {
    let (kind, stop_reason, text_chars) = match &failure.reason {
        CompactionFailureReason::Provider(_) => ("provider_error", None, None),
        CompactionFailureReason::Unusable {
            stop_reason,
            text_chars,
        } => (
            "unusable_response",
            Some(format!("{stop_reason:?}")),
            Some(*text_chars),
        ),
    };
    append_and_notify(
        store,
        SessionEvent::Custom {
            base: EventBase::new(store.last_event_id()),
            event_type: "loop.compaction_failed".to_owned(),
            data: serde_json::json!({
                "schema_version": 1, "model": failure.model, "failure_kind": kind,
                "error_class": failure.reason.class(), "error": failure.to_string(),
                "stop_reason": stop_reason, "text_chars": text_chars,
                "context_changed": false, "usage": failure.usage,
            }),
        },
        hooks,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "compaction_failure_tests.rs"]
mod tests;
