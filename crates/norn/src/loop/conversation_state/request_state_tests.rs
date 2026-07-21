use super::test_support::{append_stored_assistant, config, message};
use super::*;
use crate::provider::request::{MessageRole, ToolCallKind};
use crate::session::events::{EventBase, EventUsage, ToolCallEvent};
use crate::session::response_publication_fixture;
use crate::session::store::EventStore;

#[test]
fn threaded_request_excludes_replay_only_developer_history()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    store.append(SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "older history".to_string(),
        replaced_event_ids: Vec::new(),
    })?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "old".to_string(),
    })?;
    append_stored_assistant(&store, "old answer", "resp_old")?;

    let state = ConversationRequestState::new(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        1,
        latest_response_anchor(&store.events(), 1, true)?,
    )?;
    let messages = vec![
        message(MessageRole::System, "system"),
        message(MessageRole::Developer, "old compaction summary"),
        message(MessageRole::User, "old"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::User, "new"),
    ];

    let request_messages = state.request_messages(&messages);

    assert_eq!(request_messages.len(), 2);
    assert_eq!(request_messages[0].content.as_deref(), Some("system"));
    assert_eq!(request_messages[1].content.as_deref(), Some("new"));
    Ok(())
}

#[test]
fn unsupported_threading_is_rejected() {
    assert!(matches!(
        ConversationRequestState::new(
            &config(ConversationStateMode::ProviderThreaded),
            ProviderCapabilities::default(),
            1,
            None,
        ),
        Err(ProviderError::UnsupportedFeature { .. })
    ));
}

#[test]
fn auto_mode_falls_back_when_provider_cannot_thread() -> Result<(), ProviderError> {
    let state = ConversationRequestState::new(
        &config(ConversationStateMode::Auto),
        ProviderCapabilities::default(),
        1,
        Some(ResponseThreadAnchor {
            response_id: "resp_old".to_string(),
            input_start: 1,
        }),
    )?;

    assert_eq!(state.previous_response_id(), None);
    assert!(!state.store());
    Ok(())
}

#[test]
fn auto_mode_threads_when_provider_supports_it() -> Result<(), ProviderError> {
    let state = ConversationRequestState::new(
        &config(ConversationStateMode::Auto),
        ProviderCapabilities::openai_responses(),
        1,
        Some(ResponseThreadAnchor {
            response_id: "resp_old".to_string(),
            input_start: 1,
        }),
    )?;

    assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
    assert!(state.store());
    Ok(())
}

#[test]
fn threaded_state_requires_an_opaque_provider_identity() -> Result<(), ProviderError> {
    let state = ConversationRequestState::new(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        1,
        None,
    )?;

    assert!(matches!(
        state.require_state_identity(None),
        Err(ProviderError::ProviderStateIdentityRequired)
    ));
    assert!(
        state
            .require_state_identity(Some(ProviderStateIdentity::derive(
                "norn.conversation-state-test",
                b"threaded-provider-fixture",
            )))
            .is_ok()
    );
    Ok(())
}

#[test]
fn threaded_anchor_starts_after_latest_visible_response() -> Result<(), Box<dyn std::error::Error>>
{
    let store = EventStore::new();
    append_stored_assistant(&store, "old answer", "resp_old")?;
    store.append(SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "call_old".to_string(),
        tool_name: "read".to_string(),
        output: serde_json::json!({"ok": true}),
        spool_ref: None,
        duration_ms: 1,
    })?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "queued user".to_string(),
    })?;

    let state = ConversationRequestState::new(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        1,
        latest_response_anchor(&store.events(), 1, false)?,
    )?;
    let messages = vec![
        message(MessageRole::System, "system"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::ToolResult, "tool result"),
        message(MessageRole::User, "queued user"),
        message(MessageRole::User, "new user"),
    ];

    let request_messages = state.request_messages(&messages);

    assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
    assert_eq!(request_messages.len(), 4);
    assert_eq!(request_messages[0].content.as_deref(), Some("system"));
    assert_eq!(request_messages[1].content.as_deref(), Some("tool result"));
    assert_eq!(request_messages[2].content.as_deref(), Some("queued user"));
    assert_eq!(request_messages[3].content.as_deref(), Some("new user"));
    Ok(())
}

