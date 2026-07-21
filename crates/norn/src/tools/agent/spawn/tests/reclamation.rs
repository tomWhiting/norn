use super::*;

/// Awaits `cond` becoming true within 5 seconds, polling. Used where
/// the asserted state is produced by the child wrapper task *after*
/// the observable result delivery (so there is no handle left to
/// join on).
pub(super) async fn wait_for_condition<F: Fn() -> bool>(cond: F, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for: {what}",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub(super) async fn wait_for_child_status(
    ctx: &ToolContext,
    child_id: Uuid,
    expected: AgentStatus,
) {
    let handles = ctx.get_extension::<AgentHandles>();
    assert!(handles.is_some(), "AgentHandles must be installed");
    let Some(handles) = handles else {
        return;
    };
    let status_rx = handles.status_rx(child_id);
    assert!(
        status_rx.is_some(),
        "status receiver must be tracked for {child_id}",
    );
    let Some(mut status_rx) = status_rx else {
        return;
    };
    let reached = status_rx.wait_for(|status| *status == expected).await;
    assert!(
        reached.is_ok(),
        "child {child_id} must reach {expected:?}: {reached:?}",
    );
}

/// Wakeable-spawn regression: a naturally-completed spawned child is
/// retained as Idle even when [`ReclaimOnResultDelivery`] is installed.
/// Explicit `close_agent` is the cleanup boundary for persistent
/// spawned actors.
#[tokio::test]
async fn delivered_result_retains_registry_and_handle_when_marker_present() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "child done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    let result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(result.agent_id, child_id);
    assert!(result.succeeded);

    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?;
    assert!(handles.contains(child_id));
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    Ok(())
}

/// Reclamation ownership: with the marker installed but NO result
/// channel, the wrapper must not reclaim — the handle holder owns
/// the end of life (there is no delivery to anchor reclamation to).
#[tokio::test]
async fn no_reclamation_without_result_channel_even_with_marker() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "child done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?;
    let mut status_rx = handles.status_rx(child_id).ok_or("required test value")?;
    status_rx.wait_for(|s| *s == AgentStatus::Idle).await?;

    assert!(
        handles.contains(child_id),
        "without a result channel the handle holder owns reclamation",
    );
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    Ok(())
}

/// TUI-mode regression: without the marker (default), a delivered
/// result must NOT reclaim — terminal entries stay observable for
/// the external observer's hold window.
#[tokio::test]
async fn no_reclamation_without_marker_even_with_result_channel() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "child done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("required test value")?;

    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?;
    let mut status_rx = handles.status_rx(child_id).ok_or("required test value")?;
    status_rx.wait_for(|s| *s == AgentStatus::Idle).await?;
    assert!(
        handles.contains(child_id),
        "without the marker the external observer owns reclamation",
    );
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
    );
    Ok(())
}

/// A stop hook's Block suppresses the terminal transition — and must
/// also suppress reclamation: a deliberately-held-open child is
/// never swept away.
#[tokio::test]
async fn hook_block_suppresses_reclamation() -> TestResult {
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

    struct BlockOnStop;

    #[async_trait]
    impl SubagentHook for BlockOnStop {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "child has more to do".to_owned(),
            }
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    ctx.insert_extension(Arc::new(super::ReclaimOnResultDelivery));
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::Subagent(Box::new(BlockOnStop)));
    ctx.insert_extension(Arc::new(hook_registry));

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "finish", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;

    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("required test value")?;

    let handles = ctx
        .get_extension::<AgentHandles>()
        .ok_or("required test value")?;
    let mut status_rx = handles.status_rx(child_id).ok_or("required test value")?;
    status_rx.wait_for(|s| *s == AgentStatus::Idle).await?;

    assert!(
        handles.contains(child_id),
        "a hook-blocked child's handle must not be reclaimed",
    );
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
        "Block suppresses the terminal transition; persistent children park idle",
    );
    Ok(())
}

// NH-006 R5: SubagentHook::on_subagent_stop returning Block must
// suppress the registry's terminal transition. The child stays in
// whatever pre-terminal state it reached (Active here, since the
// wrapper never called mark_completing).
#[tokio::test]
async fn subagent_hook_stop_block_suppresses_terminal_mark() -> TestResult {
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

    struct BlockOnStop;

    #[async_trait]
    impl SubagentHook for BlockOnStop {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {}
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            HookOutcome::Block {
                reason: "child has more to do".to_owned(),
            }
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_string(),
        },
        done_event(),
    ]]));
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );

    let mut registry = HookRegistry::new();
    registry.register(Hook::Subagent(Box::new(BlockOnStop)));
    ctx.insert_extension(Arc::new(registry));

    let tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    let status = agent_registry
        .read()
        .get(child_id)
        .ok_or("required test value")?
        .status;
    assert_ne!(
        status,
        AgentStatus::Completed,
        "Block from SubagentHook::on_subagent_stop must prevent mark_completed",
    );
    Ok(())
}
