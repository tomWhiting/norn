//! Single-owner actor implementation for the live MCP control plane.

#[path = "mcp_control_approval.rs"]
mod approval;
#[path = "mcp_control_refresh.rs"]
mod refresh;

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::watch::ToolChangeWatchers;
use super::{
    Command, Envelope, McpActivationCandidate, McpActivationRequest, McpCandidateBuilder,
    McpControlError, McpControlResponse, McpMutationResult, McpServerDetails, McpServerStatus,
};
use crate::config::{
    EffectiveMcpServer, McpApprovalState, McpApprovalStore, McpConfigLayer, McpConfigSnapshot,
    McpConfigState, McpPersistentMutation, McpPersistentScope,
};
use crate::tool::ToolGenerationStore;

use super::super::McpRuntimeStore;
use super::super::mcp_runtime::{McpRuntimeServerState, McpRuntimeServerStatus};

pub(super) async fn run(
    state: McpConfigState,
    approvals: Option<McpApprovalStore>,
    builder: Arc<dyn McpCandidateBuilder>,
    generations: Arc<ToolGenerationStore>,
    active_runtime: Arc<McpRuntimeStore>,
    receiver: mpsc::Receiver<Envelope>,
    sender: mpsc::WeakSender<Envelope>,
) {
    McpController {
        state,
        approvals,
        builder,
        generations,
        active_runtime,
        receiver,
        watchers: ToolChangeWatchers::new(sender),
        applied_tool_revisions: BTreeMap::new(),
    }
    .run()
    .await;
}

struct McpController {
    state: McpConfigState,
    approvals: Option<McpApprovalStore>,
    builder: Arc<dyn McpCandidateBuilder>,
    generations: Arc<ToolGenerationStore>,
    active_runtime: Arc<McpRuntimeStore>,
    receiver: mpsc::Receiver<Envelope>,
    watchers: ToolChangeWatchers,
    applied_tool_revisions: BTreeMap<u64, u64>,
}

impl McpController {
    async fn run(mut self) {
        self.reconcile_watchers();
        while let Some(envelope) = self.receiver.recv().await {
            let operation = envelope.command.operation();
            let result = self.handle(envelope.command).await;
            if let Err(error) = result.as_ref() {
                tracing::error!(
                    operation,
                    error = %error,
                    cause = ?error,
                    "live MCP control operation failed",
                );
            }
            if envelope.reply.send(result).is_err() {
                tracing::debug!(
                    operation,
                    "live MCP control requester closed before receiving its result",
                );
            }
        }
        self.watchers.abort_all();
    }

    async fn handle(&mut self, command: Command) -> Result<McpControlResponse, McpControlError> {
        match command {
            Command::List => self.list().map(McpControlResponse::List),
            Command::Inspect(name) => self
                .inspect(&name)
                .map(Box::new)
                .map(McpControlResponse::Inspect),
            Command::SessionAdd { name, definition } => {
                self.session_mutation(move |state| state.session_add(name, definition))
                    .await
            }
            Command::SessionRemove(name) => {
                self.session_mutation(move |state| state.session_remove(&name))
                    .await
            }
            Command::SessionDisable(name) => {
                self.session_mutation(move |state| state.session_disable(&name))
                    .await
            }
            Command::SessionEnable(name) => {
                self.session_mutation(move |state| state.session_enable(&name))
                    .await
            }
            Command::Persist { scope, mutation } => self.persist(scope, mutation).await,
            Command::Approve(name) => self.change_approval(&name, true).await,
            Command::Revoke(name) => self.change_approval(&name, false).await,
            Command::Reload => self.reload().await,
            Command::RefreshTools {
                name,
                instance_id,
                revision,
            } => self.refresh_tools(&name, instance_id, revision).await,
        }
    }

