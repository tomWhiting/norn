use super::*;

/// V2-R2 (persistent parent): the spawned child's store is a REAL
/// write-through timeline under the root's `children/` dir — the index
/// row carries `rel_path` + `parent_id`, the child's own run events
/// land on disk, and the parent's file carries the `ChildBranch`
/// reservation naming the child (parent-first ordering's durable
/// record).
#[tokio::test]
async fn spawn_under_persistent_parent_persists_child_timeline() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let canonical_output = spawn_non_audio_items("spawn_persisted", "branched child");
    let mut provider_events = canonical_output
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, raw)| Ok(completed_item_event(raw, u64::try_from(index)?)?))
        .collect::<TestResult<Vec<_>>>()?;
    provider_events.push(done_event());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![provider_events]));
    let parent = Uuid::new_v4();
    let agent_registry = AgentRegistry::shared();
    let (ctx, manager, root_session_id) = persistent_parent_ctx(
        tmp.path(),
        provider,
        parent,
        &agent_registry,
        Arc::new(ToolRegistry::new()),
    )?;

    let tool = SpawnAgentTool::new();
    let out = tool
        .execute(
            &envelope_for(json!({"task": "branch me", "model": CATALOG_MODEL, "role": "worker"})),
            &ctx,
        )
        .await?;
    let child_id = Uuid::parse_str(
        out.content["agent_id"]
            .as_str()
            .ok_or("required test value")?,
    )?;
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

    // Index row: rel_path under the root's children/ dir + parent
    // linkage; the file really exists at the nested path.
    let row = manager.resolve(&child_id.to_string())?;
    let rel = row.rel_path.as_deref().ok_or("required test value")?;
    assert!(
        rel.starts_with(&format!("{root_session_id}/children/worker-"))
            && std::path::Path::new(rel)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")),
        "child file must live under the root's children/ dir: {rel}",
    );
    assert_eq!(row.parent_id.as_deref(), Some(root_session_id.as_str()));
    assert!(
        tmp.path().join(rel).exists(),
        "child timeline file must exist on disk",
    );

    // The child's run events are ON DISK — write-through, not
    // memory-only (Gap 1 closure).
    let child_events = events_on_disk(&manager, &child_id.to_string());
    assert!(
        child_events
            .iter()
            .any(|e| matches!(e, SessionEvent::ChildBranch { .. })),
        "the child's file carries its ChildBranch provenance header",
    );
    assert!(
        child_events.iter().any(|e| matches!(
            e,
            SessionEvent::AssistantMessage { content, .. } if content.contains("branched child")
        )),
        "the child's own run output must reach its on-disk timeline",
    );
    assert_eq!(
        canonical_item_values(&child_events),
        canonical_output,
        "the spawned child's completed item must survive its production write-through path",
    );
    let replay_input = stateless_payload_input(&child_events)?;
    assert_eq!(
        canonical_payload_items(&replay_input),
        canonical_output,
        "the spawned child's persisted canonical items must be the exact replay corpus",
    );

    // Parent side ON DISK: the ChildBranch reservation names the child.
    let parent_events = events_on_disk(&manager, &root_session_id);
    assert!(
        parent_events.iter().any(|e| matches!(
            e,
            SessionEvent::ChildBranch {
                child_session_id: Some(c),
                path_address,
                ..
            } if *c == child_id.to_string() && path_address.starts_with("root/worker-")
        )),
        "the parent's file must carry the child's reservation: {parent_events:?}",
    );
    Ok(())
}

// NH-006 R5 / C56 + C57: SubagentHook fires on launch (`start`)
// and on completion (`stop`). The shared HookRegistry is installed
// on the parent's ToolContext as an Arc<HookRegistry> extension —
// that is how the spawn site reaches it without a LoopContext.
#[tokio::test]
async fn subagent_hook_start_and_stop_fire_around_spawn() -> TestResult {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, SubagentHook};

    struct CountingSubagentHook {
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SubagentHook for CountingSubagentHook {
        async fn on_subagent_start(&self, _agent_id: &str, _agent_type: &str) {
            self.start_count.fetch_add(1, AtomicOrdering::SeqCst);
        }
        async fn on_subagent_stop(&self, _agent_id: &str, _agent_type: &str) -> HookOutcome {
            self.stop_count.fetch_add(1, AtomicOrdering::SeqCst);
            HookOutcome::Proceed
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

    let start_count = Arc::new(AtomicUsize::new(0));
    let stop_count = Arc::new(AtomicUsize::new(0));
    let mut registry = HookRegistry::new();
    registry.register(Hook::Subagent(Box::new(CountingSubagentHook {
        start_count: Arc::clone(&start_count),
        stop_count: Arc::clone(&stop_count),
    })));
    ctx.insert_extension(Arc::new(registry));

    let tool = SpawnAgentTool::new();
    let _child_id = spawn_and_join(
        &tool,
        &ctx,
        json!({"task": "do it", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    assert_eq!(
        start_count.load(AtomicOrdering::SeqCst),
        1,
        "SubagentHook::on_subagent_start must fire exactly once per spawn",
    );
    assert_eq!(
        stop_count.load(AtomicOrdering::SeqCst),
        1,
        "SubagentHook::on_subagent_stop must fire exactly once per spawn",
    );
    Ok(())
}
