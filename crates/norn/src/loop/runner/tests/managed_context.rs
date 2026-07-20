use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// -- PROMPT-CACHE fix: the managed dynamic-context message rides the tail --

/// Acceptance test for the prompt-cache-invalidation fix. Across two turns
/// whose managed dynamic-context message differs, every message BEFORE that
/// message is byte-identical (a stable, fully-cacheable prefix), and the
/// managed message is always the LAST message in the request. The pre-fix
/// layout (managed message at index 1, ahead of history) would break this:
/// its per-turn byte change invalidated the cache for all of history.
#[tokio::test]
async fn managed_dev_message_rides_the_tail_keeping_the_history_prefix_byte_stable() -> TestResult {
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{
        DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming as TT,
    };
    use crate::system_prompt::environment::EnvironmentConfig;

    // Turn 1 writes a `.rs` file (firing a Before-timing SystemContextAppend
    // rule); turn 2 ends. The rule body enters the managed dynamic-context
    // message only from turn 2 on, so the two managed messages differ
    // deterministically — no wall-clock dependence.
    let turn1 = vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![text_delta("done"), done_event(StopReason::EndTurn)];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };
    let rule = Rule {
        id: RuleId::from("rust-conventions"),
        name: "Rust Conventions".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::SystemContextAppend,
        timing: TT::Before,
        body: "Follow Rust conventions.".to_string(),
        shell_source: None,
    };

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    loop_ctx.environment = Some(EnvironmentConfig {
        session_id: Some("sess-cache".to_owned()),
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
    assert_eq!(requests.len(), 2, "two provider calls");

    // Each request ENDS with the managed dynamic-context Developer message.
    for (i, req) in requests.iter().enumerate() {
        let Some(last) = req.messages.last() else {
            return Err(std::io::Error::other(format!("request {i} was empty")).into());
        };
        assert_eq!(
            last.role,
            MessageRole::Developer,
            "request {i} must end with the managed Developer message",
        );
        assert!(
            last.content
                .as_deref()
                .is_some_and(|c| c.contains("# Environment")),
            "request {i} tail must carry the dynamic context, got: {:?}",
            last.content,
        );
        // Exactly one managed message per request: a silently-failed detach
        // would leave a stale copy mid-history while a fresh tail message
        // keeps every other assertion here green.
        let managed_count = req
            .messages
            .iter()
            .filter(|m| {
                m.role == MessageRole::Developer
                    && m.content
                        .as_deref()
                        .is_some_and(|c| c.contains("# Environment"))
            })
            .count();
        assert_eq!(
            managed_count, 1,
            "request {i} must carry exactly one managed dynamic-context message",
        );
    }

    // The tail managed message genuinely changes across turns (turn 2 gains
    // the rule body) — the per-turn volatility the fix isolates to the tail.
    let dev1 = requests[0]
        .messages
        .last()
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let dev2 = requests[1]
        .messages
        .last()
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    assert_ne!(dev1, dev2, "the tail managed message must change per turn");
    assert!(
        dev2.contains("Follow Rust conventions."),
        "turn 2's managed message must carry the rule body",
    );
    assert!(
        !dev1.contains("Follow Rust conventions."),
        "turn 1's managed message must not yet carry the rule body",
    );

    // The cacheable prefix — every message BEFORE the tail managed message —
    // is byte-identical across turns: request 1's non-managed messages are a
    // byte-for-byte prefix of request 2's non-managed messages.
    let prefix1 = &requests[0].messages[..requests[0].messages.len() - 1];
    let prefix2 = &requests[1].messages[..requests[1].messages.len() - 1];
    assert!(
        prefix2.len() > prefix1.len(),
        "history must have grown across turns: {} then {}",
        prefix1.len(),
        prefix2.len(),
    );
    for (j, msg) in prefix1.iter().enumerate() {
        let bytes1 = serde_json::to_string(msg)?;
        let bytes2 = serde_json::to_string(&prefix2[j])?;
        assert_eq!(
            bytes1, bytes2,
            "message {j} must be byte-identical across turns (cacheable prefix)",
        );
    }
    // The System message specifically stays byte-stable and unexpanded.
    assert_eq!(
        requests[0].messages[0].content.as_deref(),
        Some("base-system")
    );
    assert_eq!(
        requests[1].messages[0].content.as_deref(),
        Some("base-system")
    );
    Ok(())
}

/// Trap 1 (compaction ordering): when auto-compaction fires in-flight, the
/// request builder detaches the managed message before the preflight and
/// re-attaches it at the tail afterwards. The resulting in-flight request
/// must therefore end with exactly one managed dynamic-context message,
/// sitting AFTER the freshly appended compaction summary — proving the
/// detach/attach-around-preflight ordering keeps a single managed message at
/// the tail while compaction rewrites the history it sits after.
#[tokio::test]
async fn in_flight_compaction_leaves_one_managed_message_after_the_summary_at_the_tail()
-> TestResult {
    use crate::system_prompt::environment::EnvironmentConfig;

    let store = EventStore::new();
    // Seed chunky history so the estimate crosses the threshold immediately.
    for i in 0..6 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("seed question {i} {}", "x".repeat(200)),
        })?;
        store.append(SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: format!("seed answer {i} {}", "y".repeat(200)),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_string(),
            response_id: None,
        })?;
    }

    // First scripted response answers the summarization call, the second
    // answers the main (compacted) request.
    let provider = MockProvider::new(vec![
        vec![
            text_delta("LLM summary of the seed turns"),
            done_event(StopReason::EndTurn),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));
    // A non-empty managed dynamic-context message (distinguishable from the
    // compaction summary by its `# Environment` heading).
    loop_ctx.environment = Some(EnvironmentConfig {
        session_id: Some("sess-compact".to_owned()),
        model: "test-model".to_owned(),
    });

    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
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
    assert_eq!(requests.len(), 2, "summarization call plus the main call");
    let main = &requests[1];
    assert!(!main.store, "local compaction remains a stateless request");
    assert!(main.previous_response_id.is_none());
    assert!(main.context_management.is_none());

    // Compacted history is gone; the LLM summary is present.
    assert!(
        !main.messages.iter().any(|m| m
            .content
            .as_deref()
            .is_some_and(|c| c.contains("seed question 0"))),
        "compacted history must be absent from the in-flight request",
    );
    // The kept turn (keep_recent_turns = 1) survives.
    assert!(
        main.messages.iter().any(|m| m
            .content
            .as_deref()
            .is_some_and(|c| c.contains("seed answer 5"))),
        "the kept recent turn must remain in the in-flight request",
    );

    // Exactly one managed dynamic-context message, and it is LAST.
    let managed_count = main
        .messages
        .iter()
        .filter(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("# Environment"))
        })
        .count();
    assert_eq!(
        managed_count, 1,
        "exactly one managed dynamic-context message"
    );
    let Some(last) = main.messages.last() else {
        return Err(std::io::Error::other("main request was empty").into());
    };
    assert_eq!(last.role, MessageRole::Developer);
    assert!(
        last.content
            .as_deref()
            .is_some_and(|c| c.contains("# Environment")),
        "the managed message must be the tail message, got: {:?}",
        last.content,
    );

    // The compaction summary is present as a Developer message that is NOT
    // last — the managed message was attached AFTER it.
    let summary_idx = main.messages.iter().position(|m| {
        matches!(m.role, MessageRole::Developer)
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("LLM summary of the seed turns"))
    });
    let Some(summary_idx) = summary_idx else {
        return Err(std::io::Error::other(
            "in-flight request did not carry the compaction summary",
        )
        .into());
    };
    assert!(
        summary_idx < main.messages.len() - 1,
        "the compaction summary must sit BEFORE the tail managed message",
    );
    Ok(())
}