    fn list(&self) -> Result<Vec<McpServerStatus>, McpControlError> {
        let snapshot = self
            .state
            .snapshot()
            .map_err(McpControlError::configuration)?;
        snapshot
            .iter()
            .map(|server| self.status(server, None))
            .collect()
    }

    fn inspect(&self, name: &str) -> Result<McpServerDetails, McpControlError> {
        let inspection = self
            .state
            .inspect(name)
            .map_err(McpControlError::configuration)?;
        let (approval, runtime_state, failure_present, active) = match inspection.effective() {
            Some(server) => {
                let status = self.status(server, None)?;
                (
                    Some(status.approval),
                    status.runtime_state,
                    status.failure_present,
                    status.active,
                )
            }
            None => (None, None, false, false),
        };
        Ok(McpServerDetails {
            inspection,
            approval,
            runtime_state,
            failure_present,
            active,
            revision: self.generations.snapshot().revision(),
        })
    }

    async fn session_mutation<F>(
        &mut self,
        mutate: F,
    ) -> Result<McpControlResponse, McpControlError>
    where
        F: FnOnce(&mut McpConfigState) -> Result<bool, crate::error::ConfigError>,
    {
        let mut staged = self.state.clone();
        let changed = mutate(&mut staged).map_err(McpControlError::configuration)?;
        self.commit_staged(staged, changed).await
    }

    async fn reload(&mut self) -> Result<McpControlResponse, McpControlError> {
        let mut staged = self.state.clone();
        let changed = staged
            .reload_disk()
            .map_err(McpControlError::configuration)?;
        self.commit_staged(staged, changed).await
    }

    async fn persist(
        &mut self,
        scope: McpPersistentScope,
        mutation: McpPersistentMutation,
    ) -> Result<McpControlResponse, McpControlError> {
        let previous = persistent_restore(&self.state, scope, &mutation);
        let mut staged = self.state.clone();
        let change = staged
            .persist(scope, &mutation)
            .map_err(McpControlError::configuration)?;
        if !change.changed() {
            return Ok(self.mutation_response(false));
        }
        let candidate = match self.candidate(&staged, None).await {
            Ok(candidate) => candidate,
            Err(error) => {
                if let Err(rollback) = staged.persist(scope, &previous) {
                    return Err(McpControlError::rollback(error, rollback));
                }
                return Err(error);
            }
        };
        if let Err(error) = self.publish(candidate) {
            if let Err(rollback) = staged.persist(scope, &previous) {
                return Err(McpControlError::rollback(error, rollback));
            }
            return Err(error);
        }
        self.state = staged;
        Ok(self.mutation_response(true))
    }

    async fn commit_staged(
        &mut self,
        staged: McpConfigState,
        changed: bool,
    ) -> Result<McpControlResponse, McpControlError> {
        if !changed {
            return Ok(self.mutation_response(false));
        }
        let candidate = self.candidate(&staged, None).await?;
        self.publish(candidate)?;
        self.state = staged;
        Ok(self.mutation_response(true))
    }

    async fn candidate(
        &self,
        state: &McpConfigState,
        approval_override: Option<(&str, McpApprovalState)>,
    ) -> Result<McpActivationCandidate, McpControlError> {
        self.candidate_with_runtime(
            state,
            approval_override,
            self.active_runtime.snapshot().runtime(),
        )
        .await
    }

    async fn candidate_with_runtime(
        &self,
        state: &McpConfigState,
        approval_override: Option<(&str, McpApprovalState)>,
        previous_runtime: Arc<super::super::McpRuntime>,
    ) -> Result<McpActivationCandidate, McpControlError> {
        let previous = self.generations.snapshot();
        let revision = previous
            .revision()
            .checked_add(1)
            .ok_or_else(McpControlError::revision_overflow)?;
        let snapshot = state.snapshot().map_err(McpControlError::configuration)?;
        let active_servers = self.active_servers(&snapshot, approval_override)?;
        let candidate = self
            .builder
            .build(McpActivationRequest {
                revision,
                previous,
                previous_runtime,
                active_servers,
            })
            .await
            .map_err(McpControlError::candidate)?;
        if candidate.generation.revision() != revision {
            return Err(McpControlError::candidate(
                super::McpCandidateError::RevisionMismatch {
                    expected: revision,
                    actual: candidate.generation.revision(),
                },
            ));
        }
        Ok(candidate)
    }

