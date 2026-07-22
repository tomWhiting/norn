use super::*;

#[test]
fn model_override_wins_over_profile() {
    let profile = Profile {
        model: "from-profile".to_string(),
        ..Profile::default()
    };
    let agent = AgentBuilder::new(provider_with(vec![]))
        .working_dir(std::env::temp_dir())
        .profile(profile)
        .model("override-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .build()
        .expect("build succeeds");
    assert_eq!(agent.model, "override-model");
}

#[tokio::test]
async fn run_executes_and_returns_output() {
    let outcome = AgentBuilder::new(provider_with(text_completion("Hello from the agent")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .run("say hello")
        .await
        .expect("run succeeds");
    assert!(
        outcome.is_completed(),
        "no-schema text completion is a completed run"
    );
    assert_eq!(
        outcome.output().text().as_deref(),
        Some("Hello from the agent")
    );
    assert!(
        outcome.output().event_store.is_some(),
        "event store is returned"
    );
}

/// An empty (or whitespace-only) prompt has no defined model-facing
/// meaning — it must be rejected with a typed error at the run
/// boundary, never sent to the provider as undefined behaviour.
#[tokio::test]
async fn run_rejects_empty_and_whitespace_prompts() {
    for prompt in ["", "   ", "\n\t "] {
        let agent = AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .build()
            .expect("build succeeds");
        match agent.run(prompt).await {
            Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
                assert!(reason.contains("empty prompt"), "{reason}");
            }
            Err(other) => panic!("expected a typed config error, got: {other}"),
            Ok(_) => panic!("prompt {prompt:?} must be rejected"),
        }
    }
}

/// The handle's subscription replaces the old `run_stream`: configure
/// the channel capacity on the builder, subscribe through the handle,
/// and drain alongside the run. Real consumers drain concurrently and
/// stop when the run future resolves (the handle keeps the channel
/// open, so end-of-run — not channel close — is the stop signal).
#[tokio::test]
async fn handle_subscription_delivers_events() {
    let agent = AgentBuilder::new(provider_with(text_completion("streamed")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .event_channel_capacity(64)
        .build()
        .expect("build succeeds");
    let handle = agent.handle();
    let mut rx = handle
        .subscribe()
        .expect("event channel configured — subscribe must succeed");
    let output = agent.run("go").await.expect("run succeeds");
    assert!(output.is_completed());
    // Every event the run broadcast is buffered for this receiver.
    let mut seen = 0usize;
    while rx.try_recv().is_ok() {
        seen += 1;
    }
    assert!(seen > 0, "the run must deliver at least one event");
}

#[test]
fn subscribe_without_event_channel_is_none() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    assert!(
        agent.handle().subscribe().is_none(),
        "no configured channel means no subscription — never a silent dead channel",
    );
    assert!(agent.handle().inbound_sender().is_none());
}

#[test]
fn zero_channel_capacities_fail_build() {
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .event_channel_capacity(0)
        .build();
    assert!(matches!(
        result,
        Err(NornError::Config(ConfigError::InvalidConfig { .. }))
    ));
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .inbound_capacity(0)
        .build();
    assert!(matches!(
        result,
        Err(NornError::Config(ConfigError::InvalidConfig { .. }))
    ));
}

/// W3.0: wiring `.agent_registry(..)` without the coordination
/// envelope is a build-time error naming every missing setter — Norn
/// never assumes a default child policy or channel capacity.
#[test]
fn agent_registry_without_envelope_fails_build() {
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .build(),
    );
    assert!(reason.contains(".child_policy"), "{reason}");
    assert!(reason.contains(".child_result_capacity"), "{reason}");
}

/// Each missing half of the envelope is named individually.
#[test]
fn partial_coordination_envelope_names_the_missing_setter() {
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .build(),
    );
    assert!(reason.contains(".child_result_capacity"), "{reason}");
    assert!(!reason.contains(".child_policy(ChildPolicy"), "{reason}");

    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_result_capacity(256)
            .build(),
    );
    assert!(reason.contains(".child_policy"), "{reason}");
}

