use super::*;

/// Routes provider scripts so a mid-tree fork and the grandchild it
/// spawns share the one workspace provider deterministically; the
/// fork's would-stop turn is held until the registry shows the
/// grandchild reclaimed (which guarantees its result is already in
/// the fork's own channel).
struct ForkTreeProvider {
    registry: Arc<RwLock<AgentRegistry>>,
    fork_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl Provider for ForkTreeProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        use std::sync::atomic::Ordering as AtomicOrdering;
        // The managed dynamic-context Developer message now rides at the
        // tail of every request (prompt-cache fix), so route on the last
        // non-Developer message — the turn content that actually seeds
        // this fork.
        let last = request
            .messages
            .iter()
            .rev()
            .find(|m| !matches!(m.role, crate::provider::request::MessageRole::Developer))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let end_turn = ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            },
            response_id: None,
        };
        if last == "fork-grandchild-task" {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta {
                    text: "fork grandchild says hi".to_string(),
                }),
                Ok(end_turn),
            ])));
        }
        let call = self.fork_calls.fetch_add(1, AtomicOrdering::SeqCst);
        match call {
            0 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    item_id: "tc-grandchild".to_string(),
                    call_id: None,
                    name: Some("spawn_agent".to_string()),
                    arguments_delta: json!({
                        "task": "fork-grandchild-task",
                        "model": crate::model_catalog::default_selection().model,
                        "role": "leaf",
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                }),
                Ok(done_event_tool_use()),
            ]))),
            1 => {
                let registry = Arc::clone(&self.registry);
                let s = stream::once(async move {
                    for _ in 0..2400 {
                        let reclaimed = registry
                            .read()
                            .tombstones()
                            .iter()
                            .any(|t| t.path.contains("/spawn/"));
                        if reclaimed {
                            return Ok(());
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    let snapshot = registry.read().list();
                    let tombstones = registry.read().tombstones();
                    Err(ProviderError::StreamError {
                        reason: format!(
                            "fork grandchild was never reclaimed; \
                             entries={snapshot:?}; tombstones={tombstones:?}"
                        ),
                        transient: None,
                    })
                })
                .flat_map(move |wait_result| {
                    let events = match wait_result {
                        Ok(()) => vec![
                            Ok(ProviderEvent::TextDelta {
                                text: "waited for grandchild".to_string(),
                            }),
                            Ok(end_turn.clone()),
                        ],
                        Err(error) => vec![Err(error)],
                    };
                    stream::iter(events)
                });
                Ok(Box::pin(s))
            }
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({
                        "response": "fork done after grandchild",
                        "requirements": {},
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                }),
                Ok(done_event_tool_use()),
            ]))),
        }
    }
}