#[test]
fn resume_repair_keeps_stored_anchor_for_first_healed_request()
-> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::new();
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "run it".to_string(),
    })?;
    let fixture = response_publication_fixture(store.last_event_id(), true)?;
    let assistant = SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: fixture.assistant_base,
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "call_killed".to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": "long-running"}),
            kind: ToolCallKind::Function,
            caller: crate::provider::request::ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_string(),
        response_id: Some("resp_killed".to_string()),
    };
    store.append_batch(&[fixture.boundary, fixture.provenance, assistant])?;
    crate::session::repair_dangling_tool_calls(&store)?;

    let state = ConversationRequestState::new(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        1,
        latest_response_anchor(&store.events(), 1, false)?,
    )?;
    let mut messages = vec![message(MessageRole::System, "system")];
    messages.extend(crate::session::conversion::events_to_messages(
        &store.events(),
    ));
    messages.push(message(MessageRole::User, "resume"));

    assert_eq!(state.previous_response_id().as_deref(), Some("resp_killed"));
    let request_messages = state.request_messages(&messages);
    assert_eq!(request_messages.len(), 3);
    assert_eq!(request_messages[0].content.as_deref(), Some("system"));
    assert_eq!(
        request_messages[1].tool_call_id.as_deref(),
        Some("call_killed"),
    );
    assert_eq!(request_messages[2].content.as_deref(), Some("resume"));
    Ok(())
}

#[test]
fn server_compaction_requires_threaded_state() {
    assert!(matches!(
        ConversationRequestState::new(
            &AgentLoopConfig {
                conversation_state: ConversationStateMode::ManualReplay,
                server_compaction_threshold_tokens: Some(100),
                ..AgentLoopConfig::default()
            },
            ProviderCapabilities::openai_responses(),
            1,
            None,
        ),
        Err(ProviderError::UnsupportedFeature { .. })
    ));
}

#[test]
fn public_threading_derives_server_compaction_threshold_from_existing_limits()
-> Result<(), ProviderError> {
    let config = AgentLoopConfig {
        context_window_limit: Some(200_000),
        auto_compact_reserve_tokens: Some(30_000),
        ..AgentLoopConfig::default()
    };
    let state =
        ConversationRequestState::new(&config, ProviderCapabilities::openai_responses(), 1, None)?;

    assert_eq!(
        state
            .context_management(&config)
            .map(|management| management.compact_threshold_tokens),
        Some(170_000),
    );
    Ok(())
}

#[test]
fn explicit_server_compaction_threshold_wins_over_derived_value() -> Result<(), ProviderError> {
    let config = AgentLoopConfig {
        context_window_limit: Some(200_000),
        auto_compact_reserve_tokens: Some(30_000),
        server_compaction_threshold_tokens: Some(125_000),
        ..AgentLoopConfig::default()
    };
    let state =
        ConversationRequestState::new(&config, ProviderCapabilities::openai_responses(), 1, None)?;

    assert_eq!(
        state
            .context_management(&config)
            .map(|management| management.compact_threshold_tokens),
        Some(125_000),
    );
    Ok(())
}

#[test]
fn invalid_derived_threshold_does_not_invent_a_fallback() -> Result<(), ProviderError> {
    for reserve in [100, 101] {
        let config = AgentLoopConfig {
            context_window_limit: Some(100),
            auto_compact_reserve_tokens: Some(reserve),
            ..AgentLoopConfig::default()
        };
        let state = ConversationRequestState::new(
            &config,
            ProviderCapabilities::openai_responses(),
            1,
            None,
        )?;
        assert!(state.context_management(&config).is_none());
    }
    Ok(())
}

#[test]
fn incomplete_compaction_limits_do_not_invent_a_threshold() -> Result<(), ProviderError> {
    for config in [
        AgentLoopConfig {
            context_window_limit: Some(200_000),
            auto_compact_reserve_tokens: None,
            ..AgentLoopConfig::default()
        },
        AgentLoopConfig {
            context_window_limit: None,
            auto_compact_reserve_tokens: Some(30_000),
            ..AgentLoopConfig::default()
        },
    ] {
        let state = ConversationRequestState::new(
            &config,
            ProviderCapabilities::openai_responses(),
            1,
            None,
        )?;
        assert!(state.context_management(&config).is_none());
    }
    Ok(())
}
