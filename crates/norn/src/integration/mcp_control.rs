//! Serialized live control plane for MCP configuration and tool generations.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::config::{
    EffectiveMcpServer, McpApprovalState, McpApprovalStore, McpConfigLayer, McpConfigState,
    McpPersistentMutation, McpPersistentScope, McpServerInspection, McpServerSettings,
};
use crate::tool::{ToolGeneration, ToolGenerationStore};

use super::McpRuntimeStore;
use super::mcp_runtime::{McpRuntime, McpRuntimeServerState};

#[path = "mcp_control_actor.rs"]
mod actor;
#[path = "mcp_control_watch.rs"]
mod watch;

/// Input to a candidate builder for one prospective runtime revision.
pub struct McpActivationRequest {
    revision: u64,
    previous: Arc<ToolGeneration>,
    previous_runtime: Arc<McpRuntime>,
    active_servers: Arc<[EffectiveMcpServer]>,
}

impl McpActivationRequest {
    /// Assemble the complete input for one prospective runtime revision.
    #[must_use]
    pub fn new(
        revision: u64,
        previous: Arc<ToolGeneration>,
        previous_runtime: Arc<McpRuntime>,
        active_servers: Arc<[EffectiveMcpServer]>,
    ) -> Self {
        Self {
            revision,
            previous,
            previous_runtime,
            active_servers,
        }
    }

    /// Revision the candidate must carry.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Currently published generation, retained as the stable-tool base.
    #[must_use]
    pub fn previous(&self) -> Arc<ToolGeneration> {
        Arc::clone(&self.previous)
    }

    /// Currently committed runtime, used to reuse unchanged clients.
    #[must_use]
    pub fn previous_runtime(&self) -> Arc<McpRuntime> {
        Arc::clone(&self.previous_runtime)
    }

    /// Enabled, approved servers that may contribute tools.
    #[must_use]
    pub fn active_servers(&self) -> Arc<[EffectiveMcpServer]> {
        Arc::clone(&self.active_servers)
    }
}

/// Fully assembled runtime and tool generation awaiting atomic publication.
pub struct McpActivationCandidate {
    generation: Arc<ToolGeneration>,
    runtime: Arc<McpRuntime>,
}

impl McpActivationCandidate {
    /// Pair a generation with the exact runtime that supplied its MCP tools.
    #[must_use]
    pub fn new(generation: Arc<ToolGeneration>, runtime: Arc<McpRuntime>) -> Self {
        Self {
            generation,
            runtime,
        }
    }

    /// Tool generation assembled from this exact runtime candidate.
    #[must_use]
    pub fn generation(&self) -> Arc<ToolGeneration> {
        Arc::clone(&self.generation)
    }

    /// MCP runtime whose proxies and statuses accompany the generation.
    #[must_use]
    pub fn runtime(&self) -> Arc<McpRuntime> {
        Arc::clone(&self.runtime)
    }

    fn into_parts(self) -> (Arc<ToolGeneration>, Arc<McpRuntime>) {
        (self.generation, self.runtime)
    }
}

impl fmt::Debug for McpActivationCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpActivationCandidate")
            .field("revision", &self.generation.revision())
            .field("connected_server_count", &self.runtime.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for McpActivationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpActivationRequest")
            .field("revision", &self.revision)
            .field("active_server_count", &self.active_servers.len())
            .finish_non_exhaustive()
    }
}

/// Redacted failure from an injected candidate builder.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("the MCP runtime candidate could not be built")]
pub struct McpCandidateError;

/// Builds a complete immutable generation without publishing it.
#[async_trait]
pub trait McpCandidateBuilder: Send + Sync {
    /// Build one candidate containing only `request.active_servers()`.
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError>;
}

/// One safe list/status projection of an effective MCP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerStatus {
    /// Logical server name.
    pub name: String,
    /// Winning configuration layer.
    pub source: McpConfigLayer,
    /// Whether the winning definition is enabled.
    pub enabled: bool,
    /// Definition-bound approval state.
    pub approval: McpApprovalState,
    /// Connection outcome in the committed runtime, absent when ineligible.
    pub runtime_state: Option<McpRuntimeServerState>,
    /// Whether a redacted runtime failure diagnostic is available.
    pub failure_present: bool,
    /// Whether this server contributes tools to the current generation.
    pub active: bool,
}

