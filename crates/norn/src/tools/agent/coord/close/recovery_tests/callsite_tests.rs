use super::*;

fn handle_creating_live_entry_recovery(
    recipient_id: Uuid,
    pending_messages: &Arc<PendingAgentMessages>,
    router: Arc<MessageRouter>,
    store: Arc<EventStore>,
    content: &str,
) -> TestResult<AgentHandle> {
    let lease = Arc::new(PendingMailboxLease::new());
    pending_messages.register_child_mailbox(
        recipient_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let (route_tx, mut route_inbound) = inbound_channel(2);
    router.register(recipient_id, route_tx.clone());
    route_tx
        .try_send(terminal_message(recipient_id, content))
        .map_err(|error| test_failure(format!("accept terminal message: {error}")))?;

    let (inbound_tx, inbound_rx): (_, InboundChannel) = inbound_channel(1);
    drop(inbound_rx);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    drop(status_tx);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_pending_messages = Arc::clone(pending_messages);
    let task_store = Arc::clone(&store);
    let join_handle = tokio::spawn(async move {
        task_cancel.cancelled().await;
        let transition = task_pending_messages.transition_live_route(
            recipient_id,
            task_store.as_ref(),
            router.as_ref(),
            &mut route_inbound,
            Some(&lease),
        );
        assert!(
            transition
                .as_ref()
                .is_ok_and(|transition| transition.first_error.is_some()),
            "terminal transition must retain recovery",
        );
    });

    Ok(AgentHandle {
        agent_id: recipient_id,
        status_rx,
        inbound_tx,
        wake_tx: mpsc::channel(1).0,
        wake_pending: Arc::new(AtomicBool::new(false)),
        cancel,
        join_handle,
        event_store: store,
        branch_metadata: ChildBranchMetadata {
            child_agent_id: recipient_id,
            parent_agent_id: Uuid::new_v4(),
            profile_name: None,
            spawned_at: Utc::now(),
        },
    })
}

fn descendant_handle_creating_parent_recovery(
    descendant_id: Uuid,
    parent_id: Uuid,
    registry: Arc<parking_lot::RwLock<AgentRegistry>>,
    pending_messages: &Arc<PendingAgentMessages>,
    router: Arc<MessageRouter>,
    store: Arc<EventStore>,
    content: &str,
) -> TestResult<AgentHandle> {
    let lease = Arc::new(PendingMailboxLease::new());
    pending_messages.register_child_mailbox(
        parent_id,
        SessionBinding::ephemeral_root().mailbox_id(),
        &store,
        &lease,
    )?;
    let (route_tx, mut route_inbound) = inbound_channel(2);
    router.register(parent_id, route_tx.clone());
    route_tx
        .try_send(terminal_message(parent_id, content))
        .map_err(|error| test_failure(format!("accept parent terminal message: {error}")))?;

    let (inbound_tx, inbound_rx): (_, InboundChannel) = inbound_channel(1);
    drop(inbound_rx);
    let (status_tx, status_rx) = watch::channel(AgentStatus::Active);
    drop(status_tx);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_pending_messages = Arc::clone(pending_messages);
    let task_store = Arc::clone(&store);
    let join_handle = tokio::spawn(async move {
        task_cancel.cancelled().await;
        let transition = task_pending_messages.transition_live_route(
            parent_id,
            task_store.as_ref(),
            router.as_ref(),
            &mut route_inbound,
            Some(&lease),
        );
        assert!(
            transition
                .as_ref()
                .is_ok_and(|transition| transition.first_error.is_some()),
            "terminal transition must retain parent recovery",
        );
        let mut registry = registry.write();
        let parent_mark = registry.mark_completed(parent_id);
        assert!(
            parent_mark.is_ok(),
            "parent must publish terminal status after its drain",
        );
        let descendant_mark = registry.mark_completed(descendant_id);
        assert!(
            descendant_mark.is_ok(),
            "descendant wrapper must publish terminal status",
        );
    });

    Ok(AgentHandle {
        agent_id: descendant_id,
        status_rx,
        inbound_tx,
        wake_tx: mpsc::channel(1).0,
        wake_pending: Arc::new(AtomicBool::new(false)),
        cancel,
        join_handle,
        event_store: store,
        branch_metadata: ChildBranchMetadata {
            child_agent_id: descendant_id,
            parent_agent_id: parent_id,
            profile_name: None,
            spawned_at: Utc::now(),
        },
    })
}

fn assert_payload_free_recovery_error(error: ToolError, id: Uuid, private: &str) -> TestResult {
    let ToolError::ExecutionFailed { reason } = error else {
        return Err(test_failure(format!(
            "expected typed execution failure, got {error:?}"
        )));
    };
    assert!(reason.contains(&id.to_string()));
    assert!(reason.contains("1 accepted message"));
    assert!(!reason.contains(private));
    assert!(!reason.contains("injected terminal queue outage"));
    Ok(())
}

#[tokio::test]
async fn close_agent_unconditionally_checks_recovery_after_join_for_live_entry() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let child = register_agent(&registry, "/recovery/post-join-live", None);
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    let handles = Arc::new(AgentHandles::new());
    handles.insert(handle_creating_live_entry_recovery(
        child,
        &infra.pending_messages,
        router,
        Arc::clone(&store),
        "private live-entry payload",
    )?);

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    ctx.insert_extension(Arc::clone(&handles));
    let result = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": child.to_string()})),
            &ctx,
        )
        .await;
    let Err(error) = result else {
        return Err(test_failure("post-join recovery check was bypassed"));
    };

    assert_payload_free_recovery_error(error, child, "private live-entry payload")?;
    assert!(!handles.contains(child), "joined handle is consumed");
    assert_eq!(
        registry.read().get(child).map(|entry| entry.status),
        Some(AgentStatus::Active),
        "recovery failure occurs before forced-failure mutation",
    );
    assert!(registry.read().tombstone(child).is_none());
    assert_eq!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(child)
            .map(|status| status.pending_count),
        Some(1),
    );
    assert_eq!(store.len(), 0);
    Ok(())
}

