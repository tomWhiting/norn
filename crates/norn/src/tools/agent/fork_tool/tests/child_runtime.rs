use super::*;

/// R3: tools dispatched inside the fork see the *child's* `agent_id`,
/// not the parent's. The fork's registry path nests under the parent's.
#[tokio::test]
async fn forked_child_has_correct_identity_and_hierarchical_path() -> TestResult {
    let turn1 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc1".to_string(),
            call_id: None,
            name: Some("identity".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let turn2 = vec![
        ProviderEvent::ToolCallDelta {
            item_id: "structured-out".to_string(),
            call_id: None,
            name: Some("structured_output".to_string()),
            arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ];
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![turn1, turn2]));

    let seen_agent = Arc::new(StdMutex::new(None));
    let seen_parent = Arc::new(StdMutex::new(None));
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(IdentityStubTool {
        seen_agent: Arc::clone(&seen_agent),
        seen_parent: Arc::clone(&seen_parent),
    }));
    let tool_registry = Arc::new(tool_registry);

    let agent_registry = AgentRegistry::shared();
    let parent_guard = AgentRegistry::reserve(
        &agent_registry,
        "/parent".to_string(),
        "parent".to_string(),
        "opus".to_string(),
        None,
        test_envelope().child_policy,
        None,
    )?;
    let real_parent = parent_guard.id();
    parent_guard.confirm()?;

    let (ctx, _parent_store) = parent_ctx(
        provider,
        real_parent,
        &agent_registry,
        tool_registry,
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "introspect", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let entry = required(agent_registry.read().get(fork_id), "fork registry entry")?;
    assert!(
        entry.path.starts_with("/parent/fork/"),
        "fork path must nest under parent path: {}",
        entry.path,
    );

    let handle = remove_fork_handle(&ctx, fork_id)?;
    handle.join_handle.await?;

    assert_eq!(
        *seen_agent.lock(),
        Some(fork_id),
        "child tool must observe the fork's own agent_id",
    );
    assert_eq!(
        *seen_parent.lock(),
        Some(real_parent),
        "child tool must observe the parent as its parent_id",
    );
    Ok(())
}

/// Defect 1 regression (critical): a forked child must be able to load a
/// skill end-to-end. Previously `build_fork_context` never forwarded
/// `SkillSearchPaths`/`SkillCatalog`, so the fork saw the `skill` tool
/// but every call failed `MissingExtension`. Here the fork calls `skill`
/// (then produces its structured output) and its store must carry a
/// successful `skill` tool result containing the skill body.
#[tokio::test]
async fn forked_child_loads_a_skill_end_to_end() -> TestResult {
    let dir = tempfile::tempdir()?;
    let skill_dir = dir.path().join("greet");
    std::fs::create_dir(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\ndescription: greet the user\n---\nHELLO_FROM_GREET",
    )?;
    let paths = vec![dir.path().to_path_buf()];
    let catalog = Arc::new(crate::skill::SkillCatalog::scan(&paths));

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-skill".to_string(),
                call_id: None,
                name: Some("skill".to_string()),
                arguments_delta: json!({"name": "greet"}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }],
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(crate::tools::skill::SkillTool::with_config(
        crate::tools::skill::SkillToolConfig {
            shell_execution: false,
        },
    )));
    let agent_registry = AgentRegistry::shared();
    let (ctx, _parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(crate::tools::skill::SkillSearchPaths(paths)));
    ctx.insert_extension(catalog);

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"request": "greet", "model": "gpt-5.5", "requirements": []})),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let child_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    let loaded = child_store.events().iter().any(|e| {
        matches!(
            e,
            SessionEvent::ToolResult { tool_name, output, .. }
                if tool_name == "skill" && output.to_string().contains("HELLO_FROM_GREET")
        )
    });
    assert!(
        loaded,
        "forked child must load the skill successfully (extensions forwarded): {:?}",
        child_store.events(),
    );
    Ok(())
}

