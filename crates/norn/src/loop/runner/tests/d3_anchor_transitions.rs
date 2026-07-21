use super::*;
use crate::agent::ContextFilter;
use crate::r#loop::conversation_state::ConversationRequestState;
use crate::r#loop::helpers::build_initial_messages;
use crate::provider::request::ProviderRequest;
use crate::session::context_edit::ContextEdits;
use crate::session::{
    ProviderFilteredForkBoundary, ProviderStateProvenance, committed_response_publication,
    response_publication_fixture,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy)]
enum DurableCut {
    Compaction,
    Suppression,
}

fn assistant(base: EventBase, content: &str, response_id: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base,
        content: content.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    }
}

fn stored_assistant_events(
    content: &str,
    response_id: &str,
) -> Result<Vec<SessionEvent>, Box<dyn std::error::Error>> {
    let fixture = response_publication_fixture(None, true)?;
    Ok(committed_response_publication(
        fixture.boundary,
        fixture.provenance,
        assistant(fixture.assistant_base, content, response_id),
    )?)
}

fn append_stored_assistant(
    store: &EventStore,
    content: &str,
    response_id: &str,
) -> Result<crate::session::events::EventId, Box<dyn std::error::Error>> {
    let fixture = response_publication_fixture(store.last_event_id(), true)?;
    let assistant_id = fixture.assistant_base.id.clone();
    let publication = committed_response_publication(
        fixture.boundary,
        fixture.provenance,
        assistant(fixture.assistant_base, content, response_id),
    )?;
    store.append_batch(&publication)?;
    Ok(assistant_id)
}

fn apply_cut(
    store: &EventStore,
    cut: DurableCut,
    target: crate::session::events::EventId,
) -> Result<(), crate::error::SessionError> {
    let mut edits = ContextEdits::new();
    match cut {
        DurableCut::Compaction => {
            edits.summarize(store, vec![target], "durable summary".to_owned())?;
        }
        DurableCut::Suppression => {
            edits.suppress(store, target)?;
        }
    }
    Ok(())
}

