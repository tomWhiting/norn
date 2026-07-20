mod cancellation;
mod close_and_routes;
mod completion_lifecycle;
mod context_and_files;
mod grandchildren;
mod launch_basics;
mod leaf_delegation;
mod linger_and_limits;
mod loop_config;
mod output_and_profiles;
mod permissions;
mod persistence;
mod policy_validation;
mod reasoning;
mod reclamation;
mod runtime_model;
mod signal_resume;
mod skills;
mod started_audit;
mod usage_rollup;
mod variants;

use close_and_routes::ParkedProvider;
use grandchildren::idle_grandchild_entry;
use output_and_profiles::CountingStubTool;
use reclamation::{wait_for_child_status, wait_for_condition};
use usage_rollup::done_with;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use std::path::PathBuf;

use std::time::Duration;

use futures_util::{StreamExt, stream};
use parking_lot::{Mutex as StdMutex, RwLock};
use serde_json::json;
use uuid::Uuid;

use super::super::canonical_lifecycle_test_support::{
    canonical_item_values, canonical_payload_items, completed_item_event, spawn_non_audio_items,
    stateless_payload_input,
};
use super::super::infra::AgentToolInfra;
use super::*;
use crate::agent::child_policy::MessagingScope;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentStatus;
use crate::error::ProviderError;
use crate::integration::hooks::HookOutcome;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::ProviderRequest;
use crate::provider::tools::ProviderToolDefinition;
use crate::provider::traits::Provider;
use crate::provider::traits::ProviderStream;
use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;
use crate::tool::traits::{Tool as TestTool, ToolOutput as TestToolOutput};

/// Catalogued model used for child launches throughout these tests:
/// child launch paths validate the child's context window against the
/// model catalog (agent-variants §7), so an uncatalogued placeholder
/// would be rejected before the behavior under test runs. Factual
/// (the generated catalog's default), never invented.
const CATALOG_MODEL: &str = crate::model_catalog::default_selection().model;

fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-1".to_string(),
        tool_name: "spawn_agent".to_string(),
        model_args: args,
        metadata: serde_json::Value::Null,
    }
}

fn done_event() -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        response_id: None,
    }
}

fn done_event_tool_use() -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        response_id: None,
    }
}

/// Documented-proposal coordination envelope used by tests — a
/// deliberate test-caller choice, never a library default.
fn test_envelope() -> CoordinationEnvelope {
    use crate::agent::child_policy::DelegationBudget;
    CoordinationEnvelope {
        child_policy: ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        child_result_capacity: 256,
    }
}

/// Builds a parent [`ToolContext`] with [`AgentToolInfra`], an empty
/// [`AgentHandles`], and the [`CoordinationEnvelope`] — the minimum a
/// spawning agent needs.
fn parent_ctx(
    provider: Arc<dyn Provider>,
    parent_id: Uuid,
    agent_registry: &Arc<RwLock<AgentRegistry>>,
    tool_registry: Arc<ToolRegistry>,
    router: Arc<MessageRouter>,
) -> ToolContext {
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(agent_registry),
        router,
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: parent_id,
        parent_id: None,
        grant: None,
        tool_registry: Some(tool_registry),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);
    ctx.insert_extension(Arc::new(AgentHandles::new()));
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
    ctx.insert_extension(Arc::new(test_envelope()));
    ctx
}

/// A persistent parent context: a real `SessionManager` root session
/// in `tmp`, its sink-equipped store on the infra, and a persistent
/// [`SessionBinding`](crate::session::SessionBinding) so spawned
/// children mint REAL on-disk timelines (V2-R2).
fn persistent_parent_ctx(
    tmp: &std::path::Path,
    provider: Arc<dyn Provider>,
    parent_id: Uuid,
    agent_registry: &Arc<RwLock<AgentRegistry>>,
    tool_registry: Arc<ToolRegistry>,
) -> TestResult<(ToolContext, crate::session::SessionManager, String)> {
    use crate::session::manager::{CreateSessionOptions, SessionManager};
    use crate::session::store::DurabilityPolicy;
    use crate::session::{SessionBinding, SessionBrancher};
    let manager = SessionManager::new(tmp);
    let opened = manager.create(
        CreateSessionOptions {
            model: "haiku".to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let root_session_id = opened.entry.id.clone();
    let binding = Arc::new(SessionBinding::persistent_root(
        Arc::new(SessionBrancher::new(
            manager.clone(),
            root_session_id.clone(),
            DurabilityPolicy::Flush,
        )),
        &opened.entry,
        &[],
    ));
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(agent_registry),
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(opened.store),
        agent_id: parent_id,
        parent_id: None,
        grant: None,
        tool_registry: Some(tool_registry),
        session: binding,
    });
    let ctx = ToolContext::empty();
    ctx.insert_extension(infra);
    ctx.insert_extension(Arc::new(AgentHandles::new()));
    ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
    ctx.insert_extension(Arc::new(test_envelope()));
    Ok((ctx, manager, root_session_id))
}
/// Read a persisted session's events back from DISK through its index
/// row — never from memory — so assertions prove durability.
fn events_on_disk(manager: &crate::session::SessionManager, id: &str) -> Vec<SessionEvent> {
    let entry = manager.resolve(id);
    assert!(entry.is_ok(), "session must be indexed: {id}");
    let Ok(entry) = entry else {
        return Vec::new();
    };
    let replay =
        crate::session::persistence::io::read_session_events_for_entry(manager.data_dir(), &entry);
    assert!(replay.is_ok(), "session must replay from disk: {id}");
    let Ok(replay) = replay else {
        return Vec::new();
    };
    replay.events
}

