use std::error::Error;
use std::io;
use std::time::Duration;

use chrono::Utc;
use futures_util::{StreamExt, stream};
use parking_lot::{Mutex as StdMutex, RwLock};
use serde_json::json;
use uuid::Uuid;

use super::super::canonical_lifecycle_test_support::{
    canonical_item_values, canonical_payload_items, historical_non_audio_items,
    stateless_payload_input, transcript_item,
};
use super::super::handle::{AgentHandle, AgentWakeRegistry};
use super::super::infra::AgentToolInfra;
use super::*;
use crate::agent::child_policy::{ChildPolicy, MessagingScope};
use crate::agent::fork::{
    FORK_SYSTEM_PREAMBLE, ForkRequirement, ParentPromptPlan, ParentSystemInstruction,
    build_fork_preamble, combine_system_instruction,
};
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentStatus;
use crate::error::ProviderError;
use crate::r#loop::inbound::{ChannelMessage, MessageKind};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::ProviderRequest;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::{Tool as TestTool, ToolOutput as TestToolOutput};

type TestError = Box<dyn Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;

fn test_error(message: impl Into<String>) -> TestError {
    Box::new(io::Error::other(message.into()))
}

fn required<T>(value: Option<T>, message: impl Into<String>) -> TestResult<T> {
    value.ok_or_else(|| test_error(message))
}

fn fork_id_from(output: &TestToolOutput) -> TestResult<Uuid> {
    let value = required(
        output
            .content
            .get("agent_id")
            .and_then(serde_json::Value::as_str),
        "fork output must carry a string agent_id",
    )?;
    Ok(Uuid::parse_str(value)?)
}

fn remove_fork_handle(ctx: &ToolContext, fork_id: Uuid) -> TestResult<AgentHandle> {
    let handles = required(
        ctx.get_extension::<AgentHandles>(),
        "tool context must carry AgentHandles",
    )?;
    required(handles.remove(fork_id), "fork handle must be present")
}

fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-1".to_string(),
        tool_name: "fork".to_string(),
        model_args: args,
        metadata: serde_json::Value::Null,
    }
}

fn done_event_tool_use() -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 5,
            output_tokens: 2,
            ..Usage::default()
        },
        response_id: None,
    }
}

/// Provider that records every full [`ProviderRequest`] it receives
/// while popping scripted response streams in order.
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

fn structured_response_provider(payload: &serde_json::Value) -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "structured-out".to_string(),
                call_id: None,
                name: Some("structured_output".to_string()),
                arguments_delta: payload.to_string(),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            done_event_tool_use(),
        ],
        // Fallback done-turn in case the runner loops after structured output.
        vec![ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }],
    ]))
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

fn parent_ctx(
    provider: Arc<dyn Provider>,
    parent_id: Uuid,
    agent_registry: &Arc<RwLock<AgentRegistry>>,
    tool_registry: Arc<ToolRegistry>,
    router: Arc<MessageRouter>,
) -> (ToolContext, Arc<EventStore>) {
    let event_store = Arc::new(EventStore::new());
    let infra = Arc::new(AgentToolInfra {
        registry: Arc::clone(agent_registry),
        router,
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::clone(&event_store),
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
    (ctx, event_store)
}

struct GatedProvider {
    gate: Arc<tokio::sync::Notify>,
    responses: StdMutex<Vec<Vec<ProviderEvent>>>,
}

impl Provider for GatedProvider {
    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let mut lock = self.responses.lock();
        let batch = if lock.is_empty() {
            vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }]
        } else {
            lock.remove(0)
        };
        drop(lock);
        let mut seq = Some(batch);
        let gate = Arc::clone(&self.gate);
        let s = stream::once(async move { gate.notified().await })
            .flat_map(move |()| stream::iter(seq.take().unwrap_or_default().into_iter().map(Ok)));
        Ok(Box::pin(s))
    }
}

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

mod child_runtime;
mod compaction_tools;
mod coordination;
mod coordination_policy;
mod execution;
mod hooks_lifecycle;
mod persistence;
mod prompt_authority;
mod recursive_delegation;
mod shutdown_logging;
mod terminal_mailbox;
mod tool_surface;
