//! Shared fixtures for the signal-agent behavioral suites.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentRegistry;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tools::agent::AgentToolInfra;
use crate::tools::agent::coord::test_support as coord_support;
use crate::tools::agent::infra::ParentGrant;

pub(super) use coord_support::{build_infra, envelope_for, register_agent};

pub(super) struct TestMailbox {
    pub(super) ctx: ToolContext,
    pub(super) store: Arc<EventStore>,
    pub(super) binding: crate::session::SessionBinding,
    _lease: Arc<crate::agent::PendingMailboxLease>,
}

/// Granted policy with `scope`, documented-proposal budgets.
fn policy(scope: MessagingScope) -> ChildPolicy {
    ChildPolicy {
        messaging: scope,
        delegation: DelegationBudget {
            remaining_depth: 1,
            max_concurrent_children: 32,
        },
        inbound_capacity: 32,
        loop_config: None,
    }
}

/// Infra for a child sender with the parent's dual-store audit grant.
pub(super) fn child_infra(
    sender: Uuid,
    parent: Uuid,
    scope: MessagingScope,
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    router: &Arc<MessageRouter>,
    parent_store: &Arc<EventStore>,
) -> Arc<AgentToolInfra> {
    let provider: Arc<dyn crate::provider::traits::Provider> =
        Arc::new(crate::provider::mock::MockProvider::new(vec![]));
    Arc::new(AgentToolInfra {
        registry: Arc::clone(registry),
        router: Arc::clone(router),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: sender,
        parent_id: Some(parent),
        grant: Some(ParentGrant {
            policy: policy(scope),
            parent_store: Arc::clone(parent_store),
        }),
        tool_registry: None,
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    })
}

pub(super) fn ctx_with(infra: Arc<AgentToolInfra>) -> ToolContext {
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);
    ctx
}

pub(super) fn ctx_with_mailbox(infra: Arc<AgentToolInfra>, recipient: Uuid) -> TestMailbox {
    let mailbox = Arc::new(EventStore::new());
    let binding = crate::session::SessionBinding::ephemeral_root();
    let lease = Arc::new(crate::agent::PendingMailboxLease::new());
    infra
        .pending_messages
        .register_child_mailbox(recipient, binding.mailbox_id(), &mailbox, &lease)
        .expect("register recipient mailbox");
    TestMailbox {
        ctx: ctx_with(infra),
        store: mailbox,
        binding,
        _lease: lease,
    }
}

pub(super) fn queued_authorities(store: &EventStore) -> Vec<bool> {
    store
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE => data
                .get("authoritative")
                .and_then(serde_json::Value::as_bool),
            _ => None,
        })
        .collect()
}

pub(super) fn queued_mailbox_ids(store: &EventStore) -> Vec<serde_json::Value> {
    store
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == crate::agent::AGENT_MESSAGE_QUEUED_EVENT_TYPE => {
                data.get("mailbox_id").cloned()
            }
            _ => None,
        })
        .collect()
}

pub(super) fn send_args(to: &str, kind: &str, content: &str) -> serde_json::Value {
    json!({ "to": to, "kind": kind, "content": content })
}
