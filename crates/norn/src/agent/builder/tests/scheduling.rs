use super::*;

/// A zero-tool agent (empty allow-list) is a supported configuration —
/// a pure text-transform step. Build must succeed, the gated registry
/// must expose no tools, and the assembled system prompt must omit the
/// `# Tools` section entirely. Regression for the former ≥1-tool build
/// rejection (owner decision 2026-07-02).
#[test]
fn zero_tool_agent_builds_for_transform_only_use() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .allowed_tools(&[])
        .build()
        .expect("a zero-tool agent must build");
    assert_eq!(
        agent.registry.names().count(),
        0,
        "empty allow-list must gate out every tool",
    );
    let parts = agent.into_parts();
    let prompt = parts
        .loop_context
        .system_sections
        .first()
        .expect("system prompt section assembled");
    assert!(
        !prompt.contains("# Tools"),
        "zero-tool system prompt must omit the # Tools section, got:\n{prompt}",
    );
}

/// N-026 R6 (root path): `build` registers the `cron` tool, installs
/// the `ScheduleHandle` extension on the shared tool context, and binds
/// the live executor's guard to the agent's loop context — without any
/// tool call having run.
#[tokio::test]
async fn build_registers_cron_tool_and_arms_schedule_executor() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    assert!(
        agent.registry.get("cron").is_some(),
        "the builder path registers the cron tool",
    );
    let handle = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::schedule::ScheduleHandle>()
        .expect("ScheduleHandle installed at assembly");
    assert_eq!(handle.agent_id, agent.id, "the handle carries the root id");
    assert!(handle.store.is_empty(), "a fresh session arms empty");
    assert!(
        agent.loop_context.schedule_executor.is_some(),
        "the executor guard rides on the loop context",
    );
}

/// N-026: `.without_tools(["cron"])` removes the scheduling tool like
/// any other exclusion — the arming stays (harmless), the surface gates.
#[tokio::test]
async fn without_tools_removes_cron_from_the_surface() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .without_tools(&["cron"])
        .build()
        .expect("build succeeds");
    assert!(agent.registry.get("cron").is_none());
}

/// N-026 R5/R6: a resumed root rebuilds its schedule store from the
/// session's `schedule.created` events and arms the executor from it —
/// no tool call involved. The recurring survivor re-arms from resume
/// time (a single next fire within its interval, no backfill).
#[tokio::test]
async fn resumed_root_arms_executor_from_rebuilt_store() {
    let session = EventStore::new();
    let record = crate::schedule::ScheduleRecord::new(
        uuid::Uuid::new_v4(),
        crate::schedule::ScheduleSpec::Every {
            duration: std::time::Duration::from_hours(1),
        },
        "hourly triage".to_string(),
        uuid::Uuid::new_v4(),
        chrono::Utc::now() - chrono::TimeDelta::hours(5),
    )
    .expect("record");
    let id = record.id;
    crate::schedule::append_schedule_event(
        &session,
        &crate::schedule::ScheduleLifecycle::Created { record },
    )
    .expect("persist created");

    let before = chrono::Utc::now();
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .session(Arc::new(session))
        .build()
        .expect("resume build succeeds");
    let handle = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::schedule::ScheduleHandle>()
        .expect("ScheduleHandle installed");
    let restored = handle.store.get(id).expect("pending schedule restored");
    assert!(!restored.late, "recurring schedules never fire late");
    assert!(
        restored.next_fire > before
            && restored.next_fire
                <= before + chrono::TimeDelta::hours(1) + chrono::TimeDelta::seconds(5),
        "one natural next fire within the hour, no backfill: {}",
        restored.next_fire,
    );
}

/// N-026 R6: dropping the agent instance aborts the schedule executor —
/// a short `Every` schedule stops firing after the drop (virtual-time
/// advance well past several intervals, `next_fire` frozen).
#[tokio::test(start_paused = true)]
async fn dropping_agent_aborts_schedule_executor() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    let handle = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::schedule::ScheduleHandle>()
        .expect("ScheduleHandle installed");
    let record = crate::schedule::ScheduleRecord::new(
        uuid::Uuid::new_v4(),
        crate::schedule::ScheduleSpec::Every {
            duration: std::time::Duration::from_secs(1),
        },
        "tick".to_string(),
        agent.id,
        chrono::Utc::now(),
    )
    .expect("record");
    let id = record.id;
    let armed_fire = record.next_fire;
    handle.store.insert(record);

    drop(agent);
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    let frozen = handle.store.get(id).expect("record still pending");
    assert_eq!(
        frozen.next_fire, armed_fire,
        "no fire after the agent dropped — the executor died with it",
    );
    assert!(
        !handle.event_store.events().iter().any(|e| matches!(
            e,
            crate::session::events::SessionEvent::Custom { event_type, .. }
                if event_type == crate::schedule::SCHEDULE_FIRED_EVENT_TYPE
        )),
        "no schedule.fired persisted after the drop",
    );
}