/// Drives a spawn until the child is no longer actively running.
///
/// Natural child completion now parks the spawned child in
/// [`AgentStatus::Idle`] so it can be woken later; only hard terminal
/// outcomes make the wrapper exit. This helper therefore observes the
/// status watch instead of taking and joining the handle.
async fn spawn_and_join(tool: &SpawnAgentTool, ctx: &ToolContext, args: serde_json::Value) -> Uuid {
    let output = tool.execute(&envelope_for(args), ctx).await;
    assert!(output.is_ok(), "spawn must succeed: {output:?}");
    let Ok(out) = output else {
        return Uuid::nil();
    };
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["status"], "active");
    assert!(
        out.content.get("result_summary").is_none(),
        "immediate return carries no result"
    );
    let agent_id = out.content["agent_id"].as_str();
    assert!(
        agent_id.is_some(),
        "spawn output must carry agent_id: {:?}",
        out.content,
    );
    let Some(agent_id) = agent_id else {
        return Uuid::nil();
    };
    let child_id = Uuid::parse_str(agent_id);
    assert!(
        child_id.is_ok(),
        "spawn agent_id must be a UUID: {agent_id}"
    );
    let Ok(child_id) = child_id else {
        return Uuid::nil();
    };
    let handles = ctx.get_extension::<AgentHandles>();
    assert!(handles.is_some(), "AgentHandles must be installed");
    let Some(handles) = handles else {
        return Uuid::nil();
    };
    let status_rx = handles.status_rx(child_id);
    assert!(
        status_rx.is_some(),
        "status receiver must be stored for child {child_id}",
    );
    let Some(mut status_rx) = status_rx else {
        return Uuid::nil();
    };
    let reached_terminal = tokio::time::timeout(Duration::from_secs(5), async {
        status_rx
            .wait_for(|status| *status == AgentStatus::Idle || status.is_terminal())
            .await
    })
    .await;
    assert!(
        matches!(reached_terminal, Ok(Ok(_))),
        "child must reach idle or terminal status: {reached_terminal:?}",
    );
    child_id
}

/// Stub tool that records the [`AgentToolInfra`] identity it sees, so a
/// test can prove the child dispatched against its own context.
struct IdentityStubTool {
    seen_agent: Arc<StdMutex<Option<Uuid>>>,
    seen_parent: Arc<StdMutex<Option<Uuid>>>,
}

#[async_trait]
impl TestTool for IdentityStubTool {
    fn name(&self) -> &'static str {
        "identity"
    }
    fn description(&self) -> &'static str {
        "records the agent identity it sees"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }
    async fn execute(
        &self,
        _envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<TestToolOutput, ToolError> {
        if let Some(infra) = ctx.get_extension::<AgentToolInfra>() {
            *self.seen_agent.lock() = Some(infra.agent_id);
            *self.seen_parent.lock() = infra.parent_id;
        }
        Ok(TestToolOutput::success(serde_json::json!({"ok": true})))
    }
}

/// Minimal echo stub tool.
struct EchoStubTool {
    tool_name: &'static str,
}

#[async_trait]
impl TestTool for EchoStubTool {
    fn name(&self) -> &'static str {
        self.tool_name
    }
    fn description(&self) -> &'static str {
        "echo stub for tests"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }
    async fn execute(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<TestToolOutput, ToolError> {
        Ok(TestToolOutput::success(
            serde_json::json!({"echoed": self.tool_name}),
        ))
    }
}

/// Provider that gates its first stream behind a [`tokio::sync::Notify`]
/// so a test can observe the child's `Active` status before it runs.
struct GatedProvider {
    gate: Arc<tokio::sync::Notify>,
    responses: StdMutex<Vec<Vec<ProviderEvent>>>,
}

impl Provider for GatedProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let mut seq = Some(self.responses.lock().remove(0));
        let gate = Arc::clone(&self.gate);
        let s = stream::once(async move { gate.notified().await })
            .flat_map(move |()| stream::iter(seq.take().unwrap_or_default().into_iter().map(Ok)));
        Ok(Box::pin(s))
    }
}

/// Provider that records the `tools` of every request it receives.
struct CapturingProvider {
    captured: Arc<StdMutex<Vec<ProviderToolDefinition>>>,
    responses: StdMutex<Vec<Vec<ProviderEvent>>>,
}

impl Provider for CapturingProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.captured.lock().clone_from(&request.tools);
        let seq = self.responses.lock().remove(0);
        Ok(Box::pin(stream::iter(seq.into_iter().map(Ok))))
    }
}

struct RequestCapturingProvider {
    captured: Arc<StdMutex<Vec<ProviderRequest>>>,
    responses: StdMutex<Vec<Vec<ProviderEvent>>>,
}

impl Provider for RequestCapturingProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.captured.lock().push(request);
        let seq = self.responses.lock().remove(0);
        Ok(Box::pin(stream::iter(seq.into_iter().map(Ok))))
    }
}
