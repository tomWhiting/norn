use super::*;

struct SemaphoreProvider {
    sem: Arc<tokio::sync::Semaphore>,
    responses: StdMutex<Vec<Vec<ProviderEvent>>>,
}

impl Provider for SemaphoreProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let mut lock = self.responses.lock();
        let batch = if lock.is_empty() {
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }]
        } else {
            lock.remove(0)
        };
        drop(lock);
        let mut seq = Some(batch);
        let sem = Arc::clone(&self.sem);
        let s = stream::once(async move {
            if let Ok(permit) = sem.acquire().await {
                permit.forget();
            }
        })
        .flat_map(move |()| stream::iter(seq.take().unwrap_or_default().into_iter().map(Ok)));
        Ok(Box::pin(s))
    }
}

/// R6: `AgentHandles::inbound_tx(fork_id)` returns `Some` and a message
/// sent through it reaches the fork's inbound channel.
///
/// The fork is held behind a semaphore-gated provider so the receiver
/// is guaranteed to still be live when the test sends — making the
/// inbound-delivery assertion deterministic. A semaphore (not Notify)
/// is used because the runner may loop for a second provider call after
/// the steer message, and each call needs its own independently
/// consumable permit.
#[tokio::test]
async fn fork_inbound_channel_delivers_steer_message() -> TestResult {
    let sem = Arc::new(tokio::sync::Semaphore::new(0));
    let provider: Arc<dyn Provider> = Arc::new(SemaphoreProvider {
        sem: Arc::clone(&sem),
        responses: StdMutex::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_string(),
                    call_id: None,
                    name: Some("structured_output".to_string()),
                    arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]),
    });
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;

    let handles = required(ctx.get_extension::<AgentHandles>(), "AgentHandles")?;
    let inbound = required(handles.inbound_tx(fork_id), "fork inbound sender")?;
    // Fork is gated — the receiver is still in `run_agent_step`, so the
    // bounded channel is live and the send is guaranteed to succeed.
    inbound
        .send(ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
            from: "test".to_string(),
            role: None,
            to_id: fork_id,
            content: "hello fork".to_string(),
            kind: MessageKind::Steer,
            seq: None,
            timestamp: Utc::now(),
        })
        .await?;

    // Release permits for all provider calls. The steer message causes
    // additional loop iterations; the provider returns EndTurn when
    // scripted batches are exhausted. Extra permits are harmless.
    sem.add_permits(10);
    let handle = required(handles.remove(fork_id), "fork handle")?;
    handle.join_handle.await?;
    Ok(())
}

/// R7: fork with a tasks array produces structured output validating
/// against the dynamically-built schema.
#[tokio::test]
async fn fork_with_requirements_produces_structured_output() -> TestResult {
    let valid = json!({
        "response": "all done",
        "requirements": {
            "a": {"completed": true, "completion_notes": "ok"},
            "b": {"completed": false, "completion_notes": "skipped"},
        },
    });
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: valid.to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        // Fallback done-turn in case the runner loops after structured output.
        vec![ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }],
    ]));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({
                "request": "split work",
                "model": "gpt-5.5",
                "requirements": [
                    {"name": "a", "description": "first"},
                    {"name": "b", "description": "second"},
                ],
            })),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    let events = parent_store.events();
    let summary = events.iter().rev().find_map(|e| match e {
        SessionEvent::ForkComplete { result_summary, .. } => Some(result_summary.clone()),
        _ => None,
    });
    let summary = required(summary, "ForkComplete event")?;
    let schema = build_fork_output_schema(&[
        ForkRequirement {
            name: "a".to_string(),
            description: "first".to_string(),
        },
        ForkRequirement {
            name: "b".to_string(),
            description: "second".to_string(),
        },
    ]);
    let compiled = jsonschema::validator_for(&schema)?;
    assert!(
        compiled.is_valid(&summary),
        "ForkComplete.result_summary must validate: {summary}",
    );
    Ok(())
}

/// Unbounded-retention regression: with
/// [`crate::tools::agent::ReclaimOnResultDelivery`] installed and a
/// result channel present, a naturally-completed fork's registry
/// entry AND parent-held handle are reclaimed once its result has
/// been delivered.
#[tokio::test]
async fn fork_delivered_result_reclaims_when_marker_present() -> TestResult {
    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(crate::tools::agent::ReclaimOnResultDelivery));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;

    let result = required(
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await?,
        "child result channel must stay open",
    )?;
    assert_eq!(result.agent_id, fork_id);

    let handles = required(ctx.get_extension::<AgentHandles>(), "AgentHandles")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while agent_registry.read().get(fork_id).is_some() || handles.contains(fork_id) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for fork registry entry and handle reclamation",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

/// Hardening (owner ruling 2026-07-03): a fork must run with
/// auto-compaction armed exactly like the root. The fork launch path
/// calls the shared `arm_auto_compaction`, installing the token
/// estimator and filling the fork's context window from the catalog
/// for the fork's own model. This drives a fork whose first turn
/// reports an oversized usage (setting the context-edit usage floor
/// above the window) and asserts the fork's next preflight emitted a
/// `loop.token_warning` on the fork's store — structurally impossible
/// without the estimator and window the shared arming installs.
#[tokio::test]
async fn fork_child_arms_auto_compaction_preflight() -> TestResult {
    let catalog_model = crate::model_catalog::default_selection().model;
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(IdentityStubTool {
        seen_agent: Arc::new(StdMutex::new(None)),
        seen_parent: Arc::new(StdMutex::new(None)),
    }));
    let tool_registry = Arc::new(tool_registry);

    // Turn 1: a tool call (forces a second round-trip so a second
    // preflight runs) whose reported usage dwarfs any context window —
    // this becomes the usage floor. Turn 2: structured output so the
    // fork completes cleanly.
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-id".to_string(),
                call_id: None,
                name: Some("identity".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 100_000_000,
                    output_tokens: 0,
                    ..Usage::default()
                },
                response_id: None,
            },
        ],
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
    ]));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "run", "model": catalog_model, "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let fork_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    let warned = fork_store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::Custom { event_type, .. } if event_type == "loop.token_warning"
        )
    });
    assert!(
        warned,
        "the fork's preflight must emit loop.token_warning, proving the \
         estimator and catalog window were armed on the fork",
    );
    Ok(())
}