fn resumed_threaded_request(store: &EventStore) -> Result<ProviderRequest, NornError> {
    let loop_context = LoopContext::new("system");
    let initial = build_initial_messages(Some("next prompt"), &loop_context, store)?;
    let config = AgentLoopConfig {
        conversation_state: crate::r#loop::config::ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };
    let state = ConversationRequestState::new(
        &config,
        ProviderCapabilities::openai_responses(),
        initial.prefix_len,
        initial.response_thread_anchor,
    )?;

    Ok(ProviderRequest {
        messages: state.request_messages(&initial.messages),
        tools: Vec::new(),
        model: "gpt-test".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: state.previous_response_id(),
        store: state.store(),
        context_management: state.context_management(&config),
    })
}

fn message_contents(request: &ProviderRequest) -> Vec<&str> {
    request
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect()
}

#[test]
fn tracker_free_resume_clears_anchors_at_durable_compaction_and_suppression_cuts() -> TestResult {
    for cut in [DurableCut::Compaction, DurableCut::Suppression] {
        let store = EventStore::new();
        let old_id = append_stored_assistant(&store, "old answer", "resp_old")?;
        apply_cut(&store, cut, old_id)?;

        let request = resumed_threaded_request(&store)?;

        assert!(request.store);
        assert!(request.previous_response_id.is_none());
        assert!(message_contents(&request).contains(&"next prompt"));
        assert!(
            !message_contents(&request).contains(&"old answer"),
            "the tracker-free prompt projection must apply the durable cut",
        );
        let has_persisted_summary = request.messages.iter().any(|message| {
            message.role == MessageRole::Developer
                && message.content.as_deref()
                    == Some("Prior conversation compaction summary:\ndurable summary")
        });
        assert_eq!(
            has_persisted_summary,
            matches!(cut, DurableCut::Compaction),
            "a persisted compaction summary remains an ordinary Developer input item",
        );
    }
    Ok(())
}

#[test]
fn durable_cut_excludes_pre_cut_unmarked_legacy_witnesses() -> TestResult {
    for cut in [DurableCut::Compaction, DurableCut::Suppression] {
        let store = EventStore::new();
        let legacy_id = store.append(assistant(
            EventBase::new(None),
            "legacy witness",
            "resp_legacy_witness",
        ))?;
        apply_cut(&store, cut, legacy_id)?;

        let initial =
            build_initial_messages(Some("next prompt"), &LoopContext::new("system"), &store)?;
        assert!(
            initial.legacy_response_thread_anchors.is_empty(),
            "a durable cut must close legacy-anchor eligibility",
        );
    }
    Ok(())
}

#[test]
fn provenance_era_does_not_reopen_legacy_fallback_after_a_cut() -> TestResult {
    for cut in [DurableCut::Compaction, DurableCut::Suppression] {
        let store = EventStore::new();
        let old_id = append_stored_assistant(&store, "stored answer", "resp_stored")?;
        apply_cut(&store, cut, old_id)?;
        store.append(assistant(
            EventBase::new(store.last_event_id()),
            "unmarked post-cut answer",
            "resp_unmarked_post_cut",
        ))?;

        let initial =
            build_initial_messages(Some("next prompt"), &LoopContext::new("system"), &store)?;
        assert!(
            initial.legacy_response_thread_anchors.is_empty(),
            "a session that entered the provenance era must not treat a post-cut unmarked ID as legacy",
        );
    }

    let store = EventStore::new();
    append_stored_assistant(&store, "stored answer", "resp_stored")?;
    store.append(SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(store.last_event_id()),
        reason: crate::session::events::ProviderEpochBoundaryReason::ProviderIdentityAdoption,
    })?;
    store.append(assistant(
        EventBase::new(store.last_event_id()),
        "unmarked post-boundary answer",
        "resp_unmarked_post_boundary",
    ))?;
    let initial = build_initial_messages(Some("next prompt"), &LoopContext::new("system"), &store)?;
    assert!(initial.legacy_response_thread_anchors.is_empty());

    let store = EventStore::new();
    store.append(ProviderFilteredForkBoundary::into_event(EventBase::new(
        store.last_event_id(),
    )))?;
    store.append(assistant(
        EventBase::new(store.last_event_id()),
        "unmarked filtered-child answer",
        "resp_unmarked_filtered_child",
    ))?;
    let initial = build_initial_messages(Some("next prompt"), &LoopContext::new("system"), &store)?;
    assert!(
        initial.legacy_response_thread_anchors.is_empty(),
        "a D3 filtered-fork boundary must close legacy eligibility in a fresh child",
    );
    Ok(())
}

#[test]
fn tracker_free_resume_keeps_anchor_across_a_durable_injection() -> TestResult {
    let store = EventStore::new();
    append_stored_assistant(&store, "old answer", "resp_old")?;
    let mut edits = ContextEdits::new();
    edits.inject(
        &store,
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "injected operator context".to_owned(),
        },
    )?;

    let request = resumed_threaded_request(&store)?;

    assert_eq!(request.previous_response_id.as_deref(), Some("resp_old"));
    assert_eq!(
        message_contents(&request),
        vec!["system", "injected operator context", "next prompt"],
        "injection is new provider input, not an epoch cut",
    );
    Ok(())
}

#[test]
fn tracker_free_resume_uses_the_first_post_cut_response_as_its_new_anchor() -> TestResult {
    for cut in [DurableCut::Compaction, DurableCut::Suppression] {
        let store = EventStore::new();
        let old_id = append_stored_assistant(&store, "old answer", "resp_old")?;
        apply_cut(&store, cut, old_id)?;
        append_stored_assistant(&store, "new answer", "resp_new")?;

        let request = resumed_threaded_request(&store)?;

        assert_eq!(request.previous_response_id.as_deref(), Some("resp_new"));
        assert_eq!(
            message_contents(&request),
            vec!["system", "next prompt"],
            "only input after the new response anchor should be sent",
        );
    }
    Ok(())
}