/// Full inspection plus its activation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerDetails {
    /// Complete provenance chain. Secret-bearing fields have redacted `Debug`.
    pub inspection: McpServerInspection,
    /// Definition-bound approval state, when a definition is effective.
    pub approval: Option<McpApprovalState>,
    /// Connection outcome in the committed runtime, absent when ineligible.
    pub runtime_state: Option<McpRuntimeServerState>,
    /// Whether a redacted runtime failure diagnostic is available.
    pub failure_present: bool,
    /// Whether this server contributes tools to the current generation.
    pub active: bool,
    /// Runtime generation observed by this read.
    pub revision: u64,
}

/// Result shared by all successful mutations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpMutationResult {
    /// Whether configuration or approval state changed.
    pub changed: bool,
    /// Published runtime revision after the operation.
    pub revision: u64,
}

/// Typed response from the live control actor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpControlResponse {
    /// Deterministic effective-server listing.
    List(Vec<McpServerStatus>),
    /// One full provenance inspection.
    Inspect(Box<McpServerDetails>),
    /// Mutation outcome.
    Mutation(McpMutationResult),
}

/// Redacted controller failure categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpControlError {
    /// The actor is no longer available.
    #[error("the MCP control plane is unavailable")]
    Unavailable,
    /// Configuration validation, loading, or persistence failed.
    #[error("the MCP configuration operation failed")]
    Configuration,
    /// Approval storage could not be read or changed.
    #[error("the MCP approval operation failed")]
    Approval,
    /// The requested server is not an effective project-controlled definition.
    #[error("the MCP server is not eligible for project approval")]
    NotProjectControlled,
    /// Candidate construction failed; the current generation remains active.
    #[error("the MCP runtime candidate could not be built")]
    Candidate,
    /// Candidate publication violated a generation invariant.
    #[error("the MCP runtime candidate could not be published")]
    Publication,
    /// A compensating persistent operation failed.
    #[error("the MCP configuration rollback failed")]
    Rollback,
    /// The actor returned a response variant that violates its protocol.
    #[error("the MCP control plane returned an invalid response")]
    Protocol,
}

/// Cloneable programmatic control surface suitable for an `AgentHandle`.
#[derive(Clone)]
pub struct McpControlHandle {
    sender: mpsc::Sender<Envelope>,
}

