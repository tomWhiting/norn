use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- REVIEW H2: developer-message sync must not clobber history --------

/// Resume with a compaction summary in history and no dynamic context:
/// pre-fix, the sync's first-Developer-role lookup matched the summary
/// and the `(None, Some(idx))` arm deleted it from the prompt.
#[tokio::test]
async fn history_compaction_summary_survives_dev_sync() -> TestResult {
    let store = EventStore::new();
    store.append(SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "older history summary".to_string(),
        replaced_event_ids: Vec::new(),
    })?;

    let provider = MockProvider::new(vec![vec![
        text_delta("hi"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
    let summary_present = requests[0].messages.iter().any(|m| {
        matches!(m.role, MessageRole::Developer)
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("older history summary"))
    });
    assert!(
        summary_present,
        "history compaction summary must survive the developer-message sync: {:?}",
        requests[0].messages,
    );
    Ok(())
}

/// Seam I2-1 (quadratic compaction re-walk): persisted compaction
/// marks load exactly once per loop context. Step 1 of a resumed
/// session (fresh `ContextEdits`, compaction already in the store)
/// walks the store and hides superseded history; step 2 must NOT
/// re-walk — proven by appending a raw compaction event between the
/// steps that only a re-walk could observe and asserting its
/// replaced event stays visible — while the step-1 marks survive.
#[tokio::test]
async fn persisted_compaction_marks_load_once_per_loop_context() -> TestResult {
    let store = EventStore::new();
    let old_question = store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "old question".to_string(),
    })?;
    let old_answer = store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "old answer".to_string(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_string(),
        response_id: None,
    })?;
    // Persist a compaction the way a *previous run* would have — on a
    // tracker this loop context never sees.
    let mut prior_run_edits = crate::session::context_edit::ContextEdits::new();
    prior_run_edits.summarize(
        &store,
        vec![old_question, old_answer],
        "seeded summary".to_string(),
    )?;

    let provider = MockProvider::new(vec![
        vec![text_delta("first"), done_event(StopReason::EndTurn)],
        vec![text_delta("second"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    assert!(!loop_ctx.context_marks_loaded);

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: None,
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);
    assert!(
        loop_ctx.context_marks_loaded,
        "the first step must record the one-time load",
    );

    // Between the steps, append a compaction event directly to the
    // store, superseding step 1's assistant turn. Nothing marks it on
    // the loop's tracker — only a per-step store re-walk could pick
    // it up, and the re-walk no longer exists.
    let first_answer_id = store
        .events()
        .iter()
        .find_map(|e| match e {
            SessionEvent::AssistantMessage { base, content, .. } if content == "first" => {
                Some(base.id.clone())
            }
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("step-1 assistant event was not persisted"))?;
    store.append(SessionEvent::Compaction {
        base: EventBase::new(store.last_event_id()),
        summary: "rogue walk detector".to_string(),
        replaced_event_ids: vec![first_answer_id],
    })?;

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
    let contains = |req: &crate::provider::request::ProviderRequest, needle: &str| {
        req.messages
            .iter()
            .any(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
    };

    // Step 1: resume load hid the superseded history, summary present.
    assert!(
        !contains(&requests[0], "old answer"),
        "step 1 must hide history superseded before resume: {:?}",
        requests[0].messages,
    );
    assert!(contains(&requests[0], "seeded summary"));

    // Step 2: marks from the one-time load still hold...
    assert!(
        !contains(&requests[1], "old answer"),
        "step 2 must keep the resume-loaded marks: {:?}",
        requests[1].messages,
    );
    // ...and the raw between-steps compaction was NOT re-walked in:
    // its replaced event is still visible.
    assert!(
        contains(&requests[1], "first"),
        "a per-step store re-walk crept back in — step 2 hid an event \
         that only apply_persisted_marks could have marked: {:?}",
        requests[1].messages,
    );
    Ok(())
}

/// Seam I2-1, mid-session half: a compaction that fires *during* a
/// step marks supersession at commit time on the loop's own tracker,
/// so the following step of the same loop context still sees the
/// compacted view — with no per-step re-walk to fall back on.
#[tokio::test]
async fn mid_session_compaction_marks_survive_into_the_next_step() -> TestResult {
    let store = EventStore::new();
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

    // Step 1 fires auto-compaction (summarization call + main call);
    // step 2 runs with generous limits so no second trigger fires.
    let provider = MockProvider::new(vec![
        vec![
            text_delta("LLM summary of the seed turns"),
            done_event(StopReason::EndTurn),
        ],
        vec![text_delta("done"), done_event(StopReason::EndTurn)],
        vec![text_delta("later"), done_event(StopReason::EndTurn)],
    ]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    loop_ctx.token_estimator = Some(std::sync::Arc::new(crate::r#loop::SimpleTokenEstimator));

    let compacting_config = AgentLoopConfig {
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
            config: &compacting_config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let relaxed_config = AgentLoopConfig {
        context_window_limit: Some(1_000_000),
        auto_compact_reserve_tokens: Some(10_000),
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
            config: &relaxed_config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 3, "summarization + step 1 + step 2");
    let step_two = &requests[2];
    assert!(
        !step_two.messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains("seed question 0"))
        }),
        "the step-1 compaction's marks must persist into step 2 without \
         any store re-walk: {:?}",
        step_two.messages,
    );
    assert!(
        step_two.messages.iter().any(|m| {
            matches!(m.role, MessageRole::Developer)
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("LLM summary of the seed turns"))
        }),
        "the compaction summary must ride into step 2",
    );
    Ok(())
}

/// Resume with a compaction summary in history while dynamic context
/// appears mid-step (environment section): pre-fix, the `(Some, Some)`
/// arm overwrote the summary with the dynamic context. Post-fix the
/// dynamic context gets its own message and the summary survives.
#[tokio::test]
async fn dynamic_context_does_not_overwrite_history_summary() -> TestResult {
    let store = EventStore::new();
    store.append(SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "older history summary".to_string(),
        replaced_event_ids: Vec::new(),
    })?;

    let provider = MockProvider::new(vec![vec![
        text_delta("hi"),
        done_event(StopReason::EndTurn),
    ]]);
    let executor = MockToolExecutor::empty();

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.context_edits = Some(crate::session::context_edit::ContextEdits::new());
    // Environment sections are injected at the top of each iteration,
    // i.e. AFTER the initial prompt was built without dynamic context —
    // exactly the resume shape that triggered the overwrite.
    loop_ctx.environment = Some(crate::system_prompt::environment::EnvironmentConfig {
        session_id: Some("sess-h2".to_owned()),
        model: "test-model".to_owned(),
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
    let developer_contents: Vec<&str> = requests[0]
        .messages
        .iter()
        .filter(|m| matches!(m.role, MessageRole::Developer))
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert!(
        developer_contents
            .iter()
            .any(|c| c.contains("older history summary")),
        "history summary must survive: {developer_contents:?}",
    );
    assert!(
        developer_contents
            .iter()
            .any(|c| c.contains("# Environment")),
        "dynamic context must be present in its own message: {developer_contents:?}",
    );
    assert!(
        !developer_contents
            .iter()
            .any(|c| c.contains("older history summary") && c.contains("# Environment")),
        "summary and dynamic context must be separate messages: {developer_contents:?}",
    );
    Ok(())
}