/// W3.4 end-to-end on the fork surface: a fork granted depth ≥ 1
/// spawns a grandchild; the grandchild's result is delivered into the
/// **fork's** conversation (one hop — never to the root), the fork's
/// structured result reaches the root's channel, and every registry
/// entry is reclaimed at every level.
#[tokio::test]
async fn fork_drains_its_childrens_results_one_hop_and_reclaims() -> TestResult {
    let agent_registry = AgentRegistry::shared();
    let provider: Arc<dyn Provider> = Arc::new(ForkTreeProvider {
        registry: Arc::clone(&agent_registry),
        fork_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(crate::tools::agent::SpawnAgentTool::new()));
    let root_id = Uuid::new_v4();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        root_id,
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    let mut envelope = test_envelope();
    envelope.child_policy.delegation.remaining_depth = 2;
    ctx.insert_extension(Arc::new(envelope));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(
        crate::tools::agent::reclaim::ReclaimOnResultDelivery,
    ));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "fork-task", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    assert!(!out.is_error(), "{:?}", out.content);
    let fork_id = fork_id_from(&out)?;
    let fork_path = required(
        out.content.get("path").and_then(serde_json::Value::as_str),
        "path",
    )?
    .to_string();

    // Take the handle now, before the wrapper's reclamation pass, so
    // the fork's store stays inspectable. Registry reclamation is
    // unaffected — the wrapper's handle removal is idempotent.
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let fork_store = Arc::clone(&handle.event_store);

    let result = required(
        tokio::time::timeout(Duration::from_secs(120), rx.recv()).await?,
        "fork result channel must stay open",
    )?;
    assert_eq!(result.agent_id, fork_id);
    assert!(result.succeeded, "{:?}", result.error);
    assert!(
        rx.try_recv().is_err(),
        "the grandchild's result must never reach the root directly",
    );
    // W3.6 rollup on the fork surface: the grandchild's run made one
    // provider call at (5, 2) (`ForkTreeProvider`'s grandchild turn),
    // so the fork's subtree total is exactly its own usage plus that
    // one delivered subtree — each level counted once, never folded
    // into the fork's own `usage`.
    assert_eq!(
        result.subtree_usage.input_tokens,
        result.usage.input_tokens + 5,
        "fork subtree = own + grandchild, exactly once",
    );
    assert_eq!(
        result.subtree_usage.output_tokens,
        result.usage.output_tokens + 2,
    );

    let injected = fork_store.events().iter().any(|event| {
        matches!(
            event,
            SessionEvent::UserMessage { content, .. }
                if content.contains("<agent_result")
                    && content.contains("fork grandchild says hi")
        )
    });
    assert!(
        injected,
        "the grandchild's framed result must be injected into the fork's conversation",
    );

    // Reclamation at every level, with the grandchild nested under
    // the fork's path.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if agent_registry.read().is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "registry entries leaked: {:?}",
            agent_registry.read().list(),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let reg = agent_registry.read();
    let tombstones = reg.tombstones();
    assert_eq!(tombstones.len(), 2, "fork + grandchild reclaimed");
    let grandchild_tomb = required(
        tombstones.iter().find(|t| t.id != fork_id),
        "grandchild tombstone",
    )?;
    assert_eq!(grandchild_tomb.parent_id, Some(fork_id));
    assert!(
        grandchild_tomb
            .path
            .starts_with(&format!("{fork_path}/spawn/")),
        "grandchild path nests under the fork: {}",
        grandchild_tomb.path,
    );
    Ok(())
}

/// W3.5: a fork's run token is created as a child of the forker's
/// published [`AgentCancellation`] token, so cancelling the PARENT
/// token alone — never touching the fork's handle — terminates the
/// fork's in-flight run, whose wrapper records the real Cancelled
/// outcome through its normal terminal sequence (mirrors the spawn
/// cascade tests).
#[tokio::test]
async fn cancelling_parent_token_cascades_to_in_flight_fork() -> TestResult {
    use crate::agent::output::AgentStopReason;
    use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
    use crate::tools::agent::AgentCancellation;

    /// Never-yielding provider: the fork parks inside its first
    /// in-flight call and notifies `entered`.
    struct ParkedForkProvider {
        entered: Arc<tokio::sync::Notify>,
    }
    impl Provider for ParkedForkProvider {
        fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.entered.notify_one();
            Ok(Box::pin(stream::pending::<
                Result<ProviderEvent, ProviderError>,
            >()))
        }
    }

    let entered = Arc::new(tokio::sync::Notify::new());
    let provider: Arc<dyn Provider> = Arc::new(ParkedForkProvider {
        entered: Arc::clone(&entered),
    });
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let parent_cancel = tokio_util::sync::CancellationToken::new();
    ctx.insert_extension(Arc::new(AgentCancellation(parent_cancel.clone())));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChildAgentResult>(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "summarise", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    entered.notified().await;

    // Cancel the PARENT's token only; the fork's child token observes
    // it through tokio_util's cascade.
    parent_cancel.cancel();

    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    let result = rx.try_recv()?;
    assert_eq!(result.agent_id, fork_id);
    assert!(!result.succeeded, "a cancelled fork is not a success");
    assert_eq!(result.stop, Some(AgentStopReason::Cancelled));
    let failed_entry = required(
        agent_registry.read().get(fork_id),
        "entry must remain observable without a reclaim marker",
    )?;
    assert_eq!(
        failed_entry.status,
        AgentStatus::Failed,
        "a cancelled fork records Failed — never Completed",
    );
    Ok(())
}
