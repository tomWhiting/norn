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
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::r#loop::inbound::{InboundChannel, inbound_channel};
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tool::envelope::ToolEnvelope;
use crate::tools::agent::handle::AgentHandle;
use crate::tools::agent::infra::AgentToolInfra;

/// Build a tool envelope wrapping `args` for tool `tool`.
pub(crate) fn envelope_for(tool: &str, args: serde_json::Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-1".to_string(),
        tool_name: tool.to_string(),
        model_args: args,
        metadata: serde_json::Value::Null,
    }
}

/// Build an [`AgentToolInfra`] with a fresh registry / router / event
/// store keyed to `sender` as the calling agent.
pub(crate) fn build_infra(
    sender: Uuid,
) -> (
    Arc<AgentToolInfra>,
    Arc<RwLock<AgentRegistry>>,
    Arc<MessageRouter>,
) {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&registry),
        router: Arc::clone(&router),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: sender,
        parent_id: None,
        grant: None,
        tool_registry: None,
    });
    (infra, registry, router)
}

/// Generous test [`ChildPolicy`] (depth 5) — a deliberate test-caller
/// choice, never a library default.
pub(crate) fn test_root_policy() -> crate::agent::child_policy::ChildPolicy {
    use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
    ChildPolicy {
        messaging: MessagingScope::SiblingsAndParent,
        delegation: DelegationBudget {
            remaining_depth: 5,
            max_concurrent_children: 32,
        },
        inbound_capacity: 32,
        loop_config: None,
    }
}

/// Register an agent at `path` with optional parent, returning its id.
///
/// The entry's granted policy derives from its registered parent's policy
/// (inherit-with-decrement) when one exists, else the generous
/// [`test_root_policy`], so chains register to any depth the tests need.
pub(crate) fn register_agent(
    registry: &Arc<RwLock<AgentRegistry>>,
    path: &str,
    parent: Option<Uuid>,
) -> Uuid {
    let root_policy = test_root_policy();
    let policy = match parent {
        None => root_policy.clone(),
        Some(p) => {
            let base = registry
                .read()
                .get(p)
                .map_or_else(|| root_policy.clone(), |entry| entry.policy);
            base.grant_for_child(None).unwrap()
        }
    };
    let guard = AgentRegistry::reserve(
        registry,
        path.to_string(),
        "worker".to_string(),
        "claude".to_string(),
        parent,
        policy,
        Some(&root_policy),
    )
    .unwrap();
    let id = guard.id();
    guard.confirm().unwrap();
    id
}

/// Build a synthetic `AgentHandle` for `id` with an initial status of
/// [`AgentStatus::Active`]. Returns the handle plus its status sender and
/// inbound receiver so a test can drive transitions and observe messages.
///
/// The synthetic wrapper task parks until the handle's cancellation
/// token fires (with a one-minute test-hang backstop), then exits
/// **without** performing any registry transition — modelling a wrapper
/// that dies before its terminal mark, which is exactly the window where
/// `close_agent` must own the forced-failure record.
pub(crate) fn synthetic_handle(
    id: Uuid,
) -> (AgentHandle, watch::Sender<AgentStatus>, InboundChannel) {
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    let (inbound_tx, inbound_rx) = inbound_channel(8);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task_cancel = cancel.clone();
    // The join handle must be real because `close_agent` joins it during
    // DFS shutdown. The task observes the cancellation token like a real
    // child run would; the sleep arm only bounds a defective test.
    let join_handle = tokio::spawn(async move {
        tokio::select! {
            () = task_cancel.cancelled() => {}
            () = tokio::time::sleep(Duration::from_mins(1)) => {}
        }
    });
    (
        AgentHandle {
            agent_id: id,
            status_rx,
            inbound_tx,
            wake_tx: mpsc::channel(1).0,
            wake_pending: Arc::new(AtomicBool::new(false)),
            cancel,
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
