//! Explicit registered-session discovery; no transcript loads or live-agent reconstruction.

use std::sync::Arc;

use super::ActionLogArgs;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::traits::ToolOutput;
use crate::tools::agent::AgentToolInfra;

pub(super) async fn query(
    args: &ActionLogArgs,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    if args.filter.is_some() || args.call_id.is_some() || args.scope.is_some() {
        return Ok(ToolOutput::failure(ToolErrorPayload::new(
            ToolErrorKind::InvalidArguments,
            "branches accepts only query; filter, call_id and scope do not apply",
        )));
    }
    let infra = ctx.require_extension::<AgentToolInfra>()?;
    let session = Arc::clone(&infra.session);
    // Index locking and recovery are blocking filesystem work. Cancellation may
    // abandon the result; normal index recovery may finish a prior journal, but
    // this query never initiates branching or dispatches agent work.
    let directory = tokio::task::spawn_blocking(move || session.recorded_directory())
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!("registered branch directory task failed: {error}"),
        })?
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!("registered branch directory refused: {error}"),
        })?;
    Ok(ToolOutput::success(serde_json::json!({
        "directory": directory,
        "coverage": {
            "source": "registered_session_index",
            "timeline_readability": "not_inspected",
            "live_recipients": "not_inspected",
            "unregistered_and_ephemeral_reservations": "not_inspected"
        }
    })))
}

#[cfg(test)]
#[path = "action_log_branches_tests.rs"]
mod tests;
