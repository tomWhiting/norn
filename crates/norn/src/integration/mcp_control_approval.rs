//! Transactional approval mutations for the live MCP controller.

use std::sync::Arc;

use super::McpController;
use crate::config::{
    EffectiveMcpServer, McpApprovalSnapshot, McpApprovalState, McpApprovalStore, McpConfigLayer,
    McpConfigSource, McpLayerEntry, ResolvedMcpServer,
};
use crate::integration::mcp_control::{
    McpActivationCandidate, McpControlError, McpControlResponse,
};
use crate::tool::ToolGenerationStore;

impl McpController {
    pub(super) async fn change_approval(
        &mut self,
        name: &str,
        approve: bool,
    ) -> Result<McpControlResponse, McpControlError> {
        if approve {
            self.approve(name).await
        } else {
            self.revoke(name).await
        }
    }

    async fn approve(&mut self, name: &str) -> Result<McpControlResponse, McpControlError> {
        let snapshot = self
            .state
            .snapshot()
            .map_err(McpControlError::configuration)?;
        let server = snapshot
            .get(name)
            .filter(|server| server.source().requires_remembered_approval())
            .ok_or_else(|| McpControlError::not_shared_project(name))?;
        if self.approval_state(server)? == McpApprovalState::Approved {
            return Ok(self.mutation_response(false));
        }
        let candidate = self
            .candidate(&self.state, Some((name, McpApprovalState::Approved)))
            .await?;
        let resolved = resolved_server(server);
        let approvals = self
            .approvals
            .as_ref()
            .ok_or_else(McpControlError::approval_unavailable)?;
        let previous = approvals
            .snapshot(self.state.project_root(), name)
            .map_err(McpControlError::approval)?;
        approvals
            .approve(self.state.project_root(), &resolved)
            .map_err(McpControlError::approval)?;
        let (generation, runtime) = publish_approval_candidate(
            self.generations.as_ref(),
            self.state.project_root(),
            candidate,
            approvals,
            name,
            previous,
        )?;
        self.active_runtime.replace(generation, runtime);
        self.reconcile_watchers();
        Ok(self.mutation_response(true))
    }

    async fn revoke(&mut self, name: &str) -> Result<McpControlResponse, McpControlError> {
        let inspection = self
            .state
            .inspect(name)
            .map_err(McpControlError::configuration)?;
        if !inspection
            .chain()
            .iter()
            .any(entry_requires_remembered_approval)
        {
            return Err(McpControlError::not_shared_project(name));
        }
        let approvals = self
            .approvals
            .as_ref()
            .ok_or_else(McpControlError::approval_unavailable)?;
        let previous = approvals
            .snapshot(self.state.project_root(), name)
            .map_err(McpControlError::approval)?;
        if !McpApprovalStore::snapshot_is_approved(&previous) {
            return Ok(self.mutation_response(false));
        }
        let candidate = match inspection.effective() {
            Some(server)
                if server.enabled()
                    && server.source().requires_remembered_approval()
                    && self.approval_state(server)? == McpApprovalState::Approved =>
            {
                Some(
                    self.candidate(&self.state, Some((name, McpApprovalState::Pending)))
                        .await?,
                )
            }
            _ => None,
        };
        approvals
            .revoke(self.state.project_root(), name)
            .map_err(McpControlError::approval)?;
        if let Some(candidate) = candidate {
            let (generation, runtime) = publish_approval_candidate(
                self.generations.as_ref(),
                self.state.project_root(),
                candidate,
                approvals,
                name,
                previous,
            )?;
            self.active_runtime.replace(generation, runtime);
            self.reconcile_watchers();
        }
        Ok(self.mutation_response(true))
    }

    pub(super) fn approval(
        &self,
        server: &EffectiveMcpServer,
        approval_override: Option<(&str, McpApprovalState)>,
    ) -> Result<McpApprovalState, McpControlError> {
        approval_override
            .filter(|(name, _)| {
                *name == server.name() && server.source().requires_remembered_approval()
            })
            .map(|(_, state)| state)
            .map_or_else(|| self.approval_state(server), Ok)
    }

    fn approval_state(
        &self,
        server: &EffectiveMcpServer,
    ) -> Result<McpApprovalState, McpControlError> {
        if !server.source().requires_remembered_approval() {
            return Ok(McpApprovalState::NotRequired);
        }
        self.approvals
            .as_ref()
            .map_or(Ok(McpApprovalState::Pending), |approvals| {
                approvals
                    .state(self.state.project_root(), &resolved_server(server))
                    .map_err(McpControlError::approval)
            })
    }
}

fn entry_requires_remembered_approval(entry: &McpLayerEntry) -> bool {
    matches!(
        entry,
        McpLayerEntry::Definition { layer, .. } if layer.requires_remembered_approval()
    )
}

fn publish_approval_candidate(
    generations: &ToolGenerationStore,
    project_root: &std::path::Path,
    candidate: McpActivationCandidate,
    approvals: &McpApprovalStore,
    name: &str,
    previous: McpApprovalSnapshot,
) -> Result<
    (
        Arc<crate::tool::ToolGeneration>,
        Arc<crate::integration::McpRuntime>,
    ),
    McpControlError,
> {
    let (generation, runtime) = candidate.into_parts();
    if let Err(source) = generations.publish(Arc::clone(&generation)) {
        let error = McpControlError::publication(source);
        if let Err(rollback) = approvals.restore(project_root, name, previous) {
            return Err(McpControlError::rollback(error, rollback));
        }
        return Err(error);
    }
    Ok((generation, runtime))
}

fn resolved_server(server: &EffectiveMcpServer) -> ResolvedMcpServer {
    let source = match server.source() {
        McpConfigLayer::User => McpConfigSource::User,
        McpConfigLayer::SharedProject => McpConfigSource::Project,
        McpConfigLayer::WorkspaceLocal | McpConfigLayer::PrivateLocal => McpConfigSource::Local,
        McpConfigLayer::Cli => McpConfigSource::Cli,
        McpConfigLayer::Session => McpConfigSource::Session,
    };
    ResolvedMcpServer {
        name: server.name().to_owned(),
        source,
        definition: server.definition().clone(),
        fingerprint: server.fingerprint().clone(),
    }
}
