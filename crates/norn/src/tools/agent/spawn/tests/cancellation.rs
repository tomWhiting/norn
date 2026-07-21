use super::*;

/// Provider for the W3.5 cascade trees: the mid-tree child's first
/// call emits a `spawn_agent` tool call for the grandchild; every
/// later child call — and the grandchild's only call — parks inside
/// a never-yielding stream, notifying the matching `Notify` so the
/// test knows both runs are mid-flight before cancelling. Routes by
/// last message exactly like `TreeProvider` above.
struct CascadeTreeProvider {
    child_calls: Arc<std::sync::atomic::AtomicUsize>,
    grandchild_calls: Arc<std::sync::atomic::AtomicUsize>,
    child_parked: Arc<tokio::sync::Notify>,
    grandchild_parked: Arc<tokio::sync::Notify>,
}

impl Provider for CascadeTreeProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        use std::sync::atomic::Ordering as AtomicOrdering;
        // The managed dynamic-context Developer message now rides at the
        // tail of every request (prompt-cache fix), so route on the last
        // non-Developer message — the turn content that actually seeds
        // this child.
        let last = request
            .messages
            .iter()
            .rev()
            .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        if last == "grandchild-task" {
            self.grandchild_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.grandchild_parked.notify_one();
            return Ok(Box::pin(stream::pending::<
                Result<ProviderEvent, ProviderError>,
            >()));
        }
        let call = self.child_calls.fetch_add(1, AtomicOrdering::SeqCst);
        if call == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    item_id: "tc-grandchild".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "grandchild-task",
                        "model": CATALOG_MODEL,
                        "role": "leaf",
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                }),
                Ok(done_event_tool_use()),
            ])));
        }
        self.child_parked.notify_one();
        Ok(Box::pin(stream::pending::<
            Result<ProviderEvent, ProviderError>,
        >()))
    }
}

/// A depth-2 tree (root context → child → grandchild) with both runs
/// parked inside in-flight provider calls, ready for a cascade test.
struct ParkedDepth2Tree {
    ctx: ToolContext,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    rx: tokio::sync::mpsc::Receiver<crate::agent::result_channel::ChildAgentResult>,
    root_id: Uuid,
    child_id: Uuid,
    grandchild_id: Uuid,
    grandchild_calls: Arc<std::sync::atomic::AtomicUsize>,
}

/// Builds the depth-2 tree: a root context (publishing `root_cancel`
/// as its [`AgentCancellation`] when given, token-less otherwise)
/// with delivery-anchored reclamation, an envelope granting depth 2,
/// a spawned child that spawns a grandchild, and both runs parked
/// mid-provider-call (deterministic — `notify_one` stores permits).
async fn parked_depth2_tree(
    root_cancel: Option<tokio_util::sync::CancellationToken>,
) -> TestResult<ParkedDepth2Tree> {
    let agent_registry = AgentRegistry::shared();
    let child_parked = Arc::new(tokio::sync::Notify::new());
    let grandchild_parked = Arc::new(tokio::sync::Notify::new());
    let grandchild_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CascadeTreeProvider {
        child_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        grandchild_calls: Arc::clone(&grandchild_calls),
        child_parked: Arc::clone(&child_parked),
        grandchild_parked: Arc::clone(&grandchild_parked),
    });
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(SpawnAgentTool::new()));
    let root_id = Uuid::new_v4();
    let ctx = parent_ctx(
        provider,
        root_id,
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));
    if let Some(token) = root_cancel {
        ctx.insert_extension(Arc::new(AgentCancellation(token)));
    }
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(ReclaimOnResultDelivery));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "child-task", "model": CATALOG_MODEL, "role": "lead"})),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("spawn output must carry agent_id")?,
    )?;

    tokio::time::timeout(Duration::from_secs(60), async {
        grandchild_parked.notified().await;
        child_parked.notified().await;
    })
    .await?;

    let grandchild_id = {
        let reg = agent_registry.read();
        let children = reg.children(child_id);
        assert_eq!(children.len(), 1, "exactly one grandchild registered");
        children[0].id
    };

    Ok(ParkedDepth2Tree {
        ctx,
        agent_registry,
        rx,
        root_id,
        child_id,
        grandchild_id,
        grandchild_calls,
    })
}

