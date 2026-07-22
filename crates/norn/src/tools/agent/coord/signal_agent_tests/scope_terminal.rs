use std::sync::Arc;

use uuid::Uuid;

use super::super::*;
use super::test_support::{
    build_infra, child_infra, ctx_with, envelope_for, register_agent, send_args,
};
use crate::agent::child_policy::MessagingScope;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentRegistry;
use crate::r#loop::inbound::inbound_channel;
use crate::provider::mock::MockProvider;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tools::agent::AgentToolInfra;

/// A root sender has no parent: `"parent"` fails typed, with no
/// delivery attempt.
#[tokio::test]
async fn signal_agent_parent_literal_fails_for_root_sender() {
    let (infra, _registry, _router) = build_infra(Uuid::new_v4());
    let sender_store = Arc::clone(&infra.event_store);
    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("parent", "steer", "hi")),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(out.is_error());
    assert_eq!(out.content["delivered"], false);
    let message = out.content["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("no parent"),
        "the failure names the missing parent: {message}",
    );
    assert!(
        sender_store.events().is_empty(),
        "no Sent for a rejected send"
    );
}

/// Scope matrix — `parent_only`: a sibling target is denied with a
/// structured failure naming the granted scope; nothing is delivered
/// and no Sent record is written to either store.
#[tokio::test]
async fn signal_agent_denies_sibling_under_parent_only() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let parent = register_agent(&registry, "/orchestrator", None);
    let sender = register_agent(&registry, "/orchestrator/worker-a", Some(parent));
    let sibling = register_agent(&registry, "/orchestrator/worker-b", Some(parent));
    let parent_store = Arc::new(EventStore::new());
    let infra = child_infra(
        sender,
        parent,
        MessagingScope::ParentOnly,
        &registry,
        &router,
        &parent_store,
    );
    let sender_store = Arc::clone(&infra.event_store);
    let (tx, mut rx) = inbound_channel(8);
    router.register(sibling, tx);

    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(
                "signal_agent",
                send_args("/orchestrator/worker-b", "steer", "psst"),
            ),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(out.is_error(), "out-of-scope send must fail");
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["scope"], "parent_only");
    let message = out.content["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("parent_only"),
        "the denial names the granted scope: {message}",
    );
    assert!(rx.drain().is_empty(), "nothing may be delivered");
    assert!(
        sender_store.events().is_empty(),
        "no Sent in the sender store"
    );
    assert!(
        parent_store.events().is_empty(),
        "no Sent in the parent store"
    );
}

/// Scope matrix — `siblings_and_parent`: an agent under a *different*
/// parent is out of scope (one audited hop at a time).
#[tokio::test]
async fn signal_agent_denies_non_sibling_under_siblings_and_parent() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let parent = register_agent(&registry, "/orchestrator", None);
    let other_parent = register_agent(&registry, "/other", None);
    let sender = register_agent(&registry, "/orchestrator/worker", Some(parent));
    let stranger = register_agent(&registry, "/other/worker", Some(other_parent));
    let parent_store = Arc::new(EventStore::new());
    let infra = child_infra(
        sender,
        parent,
        MessagingScope::SiblingsAndParent,
        &registry,
        &router,
        &parent_store,
    );
    let (tx, mut rx) = inbound_channel(8);
    router.register(stranger, tx);

    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("/other/worker", "update", "hi")),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(out.is_error());
    assert_eq!(out.content["scope"], "siblings_and_parent");
    assert!(rx.drain().is_empty());
}

/// Scope matrix — root sender: an agent that is not the root's own
/// child (here a parentless peer) is out of scope.
#[tokio::test]
async fn signal_agent_root_sender_limited_to_own_children() {
    let sender = Uuid::new_v4();
    let (infra, registry, router) = build_infra(sender);
    let peer = register_agent(&registry, "/peer", None);
    let (tx, mut rx) = inbound_channel(8);
    router.register(peer, tx);

    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("/peer", "steer", "hi")),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(out.is_error());
    assert_eq!(out.content["scope"], "root");
    let message = out.content["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("own children"),
        "the denial states the structural root rule: {message}",
    );
    assert!(rx.drain().is_empty());
}