    fn active_servers(
        &self,
        snapshot: &McpConfigSnapshot,
        approval_override: Option<(&str, McpApprovalState)>,
    ) -> Result<Arc<[EffectiveMcpServer]>, McpControlError> {
        let mut active = Vec::new();
        for server in snapshot.iter().filter(|server| server.enabled()) {
            let approval = self.approval(server, approval_override)?;
            if approval != McpApprovalState::Pending {
                active.push(server.clone());
            }
        }
        Ok(Arc::from(active))
    }

    fn status(
        &self,
        server: &EffectiveMcpServer,
        approval_override: Option<(&str, McpApprovalState)>,
    ) -> Result<McpServerStatus, McpControlError> {
        let approval = self.approval(server, approval_override)?;
        let eligible = server.enabled() && approval != McpApprovalState::Pending;
        let active_runtime = self.active_runtime.snapshot();
        let runtime = if eligible {
            active_runtime
                .runtime()
                .reported_server_status(server.name())
        } else {
            None
        };
        Ok(McpServerStatus {
            name: server.name().to_owned(),
            source: server.source(),
            enabled: server.enabled(),
            approval,
            runtime_state: runtime.as_ref().map(McpRuntimeServerStatus::state),
            failure_present: runtime
                .as_ref()
                .is_some_and(|status| status.failure().is_some()),
            active: runtime
                .is_some_and(|status| status.state() == McpRuntimeServerState::Connected),
        })
    }

    fn mutation_response(&self, changed: bool) -> McpControlResponse {
        McpControlResponse::Mutation(McpMutationResult {
            changed,
            revision: self.generations.snapshot().revision(),
        })
    }

    fn publish(&mut self, candidate: McpActivationCandidate) -> Result<(), McpControlError> {
        let (generation, runtime) = candidate.into_parts();
        self.generations
            .publish(Arc::clone(&generation))
            .map_err(McpControlError::publication)?;
        self.active_runtime.replace(generation, runtime);
        self.reconcile_watchers();
        Ok(())
    }

    fn reconcile_watchers(&mut self) {
        let runtime = self.active_runtime.snapshot().runtime();
        self.watchers.reconcile(runtime.as_ref());
        let active: std::collections::BTreeSet<_> = runtime
            .tool_change_subscriptions()
            .into_iter()
            .map(|(_, instance_id, _)| instance_id)
            .collect();
        self.applied_tool_revisions
            .retain(|instance_id, _| active.contains(instance_id));
    }
}

fn persistent_restore(
    state: &McpConfigState,
    scope: McpPersistentScope,
    mutation: &McpPersistentMutation,
) -> McpPersistentMutation {
    let name = match mutation {
        McpPersistentMutation::Upsert { name, .. }
        | McpPersistentMutation::Remove { name }
        | McpPersistentMutation::SetEnabled { name, .. } => name,
    };
    let layer = match scope {
        McpPersistentScope::User => McpConfigLayer::User,
        McpPersistentScope::SharedProject => McpConfigLayer::SharedProject,
        McpPersistentScope::WorkspaceLocal => McpConfigLayer::WorkspaceLocal,
        McpPersistentScope::PrivateLocal => McpConfigLayer::PrivateLocal,
    };
    match state
        .definitions(layer)
        .and_then(|definitions| definitions.get(name))
    {
        Some(definition) => McpPersistentMutation::Upsert {
            name: name.clone(),
            definition: definition.clone(),
        },
        None => McpPersistentMutation::Remove { name: name.clone() },
    }
}