impl McpControlHandle {
    /// Spawn a serialized control actor on the current Tokio runtime.
    pub fn spawn(
        state: McpConfigState,
        approvals: impl Into<Option<McpApprovalStore>>,
        builder: Arc<dyn McpCandidateBuilder>,
        generations: Arc<ToolGenerationStore>,
        active_runtime: Arc<McpRuntimeStore>,
    ) -> Result<Self, McpControlError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_runtime_error| McpControlError::Unavailable)?;
        // The actor owns one command and permits exactly one queued successor.
        let (sender, receiver) = mpsc::channel(1);
        let weak_sender = sender.downgrade();
        runtime.spawn(actor::run(
            state,
            approvals.into(),
            builder,
            generations,
            active_runtime,
            receiver,
            weak_sender,
        ));
        Ok(Self { sender })
    }

    /// List effective servers without exposing secret-bearing definitions.
    pub async fn list(&self) -> Result<Vec<McpServerStatus>, McpControlError> {
        match self.request(Command::List).await? {
            McpControlResponse::List(statuses) => Ok(statuses),
            _ => Err(McpControlError::Protocol),
        }
    }

    /// Inspect one server's complete provenance chain.
    pub async fn inspect(&self, name: String) -> Result<McpServerDetails, McpControlError> {
        match self.request(Command::Inspect(name)).await? {
            McpControlResponse::Inspect(details) => Ok(*details),
            _ => Err(McpControlError::Protocol),
        }
    }

    /// Add or wholly replace an ephemeral session definition.
    pub async fn session_add(
        &self,
        name: String,
        definition: McpServerSettings,
    ) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::SessionAdd { name, definition })
            .await
    }

    /// Remove only the session entry, revealing the next lower definition.
    pub async fn session_remove(&self, name: String) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::SessionRemove(name)).await
    }

    /// Disable the effective definition through a session tombstone.
    pub async fn session_disable(
        &self,
        name: String,
    ) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::SessionDisable(name)).await
    }

    /// Re-enable a disabled session entry without copying a lower definition.
    pub async fn session_enable(&self, name: String) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::SessionEnable(name)).await
    }

    /// Apply an explicit persistent upsert.
    pub async fn persistent_upsert(
        &self,
        scope: McpPersistentScope,
        name: String,
        definition: McpServerSettings,
    ) -> Result<McpMutationResult, McpControlError> {
        self.persist(scope, McpPersistentMutation::Upsert { name, definition })
            .await
    }

    /// Apply an explicit persistent removal.
    pub async fn persistent_remove(
        &self,
        scope: McpPersistentScope,
        name: String,
    ) -> Result<McpMutationResult, McpControlError> {
        self.persist(scope, McpPersistentMutation::Remove { name })
            .await
    }

    /// Persistently enable or disable one definition in the selected layer.
    pub async fn persistent_set_enabled(
        &self,
        scope: McpPersistentScope,
        name: String,
        enabled: bool,
    ) -> Result<McpMutationResult, McpControlError> {
        self.persist(scope, McpPersistentMutation::SetEnabled { name, enabled })
            .await
    }

    /// Persistently enable one definition in the selected layer.
    pub async fn persistent_enable(
        &self,
        scope: McpPersistentScope,
        name: String,
    ) -> Result<McpMutationResult, McpControlError> {
        self.persistent_set_enabled(scope, name, true).await
    }

    /// Persistently disable one definition in the selected layer.
    pub async fn persistent_disable(
        &self,
        scope: McpPersistentScope,
        name: String,
    ) -> Result<McpMutationResult, McpControlError> {
        self.persistent_set_enabled(scope, name, false).await
    }

    /// Approve exactly the effective project definition's fingerprint.
    pub async fn approve(&self, name: String) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::Approve(name)).await
    }

    /// Revoke remembered approval for one project/server name.
    pub async fn revoke(&self, name: String) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::Revoke(name)).await
    }

    /// Reload disk-backed layers while preserving CLI and session state.
    pub async fn reload(&self) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::Reload).await
    }

    async fn persist(
        &self,
        scope: McpPersistentScope,
        mutation: McpPersistentMutation,
    ) -> Result<McpMutationResult, McpControlError> {
        self.mutation(Command::Persist { scope, mutation }).await
    }

    async fn mutation(&self, command: Command) -> Result<McpMutationResult, McpControlError> {
        match self.request(command).await? {
            McpControlResponse::Mutation(result) => Ok(result),
            _ => Err(McpControlError::Protocol),
        }
    }

    async fn request(&self, command: Command) -> Result<McpControlResponse, McpControlError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Envelope { command, reply })
            .await
            .map_err(|_send_error| McpControlError::Unavailable)?;
        receiver
            .await
            .map_err(|_receive_error| McpControlError::Unavailable)?
    }
}

impl fmt::Debug for McpControlHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpControlHandle { .. }")
    }
}

pub(super) enum Command {
    List,
    Inspect(String),
    SessionAdd {
        name: String,
        definition: McpServerSettings,
    },
    SessionRemove(String),
    SessionDisable(String),
    SessionEnable(String),
    Persist {
        scope: McpPersistentScope,
        mutation: McpPersistentMutation,
    },
    Approve(String),
    Revoke(String),
    Reload,
    RefreshTools {
        name: String,
        instance_id: u64,
        revision: u64,
    },
}

pub(super) struct Envelope {
    pub(super) command: Command,
    pub(super) reply: oneshot::Sender<Result<McpControlResponse, McpControlError>>,
}

#[cfg(test)]
#[path = "mcp_control_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_control_refresh_tests.rs"]
mod refresh_tests;