/// Defense-in-depth: with `MessagingScope::None` the tool is absent
/// from the child's surface, but a context that reaches execute anyway
/// is refused.
#[tokio::test]
async fn signal_agent_scope_none_is_refused_at_execute() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let parent = register_agent(&registry, "/orchestrator", None);
    let sender = register_agent(&registry, "/orchestrator/mute", Some(parent));
    let parent_store = Arc::new(EventStore::new());
    let infra = child_infra(
        sender,
        parent,
        MessagingScope::None,
        &registry,
        &router,
        &parent_store,
    );

    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("parent", "steer", "hi")),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(out.is_error());
    assert_eq!(out.content["scope"], "none");
}

/// Configuration violation: a sender with a parent but no granted
/// policy is a harness wiring error, surfaced as a typed hard error —
/// never an invented scope.
#[tokio::test]
async fn signal_agent_missing_policy_on_child_is_config_error() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let parent = register_agent(&registry, "/orchestrator", None);
    let sender = register_agent(&registry, "/orchestrator/worker", Some(parent));
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&registry),
        router,
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: sender,
        parent_id: Some(parent),
        grant: None,
        tool_registry: None,
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });

    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    let err = tool
        .execute(
            &envelope_for("signal_agent", send_args("parent", "steer", "hi")),
            &ctx,
        )
        .await
        .expect_err("missing granted policy must be a hard error");
    match err {
        ToolError::ExecutionFailed { reason } => {
            assert!(
                reason.contains("ChildPolicy"),
                "the error names the missing grant: {reason}",
            );
        }
        other => panic!("expected ExecutionFailed, got {other:?}"),
    }
}

/// Messaging an agent that already finished — whether its terminal
/// entry is still listed or it was reclaimed down to a tombstone —
/// fails honestly with the recorded completion, never the dishonest
/// "not registered".
#[tokio::test]
async fn signal_agent_reports_finished_recipient_honestly() {
    let sender = Uuid::new_v4();
    let (infra, registry, _router) = build_infra(sender);
    let recipient = register_agent(&registry, "/parent/done-child", Some(sender));
    registry
        .write()
        .mark_completed(recipient)
        .expect("complete");

    let ctx = ctx_with(infra);
    let tool = SignalAgentTool::new();
    // Terminal-but-unreclaimed: resolvable by path despite the freed
    // path index, and reported as finished.
    let out = tool
        .execute(
            &envelope_for(
                "signal_agent",
                send_args("/parent/done-child", "update", "hi"),
            ),
            &ctx,
        )
        .await
        .expect("executes");
    assert!(out.is_error(), "delivery to a finished agent must fail");
    assert_eq!(out.content["delivered"], false);
    assert_eq!(out.content["recipient_status"], "completed");
    let message = out.content["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("already finished") && message.contains("completed at"),
        "the failure must state the recorded completion: {message}",
    );

    // Reclaimed: the tombstone keeps the truth available, by path and
    // by UUID.
    assert!(registry.write().remove_terminal(recipient), "reclaim");
    for identifier in ["/parent/done-child".to_string(), recipient.to_string()] {
        let out = tool
            .execute(
                &envelope_for("signal_agent", send_args(&identifier, "update", "hi")),
                &ctx,
            )
            .await
            .expect("executes");
        assert!(out.is_error());
        assert_eq!(out.content["delivered"], false);
        assert_eq!(out.content["recipient_status"], "completed");
        assert!(
            out.content["completed_at"].as_str().is_some(),
            "the completion timestamp must surface: {:?}",
            out.content,
        );
        let message = out.content["error"]["message"].as_str().expect("message");
        assert!(
            !message.contains("not registered"),
            "'not registered' is reserved for agents that never existed: {message}",
        );
    }
}

// -- W3.4: scope composition across deeper trees -------------------------

