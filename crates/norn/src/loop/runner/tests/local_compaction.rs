use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn seed_compaction_history(store: &EventStore) -> TestResult {
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
    Ok(())
}

// -- REVIEW item 6b: compaction must affect the in-flight request ------

#[tokio::test]
async fn auto_compaction_applies_to_in_flight_request() -> TestResult {
    let store = EventStore::new();
    // Seed enough chunky history that the estimate crosses the
    // threshold on the very first iteration.
    seed_compaction_history(&store)?;

    // First scripted response answers the summarization call, the
    // second answers the main (compacted) request.
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
    let (_, usage) = assert_completed(result);
    // Track L finding 1: the summarization call's usage (10/5 from the
    // first scripted response) is accounted alongside the main call's.
    assert_eq!(usage.input_tokens, 20, "summarization input tokens vanish");
    assert_eq!(
        usage.output_tokens, 10,
        "summarization output tokens vanish"
    );

    let requests = provider.requests()?;
    assert_eq!(
        requests.len(),
        2,
        "expected the summarization call plus the main call",
    );
    // The summarization request is isolated: untooled and unthreaded.
    let summarization = &requests[0];
    assert!(summarization.tools.is_empty());
    assert!(summarization.previous_response_id.is_none());
    assert!(!summarization.store);
    assert!(
        summarization.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "the summarization prompt must cover the elided history",
    );

    // The compaction must have hit the FIRST main request (in-flight),
    // not just the next step: compacted turns absent, summary present.
    let main = &requests[1];
    assert!(
        !main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "compacted history must be absent from the in-flight request",
    );
    let summary_present = main.messages.iter().any(|m| {
        matches!(m.role, MessageRole::Developer)
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("LLM summary of the seed turns"))
    });
    assert!(
        summary_present,
        "in-flight request must carry the LLM-written compaction summary",
    );
    // The most recent seeded turn survives (keep_recent_turns = 1).
    assert!(
        main.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed answer 5"))
        }),
        "kept turns must remain in the in-flight request",
    );
    // And the persisted state agrees for the next step: the compaction
    // record carries the LLM summary as its content.
    let persisted_summary = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Compaction { summary, .. } => Some(summary),
        _ => None,
    });
    assert_eq!(
        persisted_summary.as_deref(),
        Some("LLM summary of the seed turns"),
        "the compaction record's content must be the LLM summary",
    );
    // The summarization audit event is persisted with its usage.
    let audit = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Custom {
            event_type, data, ..
        } if event_type == "loop.compaction_summarization" => Some(data),
        _ => None,
    });
    let audit = audit.ok_or_else(|| {
        std::io::Error::other("loop.compaction_summarization event was not persisted")
    })?;
    assert_eq!(audit["summary_kind"], "llm_summary");
    assert_eq!(audit["usage"]["input_tokens"], 10);
    assert_eq!(audit["usage"]["output_tokens"], 5);
    Ok(())
}

