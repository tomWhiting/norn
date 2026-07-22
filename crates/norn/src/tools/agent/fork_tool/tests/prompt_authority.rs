use super::*;

// ----- agent-variants (R4/R5/R6/§7) -----------------------------------

/// §7 (fork side): a fork on an uncatalogued model is rejected BEFORE
/// anything is reserved — a typed error naming the model, no registry
/// entry, no burned name.
#[tokio::test]
async fn fork_with_uncatalogued_model_is_rejected_before_reservation() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let result = tool
        .execute(
            &envelope_for(json!({
                "request": "r", "model": "not-in-catalog-model-xyz", "requirements": [],
            })),
            &ctx,
        )
        .await;
    let Err(err) = result else {
        return Err(test_error("an uncatalogued fork model must be rejected"));
    };
    assert!(
        err.to_string().contains("not-in-catalog-model-xyz"),
        "the rejection names the model: {err}",
    );
    assert!(
        agent_registry.read().is_empty(),
        "the rejection precedes the reservation",
    );
    assert!(
        parent_store.events().is_empty(),
        "nothing was persisted for the refused fork",
    );
    Ok(())
}

/// The fork publishes only the typed, identity-free parent plan for its
/// own descendants. The legacy flattened extension is input-only.
#[tokio::test]
async fn fork_child_context_publishes_typed_identity_free_parent_plan() -> TestResult {
    struct BaseProbe {
        seen: Arc<StdMutex<Option<crate::system_prompt::PromptPlan>>>,
    }
    #[async_trait]
    impl TestTool for BaseProbe {
        fn name(&self) -> &'static str {
            "base_probe"
        }
        fn description(&self) -> &'static str {
            "records the ParentPromptPlan it sees"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            *self.seen.lock() = ctx
                .get_extension::<ParentPromptPlan>()
                .map(|ext| ext.plan().clone());
            assert!(
                ctx.get_extension::<ParentSystemInstruction>().is_none(),
                "new fork contexts must not publish a flattened authority-erasing bridge",
            );
            Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-probe".to_string(),
                call_id: None,
                name: Some("base_probe".to_string()),
                arguments_delta: "{}".to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {"check_code": {}}})
                    .to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }],
    ]));

    let seen = Arc::new(StdMutex::new(None));
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(BaseProbe {
        seen: Arc::clone(&seen),
    }));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    // The forker's own base, as its assembly path would publish it.
    ctx.insert_extension(Arc::new(ParentSystemInstruction::new("PARENT-BASE-MARKER")));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({
                "request": "probe your base",
                "model": "gpt-5.5",
                "requirements": [
                    {"name": "check code", "description": "check the code"}
                ],
            })),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    let plan = required(
        seen.lock().clone(),
        "the fork's context must publish ParentPromptPlan",
    )?;
    assert_eq!(plan.fragments().len(), 1);
    assert_eq!(
        plan.fragments()[0].source(),
        crate::system_prompt::PromptSource::EmbedderPolicy,
    );
    assert_eq!(plan.fragments()[0].content(), "PARENT-BASE-MARKER");
    assert!(
        plan.fragments().iter().all(|fragment| {
            fragment.source() != crate::system_prompt::PromptSource::ForkAgentPolicy
                && !fragment.content().contains(FORK_SYSTEM_PREAMBLE)
        }),
        "the published plan must carry no fork identity: {plan:?}",
    );
    Ok(())
}

/// Root-to-fork request authority: inherited typed fragments retain their
/// roles and order, while the human task plus requirements enter exactly once
/// as the current User prompt and never appear in System content.
#[tokio::test]
async fn root_to_fork_request_preserves_roles_and_user_task_boundary() -> TestResult {
    use crate::provider::request::MessageRole;
    use crate::system_prompt::{PromptPlan, PromptSource};

    const PRODUCT: &str = "ROOT-PRODUCT-SENTINEL";
    const OPERATOR: &str = "ROOT-OPERATOR-SENTINEL";
    const WORKSPACE: &str = "ROOT-WORKSPACE-SENTINEL";
    const TASK: &str = "FORK-TASK-SENTINEL";
    const REQUIREMENT: &str = "FORK-REQUIREMENT-NAME";
    const DESCRIPTION: &str = "FORK-REQUIREMENT-DESCRIPTION";

    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "structured-out".to_owned(),
                    call_id: None,
                    name: Some("structured_output".to_owned()),
                    arguments_delta: json!({
                        "response": "done",
                        "requirements": {
                            "fork_requirement_name": {"completed": true}
                        }
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }],
        ]),
    });
    let registry = AgentRegistry::shared();
    let (ctx, _) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let mut parent_plan = PromptPlan::new();
    parent_plan.set(PromptSource::ProductPolicy, PRODUCT);
    parent_plan.set(PromptSource::OperatorProfile, OPERATOR);
    parent_plan.set(PromptSource::WorkspaceProfile, WORKSPACE);
    ctx.insert_extension(Arc::new(ParentPromptPlan::new(parent_plan)));

    let output = ForkTool::new()
        .execute(
            &envelope_for(json!({
                "request": TASK,
                "model": "gpt-5.5",
                "requirements": [{"name": REQUIREMENT, "description": DESCRIPTION}],
            })),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&output)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    let requests = captured.lock();
    let first = required(requests.first(), "first fork provider request")?;
    let expected_task = format!("{TASK}\n\n## Requirements\n\n### {REQUIREMENT}\n{DESCRIPTION}\n");
    let relevant = first
        .messages
        .iter()
        .filter_map(|message| {
            let content = message.content.as_deref()?;
            (content == PRODUCT
                || content == OPERATOR
                || content == WORKSPACE
                || content == expected_task
                || content.contains(FORK_SYSTEM_PREAMBLE))
            .then_some((message.role.clone(), content))
        })
        .collect::<Vec<_>>();
    assert_eq!(relevant.len(), 5, "relevant request messages: {relevant:?}");
    assert_eq!(
        relevant
            .iter()
            .map(|(role, _)| role.clone())
            .collect::<Vec<_>>(),
        [
            MessageRole::System,
            MessageRole::System,
            MessageRole::Developer,
            MessageRole::User,
            MessageRole::User,
        ],
        "typed parent fragments and the one current task retain exact roles/order",
    );
    assert_eq!(relevant[0].1, PRODUCT);
    assert!(relevant[1].1.contains("## Fork identity"));
    assert_eq!(relevant[2].1, OPERATOR);
    assert_eq!(relevant[3].1, WORKSPACE);
    assert_eq!(relevant[4].1, expected_task);
    assert_eq!(
        first
            .messages
            .iter()
            .filter(|message| message.content.as_deref() == Some(expected_task.as_str()))
            .count(),
        1,
    );
    assert!(
        first
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::System)
            .all(|message| {
                message.content.as_deref().is_none_or(|content| {
                    !content.contains(TASK)
                        && !content.contains(REQUIREMENT)
                        && !content.contains(DESCRIPTION)
                })
            })
    );
    Ok(())
}