/// W3.5 cooperative cascade end-to-end: with the root's token
/// published, cancelling the ROOT token alone terminates a depth-2
/// subtree mid-run — the child's and the grandchild's runs both end
/// at their next cancellation boundary with the real `Cancelled`
/// outcome, every wrapper performs its own terminal sequence (honest
/// `Failed` records at every level, lineage intact), and the whole
/// subtree reclaims: no dangling `Started`, no leaked entries, no
/// aborted tasks. The grandchild's result lands in the cancelled
/// child's channel (delivered, or error-logged when the child's loop
/// already dropped its receiver — never silent), so the root sees
/// exactly one result: the child's.
#[tokio::test]
async fn cancelling_root_token_cascades_to_depth2_subtree_with_honest_outcomes() -> TestResult {
    use crate::agent::output::AgentStopReason;

    let root_cancel = tokio_util::sync::CancellationToken::new();
    let mut tree = parked_depth2_tree(Some(root_cancel.clone())).await?;

    root_cancel.cancel();

    // The child's wrapper delivers the run's real outcome to the
    // root's channel — cancellation yields an accounted tree.
    let result = tokio::time::timeout(Duration::from_secs(60), tree.rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(result.agent_id, tree.child_id);
    assert!(!result.succeeded, "a cancelled run is not a success");
    assert_eq!(result.stop, Some(AgentStopReason::Cancelled));

    // Whole-subtree reclamation under cascade (the W3.4 machinery at
    // depth 2): every entry leaves the registry, every level keeps an
    // honest Failed tombstone with intact parent links.
    wait_for_condition(
        || tree.agent_registry.read().is_empty(),
        "registry must fully reclaim after a root-token cascade",
    )
    .await;
    let reg = tree.agent_registry.read();
    let tombstones = reg.tombstones();
    assert_eq!(tombstones.len(), 2, "child + grandchild: {tombstones:?}");
    let child_tomb = tombstones
        .iter()
        .find(|t| t.id == tree.child_id)
        .ok_or("required test value")?;
    assert_eq!(
        child_tomb.status,
        AgentStatus::Failed,
        "a cancelled run records Failed — never Completed",
    );
    assert_eq!(child_tomb.parent_id, Some(tree.root_id));
    let grandchild_tomb = tombstones
        .iter()
        .find(|t| t.id == tree.grandchild_id)
        .ok_or("required test value")?;
    assert_eq!(grandchild_tomb.status, AgentStatus::Failed);
    assert_eq!(
        grandchild_tomb.parent_id,
        Some(tree.child_id),
        "lineage survives reclamation at every level",
    );
    drop(reg);

    // The grandchild's run actually ended: its provider was entered
    // exactly once and never again after the cascade.
    assert_eq!(
        tree.grandchild_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the grandchild's run must end at the cascaded cancel",
    );
    // No result ever skipped a level to the root.
    assert!(
        tree.rx.try_recv().is_err(),
        "the grandchild's result must never reach the root directly",
    );
    // The parent-held child handle was reclaimed by its wrapper.
    assert!(
        tree.ctx
            .get_extension::<AgentHandles>()
            .ok_or("required test value")?
            .is_empty(),
        "no handle may leak after the cascade",
    );
    Ok(())
}

/// W3.5 forced cascade at depth: `close_agent` on a MID-TREE agent —
/// the closer holds only the target's handle, never the grandchild's
/// — fires the target's token before the walk, which cascades to the
/// grandchild through token parentage. The close returns only after
/// the TARGET's wrapper completes (its Cancelled result is already
/// on the root's channel when the tool returns); the grandchild is
/// reported honestly ("cancelling", or a terminal label when its own
/// wrapper wins the race — never "unreachable") and terminates
/// through its own wrapper without close touching it. Leaves-first
/// ordering holds, and the whole subtree reclaims with honest Failed
/// records.
///
/// The root context here deliberately publishes NO
/// [`AgentCancellation`] — additionally pinning that the cascade
/// below depth 1 works under a token-less embedder root, because the
/// child's own token is published at its context construction either
/// way.
#[tokio::test]
async fn close_mid_tree_cascades_to_grandchild_and_returns_after_target_wrapper() -> TestResult {
    use crate::agent::output::AgentStopReason;
    use crate::tools::agent::coord::CloseAgentTool;

    let mut tree = parked_depth2_tree(None).await?;

    let close_out = CloseAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "close-1".to_string(),
                tool_name: "close_agent".to_string(),
                model_args: json!({
                    "agent_id": tree.child_id.to_string(),
                    "reason": "stand down",
                }),
                metadata: serde_json::Value::Null,
            },
            &tree.ctx,
        )
        .await?;
    assert!(!close_out.is_error(), "{:?}", close_out.content);

    // Leaves-first: the grandchild is reported before the target.
    let shut_down = close_out.content["shut_down"]
        .as_array()
        .ok_or("required test value")?;
    assert_eq!(shut_down.len(), 2, "{shut_down:?}");
    assert_eq!(shut_down[0]["agent_id"], tree.grandchild_id.to_string());
    assert_eq!(shut_down[1]["agent_id"], tree.child_id.to_string());

    // Never "unreachable" under a cascade: the grandchild's token
    // was cancelled before the walk, so close reports the truth —
    // cancelling (live, its wrapper finishing) or a terminal label
    // when its wrapper won the race.
    let grandchild_status = shut_down[0]["status"]
        .as_str()
        .ok_or("required test value")?;
    assert!(
        ["cancelling", "reclaimed", "already_completed"].contains(&grandchild_status),
        "cascade-reached grandchild must not be reported unreachable: {grandchild_status}",
    );
    // The target's wrapper completed before close returned, recording
    // the run's real outcome itself.
    let child_status = shut_down[1]["status"]
        .as_str()
        .ok_or("required test value")?;
    assert!(
        ["reclaimed", "already_completed"].contains(&child_status),
        "the cancelled target's wrapper owns its terminal sequence: {child_status}",
    );

    // Join-at-depth pin: the target's result was delivered before the
    // close's join returned — try_recv, no awaiting.
    let result = tree.rx.try_recv()?;
    assert_eq!(result.agent_id, tree.child_id);
    assert!(!result.succeeded);
    assert_eq!(result.stop, Some(AgentStopReason::Cancelled));

    // The grandchild terminates through its own wrapper — close never
    // held its handle — and the subtree fully reclaims with honest
    // Failed records at both levels.
    wait_for_condition(
        || tree.agent_registry.read().is_empty(),
        "subtree must fully reclaim after a mid-tree close",
    )
    .await;
    let reg = tree.agent_registry.read();
    let tombstones = reg.tombstones();
    assert_eq!(tombstones.len(), 2, "{tombstones:?}");
    for id in [tree.child_id, tree.grandchild_id] {
        let tomb = tombstones
            .iter()
            .find(|t| t.id == id)
            .ok_or("required test value")?;
        assert_eq!(
            tomb.status,
            AgentStatus::Failed,
            "honest Failed at every level — never Completed, no force marks",
        );
    }
    drop(reg);
    assert_eq!(
        tree.grandchild_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the grandchild's run must end at the cascaded cancel, not re-enter the provider",
    );
    Ok(())
}
