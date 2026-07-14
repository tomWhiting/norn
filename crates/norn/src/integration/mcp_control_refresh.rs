//! Notification-driven MCP tool refresh and bounded recovery.

use std::sync::Arc;

use super::McpController;
use crate::integration::McpRuntime;
use crate::integration::mcp_control::{
    McpActivationCandidate, McpCandidateError, McpControlError, McpControlResponse,
};
use crate::tool::ToolGeneration;

impl McpController {
    pub(super) async fn refresh_tools(
        &mut self,
        name: &str,
        instance_id: u64,
        revision: u64,
    ) -> Result<McpControlResponse, McpControlError> {
        if self
            .applied_tool_revisions
            .get(&instance_id)
            .is_some_and(|applied| *applied >= revision)
        {
            return Ok(self.mutation_response(false));
        }
        let current = self.active_runtime.snapshot().runtime();
        let refreshed = match current.refreshed_tools(name, instance_id).await {
            Ok(Some(refreshed)) => refreshed,
            Ok(None) => return Ok(self.mutation_response(false)),
            Err(error) => {
                return self
                    .recover_refresh_failure(name, instance_id, revision, current, error)
                    .await;
            }
        };
        let candidate = self
            .candidate_with_runtime(&self.state, None, refreshed)
            .await?;
        self.publish(candidate)?;
        self.applied_tool_revisions.insert(instance_id, revision);
        Ok(self.mutation_response(true))
    }

    async fn recover_refresh_failure(
        &mut self,
        name: &str,
        instance_id: u64,
        revision: u64,
        current: Arc<McpRuntime>,
        error: crate::error::IntegrationError,
    ) -> Result<McpControlResponse, McpControlError> {
        tracing::warn!(
            server = name,
            %error,
            "MCP tool refresh failed; reconnecting the invalidated client",
        );
        let refresh_error = McpControlError::candidate(McpCandidateError::Refresh(error));
        let reconnect = match self
            .candidate_with_runtime(&self.state, None, Arc::clone(&current))
            .await
        {
            Ok(candidate) => self.publish(candidate),
            Err(error) => Err(error),
        };
        if let Err(reconnect_error) = reconnect {
            let primary = McpControlError::refresh_recovery(refresh_error, reconnect_error);
            tracing::warn!(
                server = name,
                error = %primary,
                cause = ?primary,
                "MCP client reconnection failed; publishing a disconnected runtime",
            );
            if let Err(fallback_error) = self.publish_disconnected_refresh(
                name,
                instance_id,
                current.as_ref(),
                primary.to_string(),
            ) {
                return Err(McpControlError::refresh_recovery(primary, fallback_error));
            }
            return Ok(self.mutation_response(true));
        }
        self.applied_tool_revisions.insert(instance_id, revision);
        Ok(self.mutation_response(true))
    }

    fn publish_disconnected_refresh(
        &mut self,
        name: &str,
        instance_id: u64,
        current: &McpRuntime,
        failure: String,
    ) -> Result<(), McpControlError> {
        let (runtime, removed_tools) = current
            .disconnected_after_refresh_failure(name, instance_id, failure)
            .ok_or_else(|| {
                McpControlError::protocol(
                    "refresh recovery could not identify the invalidated MCP client",
                )
            })?;
        let previous = self.generations.snapshot();
        let revision = previous
            .revision()
            .checked_add(1)
            .ok_or_else(McpControlError::revision_overflow)?;
        let generation =
            ToolGeneration::removing_dynamic_tools(previous.as_ref(), &removed_tools, revision);
        self.publish(McpActivationCandidate::new(Arc::new(generation), runtime))
    }
}
