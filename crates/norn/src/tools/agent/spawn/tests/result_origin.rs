//! Production result timestamps precede delayed wrapper hooks and preserve the actual timeline.

use super::*;
use crate::integration::hooks::{Hook, SubagentHook};

#[derive(Clone)]
struct HookCall {
    agent_id: String,
    agent_type: String,
    at: chrono::DateTime<Utc>,
}

struct StopGate {
    calls: Arc<StdMutex<Vec<HookCall>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl SubagentHook for StopGate {
    async fn on_subagent_start(&self, agent_id: &str, agent_type: &str) {
        self.calls.lock().push(HookCall {
            agent_id: agent_id.to_owned(),
            agent_type: agent_type.to_owned(),
            at: Utc::now(),
        });
    }

    async fn on_subagent_stop(&self, agent_id: &str, agent_type: &str) -> HookOutcome {
        self.calls.lock().push(HookCall {
            agent_id: agent_id.to_owned(),
            agent_type: agent_type.to_owned(),
            at: Utc::now(),
        });
        self.entered.notify_one();
        self.release.notified().await;
        HookOutcome::Proceed
    }
}

#[tokio::test]
async fn result_origin_is_captured_before_delayed_wrapper_stop() -> TestResult {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![done_event()]]));
    let parent = Uuid::new_v4();
    let registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        parent,
        &registry,
        Arc::new(ToolRegistry::new()),
        Arc::new(MessageRouter::new()),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::Subagent(Box::new(StopGate {
        calls: Arc::clone(&calls),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    })));
    ctx.insert_extension(Arc::new(hooks));
    let out = SpawnAgentTool::new()
        .execute(
            &envelope_for(json!({"task":"original task","model":CATALOG_MODEL,"role":"worker"})),
            &ctx,
        )
        .await?;
    let child = Uuid::parse_str(out.content["agent_id"].as_str().ok_or("missing child id")?)?;
    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await?;
    let hook_calls = calls.lock().clone();
    assert_eq!(hook_calls.len(), 2);
    assert_eq!(hook_calls[1].agent_id, child.to_string());
    assert_eq!(hook_calls[1].agent_type, "worker");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("missing result")?;
    let origin = result.origin.ok_or("production result lacks origin")?;
    assert!(origin.completed_at <= hook_calls[1].at);
    assert!(origin.started_at >= hook_calls[0].at);
    assert_eq!(origin.source.agent_id, child);
    assert_eq!(origin.source.parent_agent_id, Some(parent));
    let handles = ctx.require_extension::<AgentHandles>()?;
    let store = handles.event_store(child).ok_or("missing child store")?;
    assert_eq!(origin.source, *store.history_reader()?.source());
    assert!(
        origin
            .end_at_event
            .as_ref()
            .is_some_and(|id| store.get(id).is_some())
    );
    wait_for_child_status(&ctx, child, AgentStatus::Idle).await;
    Ok(())
}