#[tokio::test]
async fn close_agent_preflights_terminal_recovery_across_the_whole_subtree() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let root = register_agent(&registry, "/recovery/subtree", None);
    let descendant = register_agent(&registry, "/recovery/subtree/descendant", Some(root));
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    retain_terminal_recovery(
        &infra.pending_messages,
        router.as_ref(),
        descendant,
        &store,
        "private descendant payload",
    )?;

    let handles = Arc::new(AgentHandles::new());
    let (root_handle, _root_status, _root_inbound) = synthetic_handle(root);
    let root_cancel = root_handle.cancel.clone();
    handles.insert(root_handle);
    let (descendant_handle, _descendant_status, _descendant_inbound) = synthetic_handle(descendant);
    let descendant_cancel = descendant_handle.cancel.clone();
    handles.insert(descendant_handle);

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    ctx.insert_extension(Arc::clone(&handles));
    let result = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": root.to_string()})),
            &ctx,
        )
        .await;
    let Err(error) = result else {
        return Err(test_failure(
            "descendant recovery did not stop subtree mutation",
        ));
    };

    assert_payload_free_recovery_error(error, descendant, "private descendant payload")?;
    assert!(
        !root_cancel.is_cancelled(),
        "target token remains untouched"
    );
    assert!(
        !descendant_cancel.is_cancelled(),
        "descendant token remains untouched",
    );
    assert!(handles.contains(root));
    assert!(handles.contains(descendant));
    assert!(registry.read().get(root).is_some());
    assert!(registry.read().get(descendant).is_some());

    for id in [root, descendant] {
        let Some(handle) = handles.remove(id) else {
            return Err(test_failure(format!("retained handle {id} disappeared")));
        };
        handle.cancel.cancel();
        handle.join_handle.await?;
    }
    Ok(())
}

#[tokio::test]
async fn close_agent_no_handle_terminal_path_refuses_late_parent_recovery() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let root = register_agent(&registry, "/recovery/no-handle-parent", None);
    let descendant = register_agent(
        &registry,
        "/recovery/no-handle-parent/descendant",
        Some(root),
    );
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    let handles = Arc::new(AgentHandles::new());
    handles.insert(descendant_handle_creating_parent_recovery(
        descendant,
        root,
        Arc::clone(&registry),
        &infra.pending_messages,
        router,
        Arc::clone(&store),
        "private no-handle parent payload",
    )?);

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    ctx.insert_extension(Arc::clone(&handles));
    let result = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": root.to_string()})),
            &ctx,
        )
        .await;
    let Err(error) = result else {
        return Err(test_failure("late no-handle recovery was reclaimed"));
    };

    assert_payload_free_recovery_error(error, root, "private no-handle parent payload")?;
    assert!(!handles.contains(descendant));
    assert_eq!(
        registry.read().get(root).map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "parent retained",
    );
    assert!(registry.read().tombstone(root).is_none());
    assert_eq!(
        registry
            .read()
            .tombstone(descendant)
            .map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "descendant reclaimed after its wrapper joins",
    );
    assert_eq!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(root)
            .map(|status| status.pending_count),
        Some(1),
    );
    Ok(())
}

#[tokio::test]
async fn close_agent_reclaimed_resolution_checks_retained_recovery() -> TestResult {
    let (infra, registry, router) = build_infra(Uuid::new_v4());
    let child = register_agent(&registry, "/recovery/reclaimed", None);
    let store = recovery_store(RecoverySinkMode::NeverRecover);
    retain_terminal_recovery(
        &infra.pending_messages,
        router.as_ref(),
        child,
        &store,
        "private reclaimed payload",
    )?;
    registry.write().mark_completed(child)?;
    assert!(registry.write().remove_terminal(child), "publish tombstone");

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&infra));
    let result = CloseAgentTool::new()
        .execute(
            &envelope_for("close_agent", json!({"agent_id": child.to_string()})),
            &ctx,
        )
        .await;
    let Err(error) = result else {
        return Err(test_failure("reclaimed soft success bypassed recovery"));
    };

    assert_payload_free_recovery_error(error, child, "private reclaimed payload")?;
    assert!(registry.read().get(child).is_none());
    assert_eq!(
        registry.read().tombstone(child).map(|entry| entry.status),
        Some(AgentStatus::Completed),
        "tombstone remains observable",
    );
    assert_eq!(
        infra
            .pending_messages
            .terminal_pending_recovery_status(child)
            .map(|status| status.pending_count),
        Some(1),
    );
    assert_eq!(store.len(), 0);
    Ok(())
}
