use super::*;

/// R6 (fork side): a leaf fork (granted `remaining_depth` == 0 — the
/// default derivation from a depth-1 forker) is shown NEITHER
/// `spawn_agent` nor fork in its provider payload.
#[tokio::test]
async fn leaf_fork_provider_tool_list_omits_spawn_and_fork() -> TestResult {
    struct ToolsCapturingProvider {
        captured: Arc<StdMutex<Vec<crate::provider::tools::ProviderToolDefinition>>>,
        responses: StdMutex<Vec<Vec<ProviderEvent>>>,
    }
    impl Provider for ToolsCapturingProvider {
        fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
            self.captured.lock().clone_from(&request.tools);
            let seq = self.responses.lock().remove(0);
            Ok(Box::pin(stream::iter(seq.into_iter().map(Ok))))
        }
    }

    let captured = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(ToolsCapturingProvider {
        captured: Arc::clone(&captured),
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
        ]),
    });

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(crate::tools::agent::SpawnAgentTool::new()));
    tool_registry.register(Box::new(ForkTool::new()));
    tool_registry.register(Box::new(crate::tools::read::ReadTool::new()));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(tool_registry),
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "r", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    let names: Vec<String> = captured
        .lock()
        .iter()
        .map(|def| match def {
            crate::provider::tools::ProviderToolDefinition::Function(function) => {
                function.name.clone()
            }
            other @ crate::provider::tools::ProviderToolDefinition::Hosted(_) => {
                format!("{other:?}")
            }
        })
        .collect();
    assert!(
        !names.iter().any(|n| n == "spawn_agent") && !names.iter().any(|n| n == "fork"),
        "a leaf fork must not SEE delegation tools: {names:?}",
    );
    assert!(
        names.iter().any(|n| n == "read"),
        "non-delegation tools survive: {names:?}",
    );
    Ok(())
}
