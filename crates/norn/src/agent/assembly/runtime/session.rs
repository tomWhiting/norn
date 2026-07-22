//! Session event-store and action-log restoration.

use std::sync::Arc;

use crate::r#loop::loop_context::LoopContext;
use crate::session::action_log::ActionLog;
use crate::session::store::EventStore;
use crate::tool::context::SharedWorkingDir;

/// Create or resume the session event store and the action log that shares it.
///
/// The event store backs both `ToolResult` persistence and action-log lookups,
/// so one `Arc` is shared between them. The action log resolves relative paths
/// against the live agent working directory. On resume, one
/// [`crate::session::ReplayArtifacts`] traversal restores both persisted
/// context-edit marks and the action ledger.
pub(crate) fn restore_session_state(
    session: Option<Arc<EventStore>>,
    loop_context: &mut LoopContext,
    shared_wd: SharedWorkingDir,
) -> (Arc<EventStore>, Arc<ActionLog>) {
    let resuming = session.is_some();
    let event_store = session.unwrap_or_else(|| Arc::new(EventStore::new()));
    let action_log = Arc::new(ActionLog::with_working_dir(
        Arc::clone(&event_store),
        shared_wd,
    ));
    if resuming {
        let artifacts = crate::session::ReplayArtifacts::from_events(event_store.events());
        if let Some(edits) = loop_context.context_edits.as_mut() {
            edits.mark_superseded(artifacts.superseded_event_ids.iter().cloned());
            edits.mark_suppressed(artifacts.suppressed_event_ids.iter().cloned());
            edits.mark_injected(artifacts.injected_event_ids.iter().cloned());
        }
        crate::agent::resume::rebuild_action_log(&action_log, &artifacts.events);
    }
    (event_store, action_log)
}