#[test]
fn nonidentity_context_filter_starts_a_fresh_provider_epoch() -> TestResult {
    let parent_anchor = stored_assistant_events("parent answer", "resp_parent")?;
    let filtered = ContextFilter {
        include_system: true,
        include_recent_n: Some(16),
        exclude_tool_calls: false,
    }
    .apply(&parent_anchor)?;
    let store = EventStore::new();
    for event in filtered {
        store.append(event)?;
    }

    let request = resumed_threaded_request(&store)?;

    assert!(request.previous_response_id.is_none());
    assert_eq!(
        message_contents(&request),
        vec!["system", "parent answer", "next prompt"],
        "the filtered audit row remains replayable but cannot inherit its provider anchor",
    );
    Ok(())
}

#[tokio::test]
async fn first_manual_replay_response_records_negative_provenance_and_is_not_adopted() -> TestResult
{
    let identity = crate::provider::ProviderStateIdentity::derive(
        "norn.runner.anchor-provenance",
        b"manual-to-threaded-fixture",
    );
    let first_provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("stateless answer"),
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: Some("resp_not_stored".to_owned()),
            },
        ]],
        ProviderCapabilities::openai_responses(),
    )
    .with_state_identity(identity);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let manual_config = AgentLoopConfig {
        conversation_state: crate::r#loop::config::ConversationStateMode::ManualReplay,
        ..AgentLoopConfig::default()
    };

    assert_completed(
        run_step(
            &first_provider,
            &executor,
            &store,
            &[],
            None,
            &manual_config,
            None,
        )
        .await,
    );
    let initial_events = store.events();
    let negative_provenance = initial_events
        .iter()
        .filter_map(|event| ProviderStateProvenance::from_event(event).ok().flatten())
        .collect::<Vec<_>>();
    assert_eq!(negative_provenance.len(), 1);
    assert!(
        !negative_provenance[0].stored(),
        "store:false response IDs must receive explicit negative provenance",
    );

    let second_provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("threaded answer"),
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: Some("resp_stored".to_owned()),
            },
        ]],
        ProviderCapabilities::openai_responses(),
    )
    .with_state_identity(identity);
    let threaded_config = AgentLoopConfig {
        conversation_state: crate::r#loop::config::ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };

    assert_completed(
        run_step(
            &second_provider,
            &executor,
            &store,
            &[],
            None,
            &threaded_config,
            None,
        )
        .await,
    );

    let requests = second_provider.requests()?;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].store);
    assert!(requests[0].previous_response_id.is_none());
    assert!(requests[0].messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.content.as_deref() == Some("stateless answer")
    }));

    let events = store.events();
    let Some((provenance_index, provenance_base, assistant_event_id)) =
        events.iter().enumerate().find_map(|(index, event)| {
            ProviderStateProvenance::from_event(event)
                .ok()
                .flatten()
                .filter(ProviderStateProvenance::stored)
                .map(|provenance| (index, event.base(), provenance.assistant_event_id().clone()))
        })
    else {
        return Err(
            std::io::Error::other("stored response did not publish positive provenance").into(),
        );
    };
    let Some((assistant_index, assistant_base)) =
        events
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                SessionEvent::AssistantMessage {
                    base,
                    response_id: Some(response_id),
                    ..
                } if base.id == assistant_event_id && response_id == "resp_stored" => {
                    Some((index, base))
                }
                _ => None,
            })
    else {
        return Err(std::io::Error::other("provenance target was not published").into());
    };
    assert_eq!(assistant_index, provenance_index.saturating_add(1));
    assert_eq!(assistant_base.parent_id.as_ref(), Some(&provenance_base.id));
    Ok(())
}