/// An envelope without `.agent_registry(..)` would be silently
/// ignored — that is rejected, never tolerated.
#[test]
fn orphaned_coordination_envelope_fails_build() {
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .child_policy(test_child_policy())
            .build(),
    );
    assert!(reason.contains("child_policy"), "{reason}");
    assert!(reason.contains("agent_registry"), "{reason}");

    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .child_result_capacity(256)
            .build(),
    );
    assert!(reason.contains("child_result_capacity"), "{reason}");
    assert!(reason.contains("agent_registry"), "{reason}");
}

/// Zero capacities anywhere in the envelope fail the build — a
/// zero-capacity channel cannot exist.
#[test]
fn zero_coordination_capacities_fail_build() {
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(test_child_policy())
            .child_result_capacity(0)
            .build(),
    );
    assert!(reason.contains("child_result_capacity is 0"), "{reason}");

    let mut policy = test_child_policy();
    policy.inbound_capacity = 0;
    let reason = invalid_config_reason(
        AgentBuilder::new(provider_with(vec![]))
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .agent_registry(AgentRegistry::shared())
            .child_policy(policy)
            .child_result_capacity(256)
            .build(),
    );
    assert!(
        reason.contains("child_policy.inbound_capacity is 0"),
        "{reason}",
    );
}

/// W3.0 carriage: the validated envelope is published on the shared
/// tool context verbatim, so the spawn/fork paths read the root's
/// policy and capacities from one place.
#[test]
fn coordination_envelope_is_published_on_tool_context() {
    let policy = test_child_policy();
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(policy.clone())
        .child_result_capacity(17)
        .build()
        .expect("build succeeds");

    let envelope = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context")
        .get_extension::<CoordinationEnvelope>()
        .expect("CoordinationEnvelope published on the shared context");
    assert_eq!(envelope.child_policy, policy);
    assert_eq!(envelope.child_result_capacity, 17);
    assert!(
        agent.loop_context.child_result_rx.is_some(),
        "the child-result receiver is wired alongside the envelope",
    );
}

/// W3.5 (review U1-M1): with NO explicit `cancel_token`, the builder
/// must still bind the published `AgentCancellation` and the agent's
/// own run token (the one `Agent::run` / `AgentHandle::cancel`
/// observe) to the SAME trigger. Two independently-minted defaults
/// compile fine and silently sever the cascade from the control
/// surface on every default-built agent — this pins the single
/// resolution in `build`.
#[test]
fn default_built_agent_publishes_its_own_run_token() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .build()
        .expect("build succeeds");

    let published = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::tools::agent::AgentCancellation>()
        .expect("AgentCancellation published on the shared context");
    assert!(!published.0.is_cancelled());

    agent.cancel.cancel();
    assert!(
        published.0.is_cancelled(),
        "the published cascade token must share the trigger with the \
         default-built agent's own run token",
    );
}

/// W3.2 routing: a root built with an inbound channel registers its
/// sender in the message router under its own id, so children can
/// address `"parent"` at the top level; without an inbound channel
/// the root is honestly unrouted.
#[test]
fn root_inbound_sender_registers_in_message_router() {
    let id = Uuid::new_v4();
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_id(id)
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .inbound_capacity(8)
        .build()
        .expect("build succeeds");
    let infra = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::tools::agent::AgentToolInfra>()
        .expect("AgentToolInfra installed");
    assert!(
        infra.router.is_routed(id),
        "the root's inbound sender must be registered under the root id",
    );

    let unrouted_id = Uuid::new_v4();
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_id(unrouted_id)
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .build()
        .expect("build succeeds");
    let infra = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::tools::agent::AgentToolInfra>()
        .expect("AgentToolInfra installed");
    assert!(
        !infra.router.is_routed(unrouted_id),
        "a root without an inbound channel must not be routed",
    );
}
