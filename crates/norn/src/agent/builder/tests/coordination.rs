use super::*;

#[tokio::test]
async fn agent_registry_wires_fork_spawn_infra() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(256)
        .build()
        .expect("build succeeds");
    let executor: &dyn ToolExecutor = agent.registry.as_ref();
    let result = executor
        .execute(
            "spawn_agent",
            "test-call",
            serde_json::json!({"task": "do x", "model": "gpt-5.5", "role": "worker"}),
        )
        .await;
    if let Err(err) = result {
        assert!(
            !err.to_string().contains("AgentToolInfra"),
            "spawn_agent must get past infra resolution once agent_registry is wired: {err}",
        );
    }
}

/// H13 regression: a *shared* programmatic hook registry (the caller kept
/// an `Arc` clone) plus diagnostic infrastructure used to make `build`
/// fail with "hook registry is shared". The merge-based assembly accepts
/// it and the caller's stop hook still wins first-`Block` conflicts over
/// the diagnostic stop hook.
#[tokio::test]
async fn shared_hooks_arc_with_diagnostic_infra_keeps_user_hooks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));
    let mut registry = HookRegistry::new();
    registry.register(Hook::Stop(Box::new(BlockingStopHook)));
    let shared_hooks = Arc::new(registry);
    let outstanding_clone = Arc::clone(&shared_hooks);

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .hooks(shared_hooks)
        .diagnostic_infra(infra)
        .build()
        .expect("a shared hook Arc must not fail the build");

    let hooks = agent
        .loop_context
        .hooks
        .as_ref()
        .expect("merged hook registry installed");
    let outcome = hooks.run_stop("done").await;
    match outcome {
        HookOutcome::Block { reason } => assert!(
            reason.starts_with("user-stop-hook"),
            "the caller's stop hook must keep precedence: {reason}",
        ),
        HookOutcome::Proceed | HookOutcome::Modify { .. } => {
            panic!("the forwarded user stop hook must still block")
        }
    }
    drop(outstanding_clone);
}

/// H14 regression: the *final merged* hook registry is published on the
/// shared tool context — same `Arc` the loop dispatches — so sub-agent
/// tools can fire subagent hooks.
#[tokio::test]
async fn build_publishes_final_hook_registry_on_tool_context() {
    use crate::integration::hooks::SubagentHook;

    struct BlockingSubagentStop;

    #[async_trait::async_trait]
    impl SubagentHook for BlockingSubagentStop {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "subagent-hook-fired".to_owned(),
            }
        }
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));
    let mut registry = HookRegistry::new();
    registry.register(Hook::Subagent(Box::new(BlockingSubagentStop)));

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .hooks(Arc::new(registry))
        .diagnostic_infra(infra)
        .build()
        .expect("build succeeds");

    let ctx_hooks = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context")
        .get_extension::<HookRegistry>()
        .expect("the merged hook registry must be published on the tool context");
    let loop_hooks = agent
        .loop_context
        .hooks
        .as_ref()
        .expect("loop context carries the merged registry");
    assert!(
        Arc::ptr_eq(&ctx_hooks, loop_hooks),
        "tool context and loop must dispatch the same hook registry",
    );
    let outcome = ctx_hooks.run_subagent_stop("child-1", "worker").await;
    assert!(
        matches!(outcome, HookOutcome::Block { .. }),
        "subagent hooks must fire through the published extension",
    );
}

/// A caller-supplied diagnostic collector must never be silently replaced
/// by the runtime base's collector — on the loop context or on the tool
/// context.
#[test]
fn caller_diagnostics_collector_survives_runtime_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    let custom = DiagnosticCollector::shared();

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .diagnostics(Arc::clone(&custom))
        .build()
        .expect("build succeeds");

    let loop_diag = agent
        .loop_context
        .diagnostics
        .as_ref()
        .expect("loop context diagnostics populated");
    assert!(
        Arc::ptr_eq(loop_diag, &custom),
        "loop context must keep the caller's collector",
    );
    let ctx_diag = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context")
        .get_extension::<DiagnosticCollector>()
        .expect("tool context publishes a diagnostic collector");
    assert!(
        Arc::ptr_eq(&ctx_diag, &custom),
        "tool context must keep the caller's collector",
    );
}

/// `agent_registry` must wire the *complete* fork/spawn runtime:
/// `AgentToolInfra`, `AgentHandles`, `ChildResultSender`, the loop's
/// child-result receiver, and — because every builder-assembled agent
/// is an embedded/headless runtime with no external status observer —
/// the `ReclaimOnResultDelivery` marker.
#[test]
fn agent_registry_installs_complete_fork_spawn_infra() {
    use crate::agent::result_channel::ChildResultSender;
    use crate::tools::agent::{AgentHandles, AgentToolInfra, ReclaimOnResultDelivery};

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(256)
        .build()
        .expect("build succeeds");

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    assert!(
        ctx.get_extension::<AgentToolInfra>().is_some(),
        "AgentToolInfra installed",
    );
    assert!(
        ctx.get_extension::<AgentHandles>().is_some(),
        "AgentHandles installed — spawn_agent refuses to run without it",
    );
    assert!(
        ctx.get_extension::<ChildResultSender>().is_some(),
        "ChildResultSender installed — child results need a destination",
    );
    assert!(
        ctx.get_extension::<ReclaimOnResultDelivery>().is_some(),
        "ReclaimOnResultDelivery installed — embedded runtimes reclaim \
         finished children on result delivery",
    );
    assert!(
        agent.loop_context.child_result_rx.is_some(),
        "the loop must hold the receiver that drains child results",
    );
}

/// Complete spawn path through a built agent: the child runs on the
/// builder's provider, its result arrives on the loop's child-result
/// receiver, and — embedded reclamation — once the result has been
/// delivered, the child's registry entry and the parent-held handle
/// are reclaimed. Completion is driven via the result receiver (not
/// by joining the handle): the wrapper reclaims the handle after
/// delivery, so holding it would race the reclamation under test.
#[tokio::test]
async fn spawned_child_result_reaches_loop_receiver() {
    use crate::tools::agent::AgentHandles;

    let agent_registry = AgentRegistry::shared();
    let mut agent = AgentBuilder::new(provider_with(text_completion("child finished")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(Arc::clone(&agent_registry))
        .child_policy(test_child_policy())
        .child_result_capacity(256)
        .build()
        .expect("build succeeds");

    let executor: &dyn ToolExecutor = agent.registry.as_ref();
    let out = executor
        .execute(
            "spawn_agent",
            "spawn-call",
            serde_json::json!({
                "task": "report back",
                "model": crate::model_catalog::default_selection().model,
                "role": "worker",
            }),
        )
        .await
        .expect("spawn_agent dispatches through the built context");
    let child_id = Uuid::parse_str(out["agent_id"].as_str().expect("agent_id string"))
        .expect("agent_id is a uuid");

    let rx = agent
        .loop_context
        .child_result_rx
        .as_mut()
        .expect("loop holds the child result receiver");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("child result must arrive without timing out")
        .expect("channel open");
    assert_eq!(result.agent_id, child_id);
    assert!(result.succeeded, "completed child reports success");
    assert!(
        result.formatted_message.contains("child finished"),
        "the child's output flows through: {}",
        result.formatted_message,
    );

    // Spawned children are wakeable actors: after result delivery the
    // registry entry and parent-held handle remain so signal_agent can
    // queue work and wake_agent can resume the child explicitly.
    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let handles = ctx
        .get_extension::<AgentHandles>()
        .expect("AgentHandles installed");
    assert!(agent_registry.read().get(child_id).is_some());
    assert!(handles.contains(child_id));
}
