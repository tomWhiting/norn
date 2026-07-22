use super::*;

/// Hook-coverage regression (reviewer issue): a `PreToolUse` hook
/// registered on the parent must observe a fork's tool calls — the
/// fork's loop dispatches hooks from its own `LoopContext`, so the
/// parent's registry must be forwarded.
#[tokio::test]
async fn parent_pre_tool_hook_fires_for_fork_tool_call() -> TestResult {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};

    struct CountingPreTool {
        tool_name: &'static str,
        count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PreToolHook for CountingPreTool {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == self.tool_name {
                self.count.fetch_add(1, AtomicOrdering::SeqCst);
            }
            HookOutcome::Proceed
        }
    }

    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("identity".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let turn2 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "structured-out".to_string(),
            call_id: None,
            name: Some("structured_output".to_string()),
            arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(IdentityStubTool {
        seen_agent: Arc::new(StdMutex::new(None)),
        seen_parent: Arc::new(StdMutex::new(None)),
    }));
    let tool_registry = Arc::new(tool_registry);

    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );
    let count = Arc::new(AtomicUsize::new(0));
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::PreTool(Box::new(CountingPreTool {
        tool_name: "identity",
        count: Arc::clone(&count),
    })));
    ctx.insert_extension(Arc::new(hook_registry));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "probe it", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    assert_eq!(
        count.load(AtomicOrdering::SeqCst),
        1,
        "a parent-registered PreToolUse hook must fire for the fork's tool call",
    );
    Ok(())
}

/// Typed lifecycle: fork emits `SubagentLifecycle::Started` then
/// `Completed` on the shared broadcast channel — child-tagged, with
/// the fork descriptor, ordered wall-clock timestamps, and the
/// fork's accumulated usage — appends the matching Custom audit
/// events to the parent's store, and the result channel carries the
/// same per-child usage.
#[tokio::test]
async fn fork_emits_typed_lifecycle_events_on_channel_and_parent_store() -> TestResult {
    use crate::agent::result_channel::ChildResultSender;
    use crate::provider::agent_event::{
        AgentEvent, AgentEventKind, SUBAGENT_COMPLETED_EVENT_TYPE, SUBAGENT_STARTED_EVENT_TYPE,
        SharedAgentEventChannel, SubagentKind, SubagentLifecycle,
    };

    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (btx, mut brx) = tokio::sync::broadcast::channel::<AgentEvent>(64);
    ctx.insert_extension(Arc::new(SharedAgentEventChannel(btx)));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "summarise", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let path = required(
        out.content.get("path").and_then(serde_json::Value::as_str),
        "fork path",
    )?;
    assert!(
        path.contains("/fork/"),
        "fork output carries the registry path: {}",
        out.content,
    );
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    // Live carrier: child-tagged Started then Completed, with the
    // Started event preceding the fork's own provider events.
    let mut subagent_events = Vec::new();
    let mut first_child_event_is_started = None;
    while let Ok(ev) = brx.try_recv() {
        if ev.agent_id == fork_id && first_child_event_is_started.is_none() {
            first_child_event_is_started = Some(matches!(
                ev.event,
                AgentEventKind::Subagent(SubagentLifecycle::Started { .. })
            ));
        }
        if let AgentEventKind::Subagent(lifecycle) = ev.event {
            assert_eq!(ev.agent_id, fork_id, "lifecycle events are child-tagged");
            assert_eq!(ev.agent_role.as_ref(), "fork/gpt-5.5");
            subagent_events.push(lifecycle);
        }
    }
    assert_eq!(
        first_child_event_is_started,
        Some(true),
        "Started must precede the fork's own provider events",
    );
    assert_eq!(subagent_events.len(), 2, "exactly Started then Completed");
    let SubagentLifecycle::Started {
        parent_id,
        child_id,
        descriptor,
        ..
    } = &subagent_events[0]
    else {
        return Err(test_error(format!(
            "expected Started, got {:?}",
            subagent_events[0]
        )));
    };
    assert_eq!(*parent_id, parent);
    assert_eq!(*child_id, fork_id);
    assert_eq!(descriptor.kind, SubagentKind::Fork);
    assert_eq!(descriptor.role, "fork");
    assert_eq!(descriptor.model, "gpt-5.5");
    assert!(descriptor.profile.is_none(), "forks have no profile");

    let SubagentLifecycle::Completed {
        parent_id,
        child_id,
        started_at,
        completed_at,
        usage,
        subtree_usage,
        succeeded,
        error,
        stop,
        ..
    } = &subagent_events[1]
    else {
        return Err(test_error(format!(
            "expected Completed, got {:?}",
            subagent_events[1]
        )));
    };
    assert_eq!(*parent_id, parent);
    assert_eq!(*child_id, fork_id);
    assert!(*completed_at >= *started_at, "timestamps must be ordered");
    assert!(*succeeded);
    assert!(error.is_none());
    assert!(stop.is_none());
    assert_eq!(usage.input_tokens, 5, "per-fork usage must surface");
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(
        subtree_usage.input_tokens, 5,
        "a childless fork's subtree usage equals its own usage",
    );
    assert_eq!(subtree_usage.output_tokens, 2);

    // Audit carrier: the parent store got both Custom events (in
    // addition to the existing ForkComplete completion reference).
    let custom: Vec<(String, serde_json::Value)> = parent_store
        .events()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } => Some((event_type, data)),
            _ => None,
        })
        .collect();
    assert_eq!(custom.len(), 2, "started + completed audit events");
    assert_eq!(custom[0].0, SUBAGENT_STARTED_EVENT_TYPE);
    assert_eq!(custom[0].1["descriptor"]["kind"], "fork");
    assert_eq!(custom[1].0, SUBAGENT_COMPLETED_EVENT_TYPE);
    assert_eq!(custom[1].1["succeeded"], true);
    assert!(
        parent_store
            .events()
            .iter()
            .any(|e| matches!(e, SessionEvent::ForkComplete { .. })),
        "the ForkComplete completion reference is still appended",
    );

    // The result channel carries the same per-fork usage, and the
    // childless fork's subtree total equals its own usage (W3.6).
    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, fork_id);
    assert_eq!(result.usage.input_tokens, 5);
    assert_eq!(result.usage.output_tokens, 2);
    assert_eq!(result.subtree_usage.input_tokens, 5);
    assert_eq!(result.subtree_usage.output_tokens, 2);
    Ok(())
}

