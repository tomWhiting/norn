use super::*;

fn tool_use_stream_with_bare_reasoning() -> String {
    let terminal = json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_before_seed_change",
            "status": "completed",
            "output": [
                {
                    "id": "rs_before_seed_change",
                    "type": "reasoning",
                    "summary": [],
                    "status": "completed"
                },
                {
                    "id": "fc_rewrite_context",
                    "call_id": "call_rewrite_context",
                    "type": "function_call",
                    "name": "rewrite_context",
                    "arguments": "{}",
                    "status": "completed"
                }
            ],
            "incomplete_details": null,
            "usage": {
                "input_tokens": 4,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens": 2,
                "output_tokens_details": {"reasoning_tokens": 1},
                "total_tokens": 6
            }
        }
    });
    format!("event: response.completed\ndata: {terminal}\n\n")
}

async fn mount_single_tool_response(server: &MockServer) {
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |_request: &wiremock::Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(tool_use_stream_with_bare_reasoning())
            } else {
                ResponseTemplate::new(500).set_body_string("unexpected second HTTP request")
            }
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn hot_seed_cut_with_unreplayable_reasoning_fails_typed_before_second_wire() -> TestResult {
    let workspace = tempfile::tempdir()?;
    let context_path = workspace.path().join("NORN.md");
    std::fs::write(&context_path, "repository-v1")?;

    let mut loop_context = LoopContext::new("legacy");
    loop_context.context_loader = Some(ContextLoader::load(workspace.path()));
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product");
    plan.set(PromptSource::ProjectContextFile, "repository-v1");
    loop_context.install_stable_prompt_plan(plan);

    let server = MockServer::start().await;
    mount_single_tool_response(&server).await;
    let provider = wire_provider(&server)?;
    let rewrites = Arc::new(AtomicUsize::new(0));
    let rewritten_path = context_path.clone();
    let handler_rewrites = Arc::clone(&rewrites);
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "rewrite_context".to_owned(),
        Box::new(move |_| {
            handler_rewrites.fetch_add(1, Ordering::SeqCst);
            std::fs::write(&rewritten_path, "repository-v2").map_err(|error| {
                crate::error::ToolError::ExecutionFailed {
                    reason: format!("failed to rewrite project context fixture: {error}"),
                }
            })?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&rewritten_path)
                .map_err(|error| crate::error::ToolError::ExecutionFailed {
                    reason: format!("failed to reopen project context fixture: {error}"),
                })?;
            file.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
                .map_err(|error| crate::error::ToolError::ExecutionFailed {
                    reason: format!("failed to advance project context mtime: {error}"),
                })?;
            Ok(json!({"rewritten": true}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let tools = [ToolDefinition {
        name: "rewrite_context".to_owned(),
        description: "Rewrite project context".to_owned(),
        parameters: json!({}),
    }];
    let store = EventStore::new();
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "rewrite the repository context",
        tools: &tools,
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await;

    assert!(matches!(
        result,
        Err(NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(rewrites.load(Ordering::SeqCst), 1);
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    assert_eq!(
        requests.len(),
        1,
        "the anchor cut must reject bare reasoning before a second HTTP request",
    );
    let first_payload: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(first_payload["store"], Value::Bool(true));
    assert!(first_payload.get("previous_response_id").is_none());
    Ok(())
}
