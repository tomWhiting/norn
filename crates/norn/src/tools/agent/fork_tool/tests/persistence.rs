use super::*;
use crate::agent::fork::ForkIdentity;
use crate::r#loop::loop_context::LoopContext;

/// V2-R2 (persistent parent): fork mints a real on-disk child timeline under
/// the root's `children/` directory and preserves its parent relationship.
#[tokio::test]
async fn fork_under_persistent_parent_persists_child_timeline() -> TestResult {
    use crate::session::manager::{CreateSessionOptions, SessionManager};
    use crate::session::persistence::io::read_session_events_for_entry;
    use crate::session::store::DurabilityPolicy;
    use crate::session::{SessionBinding, SessionBrancher};

    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager.create(
        CreateSessionOptions {
            model: "gpt-5.5".to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let root_id = opened.entry.id.clone();
    let root_entry = opened.entry.clone();
    let parent_store = Arc::new(opened.store);
    let inherited_items =
        historical_non_audio_items("fork_inherited", "Inherited canonical context.");
    let response_items = inherited_items
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| Ok(transcript_item(raw, u64::try_from(index)?)?))
        .collect::<TestResult<Vec<_>>>()?;
    parent_store.append(SessionEvent::AssistantMessage {
        response_items,
        base: EventBase::new(None),
        content: "stale inherited projection".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_fork_inherited".to_owned()),
    })?;
    let inherited_audio_reference = {
        let audio_store = parent_store
            .response_audio()
            .ok_or_else(|| std::io::Error::other("persistent parent audio store is missing"))?;
        let mut writer = audio_store.begin(1)?;
        let raw =
            crate::provider::openai::response_stream_event::ResponseStreamEvent::from_raw(json!({
                "type": "response.audio.delta",
                "sequence_number": 1,
                "delta": "aW5oZXJpdGVkIGZvcmsgYXVkaW8=",
            }))?;
        let event = crate::provider::response_audio::ResponseAudioEvent::from_stream_event(&raw)?
            .ok_or_else(|| std::io::Error::other("fork audio fixture was not audio"))?;
        writer.append(&raw, &event)?;
        writer.seal(Some("resp_fork_inherited_audio"))?
    };
    let audio_link_base = EventBase::new(parent_store.last_event_id());
    let audio_assistant_base = EventBase::new(Some(audio_link_base.id.clone()));
    let audio_link = crate::session::ResponseAudioArtifactLink::new(
        audio_assistant_base.id.clone(),
        inherited_audio_reference,
        Some("resp_fork_inherited_audio".to_owned()),
    )
    .into_custom_event(audio_link_base)?;
    parent_store.append(audio_link)?;
    parent_store.append(SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: audio_assistant_base,
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_fork_inherited_audio".to_owned()),
    })?;
    parent_store.checkpoint()?;
    let binding = Arc::new(SessionBinding::persistent_root(
        Arc::new(SessionBrancher::new(
            manager.clone(),
            root_id.clone(),
            DurabilityPolicy::Flush,
        )),
        &root_entry,
        &[],
    ));

    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(&agent_registry),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::clone(&parent_store),
        agent_id: parent,
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::new(ToolRegistry::new())),
        session: binding,
    });
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);
    ctx.insert_extension(Arc::new(AgentHandles::new()));
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
    ctx.insert_extension(Arc::new(test_envelope()));

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "noop", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    // Index row: the fork's session is manifest-discoverable with
    // rel_path + parent linkage.
    let row = manager.resolve(&fork_id.to_string())?;
    let rel = required(row.rel_path.as_deref(), "child rows must carry rel_path")?;
    assert!(
        rel.starts_with(&format!("{root_id}/children/fork-"))
            && std::path::Path::new(rel)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")),
        "child file must live under the root's children/ dir: {rel}",
    );
    assert_eq!(row.parent_id.as_deref(), Some(root_id.as_str()));

    // The child timeline is REAL on-disk write-through: its file exists
    // and replays the seeded history (provenance header + seed copy +
    // the fork's own run events).
    let child_file = tmp.path().join(rel);
    assert!(child_file.exists(), "fork timeline file must exist on disk");
    let child_read = read_session_events_for_entry(tmp.path(), &row)?;
    assert!(
        child_read
            .events
            .iter()
            .any(|e| matches!(e, SessionEvent::ChildBranch { .. })),
        "the child's file carries its ChildBranch provenance header",
    );
    assert!(
        child_read
            .events
            .iter()
            .any(|e| matches!(e, SessionEvent::AssistantMessage { .. })),
        "the fork's own run events reach its on-disk timeline",
    );
    assert_eq!(
        canonical_item_values(&child_read.events),
        inherited_items,
        "the real fork seed must copy canonical items exactly and in order",
    );

    // Parent side, ON DISK: ChildBranch reservation (parent-first) and
    // the honest ForkComplete reference.
    let parent_entry = manager.resolve(&root_id)?;
    let parent_read = read_session_events_for_entry(tmp.path(), &parent_entry)?;
    let branch = parent_read.events.iter().find_map(|e| match e {
        SessionEvent::ChildBranch {
            parent_session_id,
            child_session_id,
            path_address,
            ..
        } => Some((
            parent_session_id.clone(),
            child_session_id.clone(),
            path_address.clone(),
        )),
        _ => None,
    });
    let branch = required(branch, "parent file must carry the ChildBranch reservation")?;
    assert_eq!(branch.0.as_deref(), Some(root_id.as_str()));
    let fork_id_text = fork_id.to_string();
    assert_eq!(branch.1.as_deref(), Some(fork_id_text.as_str()));
    assert!(branch.2.starts_with("root/fork-"));
    let fork_complete = parent_read.events.iter().find_map(|e| match e {
        SessionEvent::ForkComplete {
            forked_session_id, ..
        } => Some(forked_session_id.clone()),
        _ => None,
    });
    let fork_complete = required(fork_complete, "parent file must carry ForkComplete")?;
    assert_eq!(
        fork_complete.as_deref(),
        Some(fork_id_text.as_str()),
        "ForkComplete must reference the real child session, never a stand-in",
    );

    // And the child resumes through the manager like any session.
    let resumed = manager.resume(&fork_id_text, DurabilityPolicy::Flush)?;
    assert!(resumed.replay.replayed_events > 0);
    let resumed_events = resumed.store.events();
    assert_eq!(
        canonical_item_values(&resumed_events),
        inherited_items,
        "SessionManager::resume must retain the fork's inherited canonical history",
    );
    let inherited_audio_link = crate::session::response_audio_artifact_links(&resumed_events)?
        .into_iter()
        .find(|link| link.reference() == inherited_audio_reference)
        .ok_or_else(|| std::io::Error::other("fork did not inherit its parent audio link"))?;
    let inherited_audio = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("resumed fork audio store is missing"))?
        .read_linked(&inherited_audio_link)?;
    assert_eq!(inherited_audio.audio, b"inherited fork audio");
    assert_eq!(inherited_audio.owner_session_id, root_id);
    assert_eq!(inherited_audio.owner_generation, root_entry.generation);
    let replay_input = stateless_payload_input(&resumed_events)
        .map_err(|error| test_error(format!("failed to build replay payload: {error}")))?;
    assert!(
        replay_input
            .windows(inherited_items.len())
            .any(|window| window == inherited_items),
        "the resumed fork must replay its exact inherited public and opaque corpus",
    );
    let expected_opaque = inherited_items
        .last()
        .ok_or_else(|| std::io::Error::other("inherited corpus unexpectedly empty"))?
        .clone();
    let replayed_opaque = replay_input
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("future_historical_item")
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        replayed_opaque,
        vec![expected_opaque],
        "the resumed fork must replay the inherited opaque item exactly once",
    );
    let mut expected_replay = inherited_items
        .into_iter()
        .filter(|item| {
            item.get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|item_type| {
                    crate::provider::openai::response_contract::public_output_item(item_type)
                        .is_some()
                })
        })
        .collect::<Vec<_>>();
    expected_replay.extend([
        json!({
            "type": "function_call",
            "call_id": "structured-out",
            "name": "structured_output",
            "arguments": "{\"response\":\"done\",\"requirements\":{}}"
        }),
        json!({
            "type": "function_call_output",
            "call_id": "structured-out",
            "output": "accepted"
        }),
    ]);
    assert_eq!(
        canonical_payload_items(&replay_input),
        expected_replay,
        "the resumed fork must replay the exact public-item corpus followed only by its own structured result",
    );
    Ok(())
}

/// The legacy string helper retains compiled parent policy without
/// admitting requirement content into the System preamble.
#[test]
fn legacy_flattened_helper_places_fork_policy_before_parent_base() {
    let parent_base = "You are the parent. Be terse.";
    let policy = test_envelope().child_policy;
    let preamble = build_fork_preamble(&ForkIdentity {
        parent_agent_id: "parent-agent-id",
        path_address: "root/fork-a",
        granted: &policy,
    });
    let combined = combine_system_instruction(&preamble, parent_base);
    let loop_ctx = LoopContext::new(combined);
    let base = loop_ctx.base_system_instruction();
    assert!(
        base.contains(FORK_SYSTEM_PREAMBLE),
        "preamble missing: {base}"
    );
    assert!(
        base.contains("root/fork-a"),
        "structured identity missing: {base}",
    );
    assert!(!base.contains("check_code"));
    assert!(base.contains(parent_base), "parent missing: {base}");
    assert!(
        base.find(FORK_SYSTEM_PREAMBLE) < base.find(parent_base),
        "preamble must precede parent base: {base}",
    );
}