/// Depth ≥ 2: a grandchild with `siblings_and_parent` reaches its
/// sibling and its mid-tree parent — never the root, which sits two
/// hops up (escalation crosses one audited hop at a time).
#[tokio::test]
async fn grandchild_scope_reaches_sibling_and_parent_never_root() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let root = register_agent(&registry, "/root", None);
    let child = register_agent(&registry, "/root/c", Some(root));
    let g1 = register_agent(&registry, "/root/c/g1", Some(child));
    let g2 = register_agent(&registry, "/root/c/g2", Some(child));

    let parent_store = Arc::new(EventStore::new());
    let ctx = ctx_with(child_infra(
        g1,
        child,
        MessagingScope::SiblingsAndParent,
        &registry,
        &router,
        &parent_store,
    ));
    let tool = SignalAgentTool::new();

    // Sibling at depth 2: delivered, attributed from registry ground
    // truth.
    let (sib_tx, mut sib_rx) = inbound_channel(8);
    router.register(g2, sib_tx);
    let out = tool
        .execute(
            &envelope_for(
                "signal_agent",
                send_args("/root/c/g2", "steer", "hello sibling"),
            ),
            &ctx,
        )
        .await
        .expect("send");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["delivered"], true);
    let drained = sib_rx.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].from, "/root/c/g1");

    // The mid-tree parent (one hop up) via the literal "parent".
    let (parent_tx, mut parent_rx) = inbound_channel(8);
    router.register(child, parent_tx);
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("parent", "update", "status")),
            &ctx,
        )
        .await
        .expect("send");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(parent_rx.drain().len(), 1);

    // The root (two hops up): refused with the typed scope denial —
    // even though the root is live and routed.
    let (root_tx, mut root_rx) = inbound_channel(8);
    router.register(root, root_tx);
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("/root", "steer", "escalate!")),
            &ctx,
        )
        .await
        .expect("execute returns structured failure");
    assert!(out.is_error(), "{:?}", out.content);
    let payload = out.error().expect("typed payload");
    assert_eq!(payload.kind, ToolErrorKind::PermissionDenied);
    assert!(
        payload.message.contains("siblings_and_parent"),
        "the denial names the granted scope: {}",
        payload.message,
    );
    assert!(
        root_rx.drain().is_empty(),
        "nothing may be enqueued for an out-of-scope recipient",
    );
}

/// Depth ≥ 2: a mid-tree child granted `parent_only` reaches the root
/// (its parent) and nothing else — not its own sibling, not its own
/// grandchild-level children.
#[tokio::test]
async fn mid_tree_parent_only_reaches_root_and_nothing_else() {
    let registry = AgentRegistry::shared();
    let router = Arc::new(MessageRouter::new());
    let root = register_agent(&registry, "/root", None);
    let child = register_agent(&registry, "/root/c", Some(root));
    let sibling = register_agent(&registry, "/root/c2", Some(root));
    let grandchild = register_agent(&registry, "/root/c/g", Some(child));

    let parent_store = Arc::new(EventStore::new());
    let ctx = ctx_with(child_infra(
        child,
        root,
        MessagingScope::ParentOnly,
        &registry,
        &router,
        &parent_store,
    ));
    let tool = SignalAgentTool::new();

    // Parent (the root) is reachable.
    let (root_tx, mut root_rx) = inbound_channel(8);
    router.register(root, root_tx);
    let out = tool
        .execute(
            &envelope_for("signal_agent", send_args("parent", "steer", "to root")),
            &ctx,
        )
        .await
        .expect("send");
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(root_rx.drain().len(), 1);

    // Sibling and own child are both out of scope under parent_only.
    for target in ["/root/c2", "/root/c/g"] {
        let out = tool
            .execute(
                &envelope_for("signal_agent", send_args(target, "update", "nope")),
                &ctx,
            )
            .await
            .expect("structured failure");
        assert!(out.is_error(), "{target} must be refused");
        let payload = out.error().expect("typed payload");
        assert_eq!(payload.kind, ToolErrorKind::PermissionDenied);
        assert!(
            payload.message.contains("parent_only"),
            "the denial names the granted scope: {}",
            payload.message,
        );
    }
    let _ = (sibling, grandchild);
}
