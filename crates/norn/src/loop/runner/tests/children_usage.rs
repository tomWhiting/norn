use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- W3.6: pre-loop child-result drain folds children_usage ----------

/// Child results already buffered when the step starts are drained
/// by the runner's pre-loop sweep; each drained result's
/// `subtree_usage` must be folded into the step's `children_usage`
/// (summed across the batch) while the step's own `usage` stays
/// own-calls-only — the two never mix.
#[tokio::test]
async fn buffered_child_results_fold_into_children_usage_at_step_start() -> TestResult {
    use crate::agent::result_channel::ChildAgentResult;
    use uuid::Uuid;

    let provider = MockProvider::new(vec![vec![
        text_delta("done"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, rx) = tokio::sync::mpsc::channel(4);
    for (input, output) in [(7_u64, 3_u64), (11, 6)] {
        tx.send(ChildAgentResult {
            agent_id: Uuid::new_v4(),
            agent_role: "spawn/worker".to_string(),
            succeeded: true,
            formatted_message: "child done".to_string(),
            error: None,
            stop: None,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
            subtree_usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
        })
        .await?;
    }
    drop(tx);

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.child_result_rx = Some(rx);

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "prompt",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;

    let (usage, children_usage) = match result {
        AgentStepResult::Completed {
            usage,
            children_usage,
            ..
        } => (usage, children_usage),
        other => {
            return Err(
                std::io::Error::other(format!("expected Completed, received {other:?}")).into(),
            );
        }
    };
    assert_eq!(usage.input_tokens, 10, "own usage is own calls only");
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(
        children_usage.input_tokens, 18,
        "both buffered subtrees fold exactly once: 7 + 11",
    );
    assert_eq!(children_usage.output_tokens, 9, "3 + 6");
    Ok(())
}

/// Child results that arrive while a parent is executing tools must
/// be injected before the next provider request, not held until the
/// parent reaches a would-stop boundary.
#[tokio::test]
async fn child_results_arriving_during_tool_iteration_reach_next_request() -> TestResult {
    use crate::agent::result_channel::ChildAgentResult;
    use uuid::Uuid;

    let provider = MockProvider::new(vec![
        vec![
            tool_call_delta("tc1", Some("send_child_result"), "{}"),
            done_event(StopReason::ToolUse),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.child_result_rx = Some(rx);

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "send_child_result".to_string(),
        Box::new(move |_| {
            tx.try_send(ChildAgentResult {
                agent_id: Uuid::new_v4(),
                agent_role: "spawn/worker".to_string(),
                succeeded: true,
                formatted_message: "child finished during tool batch".to_string(),
                error: None,
                stop: None,
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                },
                subtree_usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                },
            })
            .map_err(|error| crate::error::ToolError::ExecutionFailed {
                reason: format!("child-result fixture could not enqueue its message: {error}"),
            })?;
            Ok(serde_json::json!({ "queued_child_result": true }))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let tools = [ToolDefinition {
        name: "send_child_result".to_string(),
        description: "queue a child result".to_string(),
        parameters: serde_json::json!({}),
    }];

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &tools,
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let children_usage = match result {
        AgentStepResult::Completed { children_usage, .. } => children_usage,
        other => {
            return Err(
                std::io::Error::other(format!("expected Completed, received {other:?}")).into(),
            );
        }
    };
    assert_eq!(children_usage.input_tokens, 7);
    assert_eq!(children_usage.output_tokens, 3);

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2, "tool result should force a second turn");
    let second_request = requests
        .get(1)
        .ok_or_else(|| std::io::Error::other("provider recorded no second request"))?;
    let second_request_text = second_request
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("<agent_result from=\"spawn/worker\"")
            && second_request_text.contains("child finished during tool batch"),
        "second request must include the prompt child result: {second_request_text}",
    );
    Ok(())
}

/// REVIEW W3.6 HIGH-1 regression: every step's `children_usage`
/// covers ONLY the results delivered into that step. A reused
/// `LoopContext` (interactive sessions run many steps over one
/// context) must not carry step 1's children into step 2's
/// snapshot — pre-fix, the accumulator was monotonic for the
/// context's lifetime and did exactly that.
#[tokio::test]
async fn reused_loop_context_reports_each_steps_children_only() -> TestResult {
    use crate::agent::result_channel::ChildAgentResult;
    use uuid::Uuid;

    let child_result = |input: u64, output: u64| ChildAgentResult {
        agent_id: Uuid::new_v4(),
        agent_role: "spawn/worker".to_string(),
        succeeded: true,
        formatted_message: "child done".to_string(),
        error: None,
        stop: None,
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        },
        subtree_usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        },
    };

    let provider = MockProvider::new(vec![
        vec![text_delta("turn one"), done_event(StopReason::EndTurn)],
        vec![text_delta("turn two"), done_event(StopReason::EndTurn)],
    ]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.child_result_rx = Some(rx);

    tx.send(child_result(7, 3)).await?;
    let step_one = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "first",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    let children_usage = match step_one {
        AgentStepResult::Completed { children_usage, .. } => children_usage,
        other => {
            return Err(std::io::Error::other(format!(
                "expected step one to complete, received {other:?}"
            ))
            .into());
        }
    };
    assert_eq!(children_usage.input_tokens, 7, "step 1 sees its child");

    tx.send(child_result(11, 6)).await?;
    let step_two = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "second",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    let children_usage = match step_two {
        AgentStepResult::Completed { children_usage, .. } => children_usage,
        other => {
            return Err(std::io::Error::other(format!(
                "expected step two to complete, received {other:?}"
            ))
            .into());
        }
    };
    assert_eq!(
        children_usage.input_tokens, 11,
        "step 2 reports ONLY step 2's delivery — 18 here means \
         step 1's child leaked across the reset boundary",
    );
    assert_eq!(children_usage.output_tokens, 6);
    Ok(())
}
