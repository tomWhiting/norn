//! Terminal pending-message recovery gates for `close_agent`.

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::PendingAgentMessages;
use crate::agent::registry::AgentRegistry;
use crate::error::ToolError;

/// Discharge retained terminal queue authority before lifecycle reclamation.
///
/// The model-facing error deliberately reports only identity and count;
/// accepted message content remains confined to the recovery store.
pub(super) fn recover_terminal_pending_before_reclamation(
    pending_messages: &PendingAgentMessages,
    id: Uuid,
) -> Result<(), ToolError> {
    let Some(initial) = pending_messages.terminal_pending_recovery_status(id) else {
        return Ok(());
    };

    if let Err(error) = pending_messages.retry_terminal_pending(id) {
        let pending_count = pending_messages
            .terminal_pending_recovery_status(id)
            .map_or(initial.pending_count, |status| status.pending_count);
        tracing::error!(
            agent_id = %id,
            pending_count,
            error = %error,
            "close_agent: exact terminal pending-message recovery failed; lifecycle state retained",
        );
        return Err(unresolved_terminal_pending_error(id, pending_count));
    }

    if let Some(status) = pending_messages.terminal_pending_recovery_status(id) {
        tracing::error!(
            agent_id = %id,
            pending_count = status.pending_count,
            "close_agent: terminal pending-message recovery returned without discharging authority",
        );
        return Err(unresolved_terminal_pending_error(id, status.pending_count));
    }
    Ok(())
}

/// Recheck recovery after terminal status becomes observable, then reclaim.
///
/// Spawn/fork wrappers finish their terminal drain before publishing terminal
/// status. Therefore, once close observes that status, this final recovery gate
/// covers a marker created after the earlier no-handle postflight.
pub(super) fn reclaim_observed_terminal(
    registry: &RwLock<AgentRegistry>,
    pending_messages: &PendingAgentMessages,
    id: Uuid,
) -> Result<&'static str, ToolError> {
    recover_terminal_pending_before_reclamation(pending_messages, id)?;

    let mut registry = registry.write();
    if registry.remove_terminal(id) {
        return Ok("reclaimed");
    }
    if registry.tombstone(id).is_some() {
        return Ok("already_completed");
    }
    tracing::error!(
        agent_id = %id,
        "close_agent: terminal entry changed before recovery-gated reclamation",
    );
    Ok("missing")
}

fn unresolved_terminal_pending_error(id: Uuid, pending_count: usize) -> ToolError {
    ToolError::ExecutionFailed {
        reason: format!(
            "close_agent cannot reclaim agent {id}: terminal pending-message persistence remains \
             unresolved for {pending_count} accepted message(s); exact recovery authority was retained"
        ),
    }
}
