use super::*;

/// The builder owns the event channel end to end: the raw broadcast
/// channel must be published on the tool context as
/// `SharedAgentEventChannel` so fork/spawn children stream their
/// events through the same channel the embedder subscribes to.
#[test]
fn event_channel_is_published_for_subagents() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .event_channel_capacity(16)
        .build()
        .expect("build succeeds");
    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let shared_channel = ctx
        .get_extension::<SharedAgentEventChannel>()
        .expect("SharedAgentEventChannel must be installed for child streaming");
    let mut handle_rx = agent.handle().subscribe().expect("subscribe");
    shared_channel
        .0
        .send(crate::provider::AgentEvent {
            agent_id: Uuid::nil(),
            agent_role: std::sync::Arc::from("spawn/test"),
            event: crate::provider::AgentEventKind::Provider(ProviderEvent::TextDelta {
                text: "child delta".to_string(),
            }),
        })
        .expect("handle subscription keeps the channel open");
    let received = handle_rx.try_recv().expect("event arrives");
    assert_eq!(&*received.agent_role, "spawn/test");
}

/// The inbound sender is reachable both mid-chain (for infrastructure
/// built before the agent) and on the handle, and both feed the same
/// channel the loop drains.
#[test]
fn inbound_sender_available_pre_build_and_on_handle() {
    let builder = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .inbound_capacity(8);
    let pre_build = builder
        .inbound_sender()
        .expect("sender available as soon as the capacity is set");
    let agent = builder.build().expect("build succeeds");
    let handle_sender = agent
        .handle()
        .inbound_sender()
        .expect("sender available on the handle");
    // Both senders feed the channel whose receiver the agent holds.
    assert!(agent.inbound.is_some(), "loop receives the inbound half");
    drop((pre_build, handle_sender));
}

#[test]
fn handle_exposes_resolved_introspection() -> Result<(), Box<dyn std::error::Error>> {
    let schema = serde_json::json!({"type": "object", "required": ["answer"]});
    let temp = tempfile::tempdir()?;
    let id = Uuid::new_v4();
    let agent = AgentBuilder::new(provider_with(vec![]))
        .profile(Profile {
            name: "reviewer".to_owned(),
            model: "profile-model".to_owned(),
            ..Profile::default()
        })
        .model("resolved-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .agent_id(id)
        .allowed_tools(&["read", "search"])
        .output_schema(schema.clone())
        .build()?;

    let info = agent.handle().info().clone();
    assert_eq!(info.agent_id, id);
    assert_eq!(info.model, "resolved-model", "model override wins");
    assert_eq!(info.profile_name.as_deref(), Some("reviewer"));
    assert_eq!(info.working_dir, temp.path().canonicalize()?);
    assert_eq!(info.output_schema.as_ref(), Some(&schema));
    assert!(!info.session_id.is_empty(), "session id always resolved");
    let mut tools = info.tool_names.clone();
    tools.sort();
    assert_eq!(tools, vec!["read".to_owned(), "search".to_owned()]);
    // The snapshot is serializable for activity records / telemetry.
    let json = serde_json::to_value(&info)?;
    assert_eq!(json["model"], "resolved-model");
    assert_eq!(json["output_schema"], schema);
    // Agent-side accessors agree with the handle.
    assert_eq!(agent.info().model, info.model);
    assert_eq!(agent.agent_id(), id);
    Ok(())
}

#[test]
fn default_profile_yields_no_profile_name() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    assert_eq!(agent.info().profile_name, None);
}

/// Cancellation through the handle: no caller-supplied token needed —
/// the builder mints one and the handle controls it.
#[tokio::test]
async fn handle_cancel_stops_run_with_cancelled_reason() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    let handle = agent.handle();
    assert!(!handle.cancellation_token().is_cancelled());
    handle.cancel();
    let outcome = agent
        .run("go")
        .await
        .expect("cancelled run returns Ok(Stopped)");
    assert_eq!(outcome.stop_reason(), Some(&AgentStopReason::Cancelled));
}

#[test]
fn custom_tool_is_added_alongside_defaults() {
    use crate::error::ToolError;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::scheduling::ToolEffect;
    use crate::tool::traits::ToolOutput;

    struct CustomTool;
    #[async_trait::async_trait]
    impl Tool for CustomTool {
        fn name(&self) -> &'static str {
            "custom_probe"
        }
        fn description(&self) -> &'static str {
            "a custom probe tool"
        }
        fn input_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            _envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(Value::Null))
        }
    }

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .tool(Box::new(CustomTool))
        .build()
        .expect("build succeeds");
    assert!(agent.registry.get("custom_probe").is_some());
    assert!(
        agent.registry.get("read").is_some(),
        "defaults still present"
    );
}

#[test]
fn default_retry_policy_is_two_one_second_two_x() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    assert_eq!(agent.loop_context.retry_policy.max_retries, 2);
    assert_eq!(
        agent.loop_context.retry_policy.initial_backoff,
        std::time::Duration::from_secs(1),
    );
    assert!(
        (agent.loop_context.retry_policy.backoff_multiplier - 2.0).abs() < f64::EPSILON,
        "default multiplier must be 2x",
    );
}

#[test]
fn retry_policy_setter_applies_to_loop_context() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .retry_policy(RetryPolicy {
            max_retries: 7,
            ..RetryPolicy::default()
        })
        .build()
        .expect("build succeeds");
    assert_eq!(agent.loop_context.retry_policy.max_retries, 7);
}

#[tokio::test]
async fn cancelled_token_yields_cancelled_stop_reason() {
    let token = CancellationToken::new();
    token.cancel();
    let outcome = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .cancel_token(token)
        .run("go")
        .await
        .expect("cancelled run returns Ok(Stopped) with a Cancelled reason");
    assert!(!outcome.is_completed());
    assert_eq!(outcome.stop_reason(), Some(&AgentStopReason::Cancelled));
    // The Stopped arm's partial payload genuinely carries the run's
    // session state — the event store is handed back exactly as on
    // the Completed arm, so a stopped run remains resumable.
    assert!(
        outcome.output().event_store.is_some(),
        "stopped run must hand the event store back on the partial payload"
    );
}

#[tokio::test]
async fn session_resume_accumulates_events() {
    let first = AgentBuilder::new(provider_with(text_completion("first")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .run("question one")
        .await
        .expect("first run succeeds");
    let store = first
        .into_output()
        .event_store
        .expect("event store returned");
    let after_first = store.events().len();
    assert!(after_first > 0, "first run records events");

    let second = AgentBuilder::new(provider_with(text_completion("second")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .session(store)
        .run("question two")
        .await
        .expect("resumed run succeeds");
    let store = second
        .into_output()
        .event_store
        .expect("event store returned");
    assert!(
        store.events().len() > after_first,
        "resumed run appends onto the prior session's events",
    );
}