/// C4: a fired auto-compaction broadcasts a live [`AgentCompaction`]
/// event carrying honest accounting (reclaimed-token estimate plus the
/// summarization call's real usage); the summarization sub-call's own
/// provider stream (its `Done` / text deltas) must NOT leak onto the
/// agent event channel; and the persisted `SessionEvent::Compaction` is
/// unchanged.
#[tokio::test]
async fn auto_compaction_broadcasts_live_event_and_hides_summarization_stream() -> TestResult {
    use crate::provider::agent_event::{AgentEvent, AgentEventKind, CompactionSummaryKind};

    let store = EventStore::new();
    seed_compaction_history(&store)?;

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

    let config = AgentLoopConfig {
        context_window_limit: Some(100),
        auto_compact_reserve_tokens: Some(50),
        auto_compact_keep_recent_turns: 1,
        ..AgentLoopConfig::default()
    };

    let (tx, mut rx) = tokio::sync::broadcast::channel::<AgentEvent>(256);
    let sender = AgentEventSender::new(tx, uuid::Uuid::nil(), "root".to_string());

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &config,
            event_tx: Some(&sender),
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    let _ = assert_completed(result);

    let mut compactions = Vec::new();
    let mut provider_dones = 0usize;
    let mut leaked_summary = false;
    while let Ok(ev) = rx.try_recv() {
        match ev.event {
            AgentEventKind::Compaction(compaction) => compactions.push(compaction),
            AgentEventKind::Provider(ProviderEvent::Done { .. }) => provider_dones += 1,
            AgentEventKind::Provider(
                ProviderEvent::TextDelta { text } | ProviderEvent::TextComplete { text },
            ) if text.contains("LLM summary of the seed turns") => {
                leaked_summary = true;
            }
            _ => {}
        }
    }

    assert_eq!(
        compactions.len(),
        1,
        "exactly one live compaction event must broadcast",
    );
    let compaction = compactions
        .first()
        .ok_or_else(|| std::io::Error::other("live compaction event was not broadcast"))?;
    assert!(
        compaction.events_compacted > 0,
        "the event must report the hidden turns",
    );
    assert!(
        compaction.tokens_before > compaction.tokens_after,
        "reclaim must be positive: {} -> {}",
        compaction.tokens_before,
        compaction.tokens_after,
    );
    assert!(matches!(
        compaction.summary_source,
        CompactionSummaryKind::Llm
    ));
    let usage = compaction
        .summarization_usage
        .as_ref()
        .ok_or_else(|| std::io::Error::other("summarization usage was not carried"))?;
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);

    assert!(
        !leaked_summary,
        "the summarization sub-call's text must never leak onto the agent stream",
    );
    assert_eq!(
        provider_dones, 1,
        "only the main call's Done may broadcast — the summarization sub-call's must not",
    );

    // The session-store Compaction event is unchanged by the live broadcast.
    let persisted_summary = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Compaction { summary, .. } => Some(summary),
        _ => None,
    });
    assert_eq!(
        persisted_summary.as_deref(),
        Some("LLM summary of the seed turns"),
    );

    // Session-fidelity Gap 9: the persisted `loop.compaction_summarization`
    // audit record carries the SAME facts as the live broadcast — a
    // log-only consumer reproduces the live event's accounting exactly.
    let persisted_audit = store
        .events()
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == "loop.compaction_summarization" => Some(data),
            _ => None,
        })
        .ok_or_else(|| {
            std::io::Error::other("the compaction-summarization audit record did not persist")
        })?;
    assert_eq!(
        persisted_audit["compaction_id"].as_str(),
        Some(compaction.compaction_id.to_string().as_str()),
        "audit: {persisted_audit}",
    );
    assert_eq!(
        persisted_audit["events_compacted"].as_u64(),
        Some(compaction.events_compacted as u64),
    );
    assert_eq!(
        persisted_audit["tokens_before"].as_u64(),
        Some(compaction.tokens_before),
    );
    assert_eq!(
        persisted_audit["tokens_after"].as_u64(),
        Some(compaction.tokens_after),
    );
    assert_eq!(persisted_audit["model"].as_str(), Some("test-model"));
    assert_eq!(compaction.model, "test-model");
    assert_eq!(
        persisted_audit["freed_token_estimate"].as_u64(),
        Some(compaction.freed_token_estimate as u64),
    );
    assert_eq!(
        persisted_audit["summary_kind"].as_str(),
        Some("llm_summary"),
    );
    assert_eq!(
        persisted_audit["usage"]["input_tokens"].as_u64(),
        Some(usage.input_tokens),
    );
    assert_eq!(
        persisted_audit["usage"]["output_tokens"].as_u64(),
        Some(usage.output_tokens),
    );
    Ok(())
}

/// Track L finding 1 (failure policy): a failed summarization call
/// must not abort the step — the compaction still fires with the
/// mechanical digest, explicitly marked as a non-semantic fallback.
#[tokio::test]
async fn summarization_failure_falls_back_without_aborting_the_step() -> TestResult {
    let store = EventStore::new();
    seed_compaction_history(&store)?;

    // A truncated summarization response (MaxTokens) is unusable; the
    // main call then succeeds. Its usage must still be accounted.
    let provider = MockProvider::new(vec![
        vec![text_delta("cut off"), done_event(StopReason::MaxTokens)],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

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
    let (_, usage) = assert_completed(result);
    assert_eq!(
        usage.input_tokens, 20,
        "rejected summarization tokens were still spent and must be accounted",
    );

    let persisted_summary = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Compaction { summary, .. } => Some(summary),
        _ => None,
    });
    let summary = persisted_summary
        .ok_or_else(|| std::io::Error::other("compaction did not fire on fallback"))?;
    let parsed: serde_json::Value = serde_json::from_str(&summary)?;
    assert_eq!(parsed["summary_kind"], "mechanical_digest_fallback");
    assert!(
        parsed["summarization_error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "the fallback must carry why the LLM summary was unavailable: {parsed}",
    );

    let audit = store.events().into_iter().find_map(|e| match e {
        SessionEvent::Custom {
            event_type, data, ..
        } if event_type == "loop.compaction_summarization" => Some(data),
        _ => None,
    });
    let audit =
        audit.ok_or_else(|| std::io::Error::other("audit event was not persisted on fallback"))?;
    assert_eq!(audit["summary_kind"], "mechanical_digest_fallback");
    Ok(())
}