/// Terminal-transition race repro (production WARNs
/// `fork: mark_completing failed ... agent not found` /
/// `fork: mark_completed failed ... agent not found`):
///
/// The fork's completion wrapper owns the terminal sequence
/// mark → `ForkComplete` → lifecycle → delivery → reclaim. This test
/// parks the wrapper deterministically *after* the fork's run has
/// finished and *before* `mark_fork_terminal`, by gating
/// `SubagentHook::on_subagent_stop` (the only await between the two).
/// While the wrapper is parked, a `close_agent` issued by an agent
/// that holds NO handle for the fork targets it.
///
/// Before the fix, `close_agent` marked the still-Active entry
/// Completing → Completed and removed it — stealing the wrapper's
/// terminal transition — so the wrapper's own mark hit `NotFound`
/// (the production WARN pair) and the closer falsified the fork's
/// recorded outcome. After the fix, a closer that cannot stop the
/// fork's task (no handle) must not touch its live registry entry:
/// the entry survives the close, and the wrapper's terminal mark
/// lands exactly once.
#[tokio::test]
async fn close_without_handle_cannot_steal_fork_terminal_transition() -> TestResult {
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};
    use crate::tools::agent::coord::CloseAgentTool;

    struct GateStopHook {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl SubagentHook for GateStopHook {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            self.entered.notify_one();
            self.release.notified().await;
            HookOutcome::Proceed
        }
    }

    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::Subagent(Box::new(GateStopHook {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    })));
    ctx.insert_extension(Arc::new(hook_registry));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "race", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;

    // The wrapper is now parked inside on_subagent_stop: the fork's
    // run is finished, the terminal mark has NOT happened yet.
    // (`notify_one` stores a permit, so this is race-free even if the
    // wrapper reached the gate before we subscribed.)
    entered.notified().await;

    // A different agent — same registry, no handle for the fork —
    // closes it while the wrapper still owes the terminal mark.
    let closer_provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let closer_infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&agent_registry),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider: closer_provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: None,
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });
    let closer_ctx = ToolContext::empty();
    closer_ctx.insert_extension(closer_infra);
    let close_out = CloseAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "close-1".to_string(),
                tool_name: "close_agent".to_string(),
                model_args: json!({"agent_id": fork_id.to_string()}),
                metadata: serde_json::Value::Null,
            },
            &closer_ctx,
        )
        .await?;

    // INVARIANT: the wrapper still owes this entry a terminal
    // transition, so the close must not have removed it. Before the
    // fix the entry is gone here — the wrapper's subsequent
    // mark_completing/mark_completed hit NotFound (the WARN pair).
    assert!(
        agent_registry.read().get(fork_id).is_some(),
        "a closer without the fork's handle must never remove the live \
         registry entry out from under the completion wrapper; close output: {:?}",
        close_out.content,
    );
    assert_eq!(
        close_out.content["shut_down"][0]["status"], "unreachable",
        "close must report honestly that it cannot force-stop an agent \
         whose handle it does not hold: {:?}",
        close_out.content,
    );

    // Release the wrapper and let it finish its terminal sequence.
    release.notify_one();
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    // The wrapper's mark landed exactly once: Completed, observable.
    let completed_entry = required(
        agent_registry.read().get(fork_id),
        "the wrapper's terminal transition must find its entry",
    )?;
    assert_eq!(completed_entry.status, AgentStatus::Completed);
    Ok(())
}
