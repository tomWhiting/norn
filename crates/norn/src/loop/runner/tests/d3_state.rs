use super::*;
use crate::session::response_publication_fixture;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Provider-owned response threading and local summarization are mutually
/// exclusive. The token preflight still observes the full context, but the
/// request keeps its anchor and sends provider-side compaction policy.
#[tokio::test]
async fn threaded_state_uses_provider_compaction_without_local_summarization() -> TestResult {
    let store = EventStore::new();
    let state_identity = crate::provider::ProviderStateIdentity::derive(
        "norn.runner.compaction-test",
        b"threaded-compaction-fixture",
    );
    store.validate_or_bind_provider_state_identity(Some(state_identity))?;
    for i in 0..6 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("seed question {i} {}", "x".repeat(200)),
        })?;
        let fixture = response_publication_fixture(store.last_event_id(), true)?;
        let assistant = SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: fixture.assistant_base,
            content: format!("seed answer {i} {}", "y".repeat(200)),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: Some(format!("resp_seed_{i}")),
        };
        let publication = crate::session::committed_response_publication(
            fixture.boundary,
            fixture.provenance,
            assistant,
        )?;
        store.append_batch(&publication)?;
    }

    let provider = MockProvider::with_capabilities(
        vec![vec![text_delta("done"), done_event(StopReason::EndTurn)]],
        crate::provider::tools::ProviderCapabilities::openai_responses(),
    )
    .with_state_identity(state_identity);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));
    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        conversation_state: crate::r#loop::config::ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests()?;
    assert_eq!(
        requests.len(),
        1,
        "threaded preflight must not spend a local summarization request",
    );
    let main = &requests[0];
    assert_eq!(
        main.previous_response_id.as_deref(),
        Some("resp_seed_5"),
        "provider-owned history keeps its durable anchor",
    );
    assert_eq!(
        main.context_management
            .as_ref()
            .map(|management| management.compact_threshold_tokens),
        Some(50),
        "the existing context limit minus reserve derives the server threshold",
    );
    assert!(
        !main.messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("seed question"))
        }),
        "threaded input must remain a delta rather than replaying old history",
    );
    assert!(
        !store
            .events()
            .iter()
            .any(|event| matches!(event, SessionEvent::Compaction { .. })),
        "provider-threaded state must not create a local compaction record",
    );
    let warnings = store
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == "loop.token_warning" => Some(data),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        warnings.len(),
        1,
        "threaded preflight must still observe the reconstructed context",
    );
    let warning = &warnings[0];
    let estimated = warning["estimated"].as_u64();
    assert!(estimated.is_some_and(|value| value > 100));
    assert!(warning["usage_floor"].is_null());
    assert_eq!(warning["effective"].as_u64(), estimated);
    assert_eq!(warning["limit"].as_u64(), Some(100));
    Ok(())
}

/// Public threading uses replaceable Responses instructions for dynamic
/// context on both the first request and anchored continuations.
#[tokio::test]
async fn threaded_dynamic_context_is_replaceable_instructions_not_input() -> TestResult {
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{
        DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming as TT,
    };
    use crate::system_prompt::environment::EnvironmentConfig;

    let first = vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: Some("resp_dynamic_1".to_owned()),
        },
    ];
    let second = vec![
        text_delta("done"),
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: Some("resp_dynamic_2".to_owned()),
        },
    ];
    let identity = crate::provider::ProviderStateIdentity::derive(
        "norn.runner.dynamic-instructions",
        b"dynamic-instructions-fixture",
    );
    let provider = MockProvider::with_capabilities(
        vec![first, second],
        crate::provider::tools::ProviderCapabilities::openai_responses(),
    )
    .with_state_identity(identity);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_owned(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let write_tool = ToolDefinition {
        name: "write".to_owned(),
        description: "Write a file".to_owned(),
        parameters: serde_json::json!({}),
    };
    let rule = Rule {
        id: RuleId::from("rust-conventions"),
        name: "Rust Conventions".to_owned(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_owned(),
        }],
        delivery: RDM::SystemContextAppend,
        timing: TT::Before,
        body: "Follow Rust conventions.".to_owned(),
        shell_source: None,
    };
    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    loop_ctx.environment = Some(EnvironmentConfig {
        session_id: Some("sess-dynamic-instructions".to_owned()),
        model: "test-model".to_owned(),
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[write_tool],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    let payloads = requests
        .iter()
        .map(|request| crate::provider::openai::request::build_payload(request, "responses_api"))
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(payloads[0]["store"], true);
    assert!(payloads[0]["previous_response_id"].is_null());
    assert_eq!(payloads[1]["store"], true);
    assert_eq!(payloads[1]["previous_response_id"], "resp_dynamic_1");
    for (index, payload) in payloads.iter().enumerate() {
        let instructions = payload["instructions"].as_str();
        assert!(instructions.is_some_and(|text| text.contains("base-system")));
        assert!(instructions.is_some_and(|text| text.contains("# Environment")));
        assert_eq!(
            instructions.map(|text| text.matches("# Environment").count()),
            Some(1),
            "request {index} must carry one current dynamic context"
        );
        assert!(
            payload["input"]
                .as_array()
                .is_some_and(|input| input.iter().all(|item| item["role"] != "developer")),
            "request {index} must not persist managed context as a Developer input item"
        );
    }
    assert!(
        !payloads[0]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains("Follow Rust conventions."))
    );
    assert!(
        payloads[1]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains("Follow Rust conventions."))
    );
    Ok(())
}
