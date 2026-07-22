use super::*;
use crate::profile::PromptCommand;

fn counting_prompt_context(working_dir: &std::path::Path) -> LoopContext {
    let mut loop_context = LoopContext::with_working_dir(
        "legacy",
        crate::tool::context::SharedWorkingDir::new(working_dir.to_path_buf()),
    );
    loop_context.prompt_commands.push(PromptCommand {
        name: "execution-counter".to_owned(),
        command: "printf runtime; printf x >> prompt-command-executions".to_owned(),
        cache_ttl: None,
    });
    loop_context
}

struct SeedReplayGuardProvider {
    calls: AtomicUsize,
    identity: crate::provider::ProviderStateIdentity,
    first_response_uses_tool: bool,
}

impl Default for SeedReplayGuardProvider {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            identity: crate::provider::ProviderStateIdentity::derive(
                "norn.d8.prompt-command-replay",
                b"stable-test-identity",
            ),
            first_response_uses_tool: false,
        }
    }
}

impl SeedReplayGuardProvider {
    fn with_tool_continuation() -> Self {
        Self {
            first_response_uses_tool: true,
            ..Self::default()
        }
    }
}

impl Provider for SeedReplayGuardProvider {
    fn state_identity(&self) -> Option<crate::provider::ProviderStateIdentity> {
        Some(self.identity)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::openai_responses()
    }

    fn validate_replay(&self, messages: &[Message]) -> Result<(), ProviderError> {
        if messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant)
        {
            return Err(ProviderError::ProviderStateReplayUnavailable);
        }
        Ok(())
    }

    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call != 0 {
            return Err(ProviderError::InvalidRequest {
                message: "replay guard allowed a second provider call".to_owned(),
            });
        }
        let events = if self.first_response_uses_tool {
            vec![
                Ok(tool_call_delta(
                    "fc_continue_work",
                    Some("continue_work"),
                    "{}",
                )),
                Ok(ProviderEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                    response_id: Some("resp_seed_guard".to_owned()),
                }),
            ]
        } else {
            vec![
                Ok(ProviderEvent::TextDelta {
                    text: "first answer".to_owned(),
                }),
                Ok(ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    response_id: Some("resp_seed_guard".to_owned()),
                }),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[tokio::test]
async fn operator_prompt_command_is_developer_bound_and_cuts_only_on_change() -> TestResult {
    const COMMAND_V1: &str = "D8-PROMPT-COMMAND-V1";
    const COMMAND_V2: &str = "D8-PROMPT-COMMAND-V2";

    let server = MockServer::start().await;
    mount_response_sequence(
        &server,
        &["resp_command_1", "resp_command_2", "resp_command_3"],
    )
    .await?;
    let provider = wire_provider(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("prompt-command-executions");

    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product-policy");
    let mut loop_context = LoopContext::with_working_dir(
        "legacy",
        crate::tool::context::SharedWorkingDir::new(working_dir.path().to_path_buf()),
    );
    loop_context.install_stable_prompt_plan(plan);
    loop_context.prompt_commands.push(PromptCommand {
        name: "runtime-state".to_owned(),
        command: format!("printf {COMMAND_V1}; printf x >> prompt-command-executions"),
        cache_ttl: None,
    });

    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "task-one").await?);
    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "task-two").await?);
    loop_context.prompt_commands[0].command =
        format!("printf {COMMAND_V2}; printf x >> prompt-command-executions");
    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "task-three").await?);

    let payloads = received_payloads(&server, 3).await?;
    assert!(payloads[0].get("previous_response_id").is_none());
    assert_eq!(payloads[1]["previous_response_id"], "resp_command_1");
    assert!(
        payloads[2].get("previous_response_id").is_none(),
        "changed Developer command output must cut the old anchor",
    );

    let first = serde_json::to_string(&payloads[0])?;
    let second = serde_json::to_string(&payloads[1])?;
    let third = serde_json::to_string(&payloads[2])?;
    assert_eq!(first.matches(COMMAND_V1).count(), 1);
    assert!(!second.contains(COMMAND_V1));
    assert_eq!(third.matches(COMMAND_V2).count(), 1);
    assert!(!third.contains(COMMAND_V1));
    for (payload, command) in [(&payloads[0], COMMAND_V1), (&payloads[2], COMMAND_V2)] {
        assert!(payload["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["role"] == "developer"
                    && serde_json::to_string(item).is_ok_and(|encoded| encoded.contains(command))
            })
        }));
        assert!(
            !payload["instructions"]
                .as_str()
                .is_some_and(|instructions| {
                    instructions.contains(COMMAND_V1) || instructions.contains(COMMAND_V2)
                })
        );
    }

    let seeds = store
        .events()
        .iter()
        .filter_map(|event| ProviderStateProvenance::from_event(event).ok().flatten())
        .filter_map(|provenance| provenance.prompt_seed_fingerprint())
        .collect::<Vec<_>>();
    assert_eq!(seeds.len(), 3);
    assert_eq!(seeds[0], seeds[1]);
    assert_ne!(seeds[1], seeds[2]);
    assert_eq!(
        std::fs::read_to_string(execution_log)?,
        "xxx",
        "each request must execute its prompt command exactly once",
    );
    Ok(())
}

