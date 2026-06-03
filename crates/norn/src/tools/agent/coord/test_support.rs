//! Shared fixtures for `coord` submodule tests.

#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_const_for_fn,
    clippy::clone_on_ref_ptr,
    clippy::uninlined_format_args
)]

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::watch;
use uuid::Uuid;

use crate::agent::mailbox::Mailbox;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::r#loop::inbound::{InboundChannel, inbound_channel};
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
use crate::tools::agent::handle::AgentHandle;
use crate::tools::agent::infra::AgentToolInfra;

/// Build a tool envelope wrapping `args` for tool `tool`.
pub(crate) fn envelope_for(tool: &str, args: serde_json::Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-1".to_string(),
        tool_name: tool.to_string(),
        model_args: args,
        runtime_inputs: RuntimeInputs::default(),
        metadata: serde_json::Value::Null,
    }
}

/// Build an [`AgentToolInfra`] with a fresh registry / mailbox / event
/// store keyed to `sender` as the calling agent.
pub(crate) fn build_infra(
    sender: Uuid,
) -> (
    Arc<AgentToolInfra>,
    Arc<RwLock<AgentRegistry>>,
    Arc<Mailbox>,
) {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
    let registry = AgentRegistry::shared();
    let mailbox = Arc::new(Mailbox::new());
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&registry),
        mailbox: Arc::clone(&mailbox),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: sender,
        parent_id: None,
        tool_registry: None,
    });
    (infra, registry, mailbox)
}

/// Register an agent at `path` with optional parent, returning its id.
pub(crate) fn register_agent(
    registry: &Arc<RwLock<AgentRegistry>>,
    path: &str,
    parent: Option<Uuid>,
) -> Uuid {
    let guard = AgentRegistry::reserve(
        registry,
        path.to_string(),
        "worker".to_string(),
        "claude".to_string(),
        parent,
    )
    .unwrap();
    let id = guard.id();
    guard.confirm().unwrap();
    id
}

/// Build a synthetic `AgentHandle` for `id` with an initial status of
/// [`AgentStatus::Active`]. Returns the handle plus its status sender and
/// inbound receiver so a test can drive transitions and observe messages.
pub(crate) fn synthetic_handle(
    id: Uuid,
) -> (AgentHandle, watch::Sender<AgentStatus>, InboundChannel) {
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let (inbound_tx, inbound_rx) = inbound_channel(8);
    // The join handle must be real because `abort()` is invoked during
    // DFS shutdown. A long-sleeping task is fine — it lives only as long
    // as the test.
    let join_handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_mins(1)).await;
    });
    (
        AgentHandle {
            agent_id: id,
            status_rx,
            inbound_tx,
            join_handle,
            event_store: Arc::new(crate::session::store::EventStore::new()),
            branch_metadata: crate::tools::agent::handle::ChildBranchMetadata {
                child_agent_id: id,
                parent_agent_id: Uuid::new_v4(),
                profile_name: None,
                spawned_at: chrono::Utc::now(),
            },
        },
        status_tx,
        inbound_rx,
    )
}