/// N-026 R6 (fork path): the fork's own tool context carries a
/// `ScheduleHandle`, proven behaviorally — the fork calls the `cron`
/// tool mid-run and the `schedule.created` event lands on the FORK's
/// event store (never the parent's: a fork's schedules are its own).
#[tokio::test]
async fn forked_child_resolves_cron_tool_against_its_own_schedule_handle() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "tc-cron".to_string(),
                call_id: None,
                name: Some("cron".to_string()),
                arguments_delta:
                    json!({"op": "schedule", "every": "2h", "message": "fork check-in"}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: json!({"response": "done", "requirements": {}}).to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        vec![ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }],
    ]));

    let mut registry = ToolRegistry::new();
    crate::tools::registry_builder::register_cron_tool(&mut registry);
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::new(MessageRouter::new()),
    );

    let tool = ForkTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({
                "request": "schedule a check-in", "model": "gpt-5.5", "requirements": [],
            })),
            &ctx,
        )
        .await?;
    let fork_id = fork_id_from(&out)?;
    let handle = remove_fork_handle(&ctx, fork_id)?;
    let child_store = Arc::clone(&handle.event_store);
    handle.join_handle.await?;

    let created = |store: &EventStore| {
        store.events().into_iter().any(|e| {
            matches!(
                &e,
                SessionEvent::Custom { event_type, .. }
                    if event_type == crate::schedule::SCHEDULE_CREATED_EVENT_TYPE
            )
        })
    };
    assert!(
        created(&child_store),
        "the fork's cron call must persist schedule.created to the fork's own store",
    );
    assert!(
        !created(&parent_store),
        "the fork's schedule must never leak onto the parent's store",
    );
    Ok(())
}

/// R4: `ForkComplete` event appended to parent's timeline with a
/// round-trippable variant tag.
#[tokio::test]
async fn fork_complete_event_appended_to_parent_store() -> TestResult {
    let provider = structured_response_provider(&json!({"response": "done", "requirements": {}}));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, parent_store) = parent_ctx(
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

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

    let events = parent_store.events();
    let complete = events.iter().rev().find_map(|e| match e {
        SessionEvent::ForkComplete {
            forked_session_id,
            result_summary,
            ..
        } => Some((forked_session_id.clone(), result_summary.clone())),
        _ => None,
    });
    let (fsid, summary) = required(complete, "ForkComplete event must be present")?;
    assert_eq!(summary["response"], "done");
    // F9: this parent is EPHEMERAL (parent_ctx arms ephemeral_root),
    // so the fork has no session file and the completion reference
    // records honest absence — never a registry-id stand-in.
    assert!(
        fsid.is_none(),
        "an ephemeral fork's ForkComplete must carry forked_session_id: None, got {fsid:?}",
    );
    // The honest `session: None` reservation is on the parent's
    // (in-memory) timeline too — the ONLY trace an ephemeral child's
    // name allocation leaves.
    let reservation = events.iter().find_map(|e| match e {
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
    let reservation = required(
        reservation,
        "the ephemeral parent's store must carry the ChildBranch reservation",
    )?;
    assert_eq!(
        reservation.0, None,
        "an ephemeral parent has no session id — honest None",
    );
    assert_eq!(
        reservation.1, None,
        "an ephemeral fork has no session id — honest None, never a fake id",
    );
    assert!(reservation.2.starts_with("root/fork-"), "{}", reservation.2);

    let event = SessionEvent::ForkComplete {
        base: EventBase::new(None),
        forked_session_id: fsid.clone(),
        result_summary: summary,
        usage: EventUsage::default(),
        duration_ms: 0,
    };
    let json_s = serde_json::to_string(&event)?;
    let parsed: SessionEvent = serde_json::from_str(&json_s)?;
    match parsed {
        SessionEvent::ForkComplete {
            forked_session_id, ..
        } => {
            assert_eq!(forked_session_id, fsid);
        }
        other => return Err(test_error(format!("expected ForkComplete, got {other:?}"))),
    }
    Ok(())
}