/// Fork-of-fork regression: each provider request carries the inherited
/// identity-free plan plus one fresh `ForkAgentPolicy` System fragment, never
/// the parent fork's stale identity stacked under it.
#[tokio::test]
async fn fork_of_fork_request_has_exactly_one_current_identity_block() -> TestResult {
    // Provider shared by every level (the fork forwards the parent's
    // provider): captures each request so the grandchild's system
    // instruction can be asserted from ground truth. Scripted
    // streams: level-1 fork calls `fork` (the nested launch), then
    // both forks return the identical requirement-less structured
    // output, so the concurrent pop order between level 1's closing
    // stream and level 2's only stream cannot skew the script.
    let structured_done = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "structured-out".to_string(),
            call_id: None,
            name: Some("structured_output".to_string()),
            arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RequestCapturingProvider {
        captured: Arc::clone(&captured),
        responses: StdMutex::new(vec![
            vec![
                ProviderEvent::ToolCallDelta {
                    item_id: "tc-nested-fork".to_string(),
                    call_id: None,
                    name: Some("fork".to_string()),
                    arguments_delta: json!({
                        "request": "go one level deeper",
                        "model": "gpt-5.5",
                        "requirements": [],
                    })
                    .to_string(),
                    kind: crate::provider::request::ToolCallKind::Function,
                },
                done_event_tool_use(),
            ],
            structured_done.clone(),
            structured_done,
        ]),
    });

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(ForkTool::new()));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );
    // Depth-2 envelope so the level-1 fork (depth 1) still carries
    // the fork tool for the nested launch. Deliberate test values.
    {
        use crate::agent::child_policy::DelegationBudget;
        ctx.insert_extension(Arc::new(CoordinationEnvelope {
            child_policy: ChildPolicy {
                messaging: MessagingScope::SiblingsAndParent,
                delegation: DelegationBudget {
                    remaining_depth: 2,
                    max_concurrent_children: 4,
                },
                inbound_capacity: 8,
                loop_config: None,
            },
            child_result_capacity: 16,
        }));
    }
    ctx.insert_extension(Arc::new(ParentSystemInstruction::new("PARENT-BASE-MARKER")));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({
                "request": "fork twice",
                "model": "gpt-5.5",
                "requirements": [],
            })),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    // The grandchild runs concurrently with (and possibly beyond) level
    // 1's join. Poll until both fork levels have emitted a request with a
    // current ForkAgentPolicy message.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let fork_requests: Vec<ProviderRequest> = loop {
        let requests = captured
            .lock()
            .iter()
            .filter(|request| {
                request.messages.iter().any(|message| {
                    message.role == crate::provider::request::MessageRole::System
                        && message
                            .content
                            .as_deref()
                            .is_some_and(|content| content.contains(FORK_SYSTEM_PREAMBLE))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if requests.iter().any(|request| {
            request.messages.iter().any(|message| {
                message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("fork twice"))
            })
        }) && requests.iter().any(|request| {
            request.messages.iter().any(|message| {
                message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("go one level deeper"))
            })
        }) {
            break requests;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "both fork levels must issue provider requests; saw {requests:?}",
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    for request in &fork_requests {
        let identity_blocks = request
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .map(|content| content.matches("## Fork identity").count())
            .sum::<usize>();
        assert_eq!(
            identity_blocks, 1,
            "every fork level renders exactly one identity block: {request:?}",
        );
        assert!(
            request.messages.iter().any(|message| {
                message.role == crate::provider::request::MessageRole::System
                    && message.content.as_deref() == Some("PARENT-BASE-MARKER")
            }),
            "the original identity-free embedder policy remains System: {request:?}",
        );
    }
    Ok(())
}