#[tokio::test]
async fn changed_cached_command_seed_rejects_replay_before_prompt_persistence() -> TestResult {
    let provider = SeedReplayGuardProvider::default();
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };
    let mut loop_context = LoopContext::new("legacy");
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product-policy");
    loop_context.install_stable_prompt_plan(plan);
    loop_context.prompt_commands.push(PromptCommand {
        name: "runtime-state".to_owned(),
        command: "printf D8-CACHED-V1".to_owned(),
        cache_ttl: Some(Duration::from_mins(5)),
    });

    let first = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "first task",
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;
    assert_completed(first);
    let before_second = serde_json::to_vec(&store.events())?;

    loop_context.prompt_commands[0].command = "printf D8-CACHED-V2".to_owned();
    let second = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "must not persist",
        tools: &[],
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
        second,
        Err(NornError::Provider(
            ProviderError::ProviderStateReplayUnavailable
        ))
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        serde_json::to_vec(&store.events())?,
        before_second,
        "changed command authority must fail before appending the new prompt",
    );
    Ok(())
}

#[tokio::test]
async fn later_command_seed_cut_uses_provider_neutral_replay_guard() -> TestResult {
    let provider = SeedReplayGuardProvider::with_tool_continuation();
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("prompt-command-executions");
    let mut loop_context = LoopContext::with_working_dir(
        "legacy",
        crate::tool::context::SharedWorkingDir::new(working_dir.path().to_path_buf()),
    );
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product-policy");
    loop_context.install_stable_prompt_plan(plan);
    loop_context.prompt_commands.push(PromptCommand {
        name: "changing-runtime".to_owned(),
        command: "if [ -e prompt-command-marker ]; then printf D8-HOT-V2; \
                  else : > prompt-command-marker; printf D8-HOT-V1; fi; \
                  printf x >> prompt-command-executions"
            .to_owned(),
        cache_ttl: None,
    });
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "continue_work".to_owned(),
        Box::new(|_| Ok(json!({"continued": true}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let tools = [ToolDefinition {
        name: "continue_work".to_owned(),
        description: "Continue the fixture".to_owned(),
        parameters: json!({}),
    }];
    let store = EventStore::new();
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        max_iterations: Some(3),
        ..AgentLoopConfig::default()
    };

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "continue once",
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
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(execution_log)?,
        "xx",
        "setup and the continuation must each execute the command once",
    );
    Ok(())
}

#[tokio::test]
async fn d8_prompt_command_does_not_run_when_cancelled_before_first_request() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("prompt-command-executions");
    let mut loop_context = counting_prompt_context(working_dir.path());
    let provider = MockProvider::new(Vec::new());
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig::default();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "cancelled task",
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: Some(cancel),
    })
    .await?;

    assert!(matches!(result, AgentStepResult::Cancelled { .. }));
    assert_eq!(provider.call_count(), 0);
    assert!(!execution_log.exists());
    Ok(())
}

#[tokio::test]
async fn d8_prompt_command_does_not_run_with_zero_iteration_budget() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("prompt-command-executions");
    let mut loop_context = counting_prompt_context(working_dir.path());
    let provider = MockProvider::new(Vec::new());
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig {
        max_iterations: Some(0),
        ..AgentLoopConfig::default()
    };

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "budgeted task",
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    })
    .await?;

    assert!(matches!(
        result,
        AgentStepResult::MaxIterationsReached { .. }
    ));
    assert_eq!(provider.call_count(), 0);
    assert!(!execution_log.exists());
    Ok(())
}

#[tokio::test]
async fn d8_pre_cancelled_wake_persists_seed_without_running_command() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let execution_log = working_dir.path().join("prompt-command-executions");
    let mut loop_context = counting_prompt_context(working_dir.path());
    let provider = MockProvider::new(Vec::new());
    let executor = MockToolExecutor::empty();
    let store = EventStore::new();
    let config = AgentLoopConfig::default();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let seed = make_channel_message(
        "wake-source",
        "D8-CANCELLED-WAKE-SEED",
        crate::r#loop::inbound::MessageKind::Steer,
        0,
    );

    let result = run_agent_step_from_messages(AgentMessageStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        initial_messages: vec![seed],
        inbound: None,
        loop_context: &mut loop_context,
        cancel: Some(cancel),
    })
    .await?;

    assert!(matches!(result, AgentStepResult::Cancelled { .. }));
    assert_eq!(provider.call_count(), 0);
    assert!(!execution_log.exists());
    let persisted_seeds = store
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                SessionEvent::UserMessage { content, .. }
                    if content.contains("D8-CANCELLED-WAKE-SEED")
            )
        })
        .count();
    assert_eq!(
        persisted_seeds, 1,
        "an accepted coordination-less wake seed must survive exactly once",
    );
    Ok(())
}
