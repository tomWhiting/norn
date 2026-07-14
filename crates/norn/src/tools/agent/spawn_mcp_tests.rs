use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use super::{AgentHandles, AgentToolInfra, AgentWakeRegistry, SpawnAgentTool};
use crate::agent::child_policy::{
    ChildPolicy, CoordinationEnvelope, DelegationBudget, MessagingScope,
};
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::{AgentRegistry, AgentStatus};
use crate::config::types::VariantSettings;
use crate::error::IntegrationError;
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::integration::{McpClient, McpRuntime, McpRuntimeStore, McpToolDef};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::ToolCallKind;
use crate::provider::tools::ProviderToolDefinition;
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;
use crate::tool::{ToolGeneration, ToolGenerationStore};

struct RecordingTransport {
    methods: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Transport for RecordingTransport {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let request: serde_json::Value =
            serde_json::from_str(&payload).map_err(|error| IntegrationError::McpError {
                reason: format!("invalid recording-transport request: {error}"),
            })?;
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| IntegrationError::McpError {
                reason: "recording transport received a request without a method".to_owned(),
            })?;
        self.methods
            .lock()
            .map_err(|error| IntegrationError::McpError {
                reason: format!("recording transport lock was poisoned: {error}"),
            })?
            .push(method.to_owned());
        serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [{"type": "text", "text": "beta dispatched"}],
                "isError": false
            }
        }))
        .map_err(|error| IntegrationError::McpError {
            reason: format!("invalid recording-transport response: {error}"),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

fn client(name: &str, methods: Arc<Mutex<Vec<String>>>) -> McpClient {
    McpClient::from_transport(name, Box::new(RecordingTransport { methods })).with_test_tools(vec![
        McpToolDef {
            name: "echo".to_owned(),
            description: "records dispatch".to_owned(),
            input_schema: json!({"type": "object"}),
        },
    ])
}

fn parent_context(
    provider: Arc<dyn Provider>,
    registry: &Arc<ToolRegistry>,
    runtime: Arc<McpRuntime>,
) -> (ToolContext, Arc<AgentHandles>) {
    let agent_registry = AgentRegistry::shared();
    let infra = Arc::new(AgentToolInfra {
        registry: agent_registry,
        router: Arc::new(MessageRouter::new()),
        pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
        provider,
        event_store: Arc::new(EventStore::new()),
        agent_id: Uuid::new_v4(),
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::clone(registry)),
        session: Arc::new(crate::session::SessionBinding::ephemeral_root()),
    });
    let handles = Arc::new(AgentHandles::new());
    let context = ToolContext::empty();
    context.insert_extension(infra);
    context.insert_extension(Arc::clone(&handles));
    context.insert_extension(Arc::new(AgentWakeRegistry::new()));
    context.insert_extension(Arc::new(CoordinationEnvelope {
        child_policy: ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 4,
            },
            inbound_capacity: 8,
            loop_config: None,
        },
        child_result_capacity: 8,
    }));
    let generation = Arc::new(ToolGeneration::from_registry(registry, 0));
    let generations = Arc::new(ToolGenerationStore::new(Arc::clone(&generation)));
    let runtimes = Arc::new(McpRuntimeStore::new(generation, Arc::clone(&runtime)));
    context.insert_extension(runtime);
    context.insert_extension(generations);
    context.insert_extension(runtimes);
    (context, handles)
}

fn done(reason: StopReason) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: reason,
        usage: Usage::default(),
        response_id: None,
    }
}

#[tokio::test]
async fn variant_child_can_widen_root_mcp_view_and_dispatch_beta()
-> Result<(), Box<dyn std::error::Error>> {
    let alpha_calls = Arc::new(Mutex::new(Vec::new()));
    let beta_calls = Arc::new(Mutex::new(Vec::new()));
    let runtime = Arc::new(McpRuntime::from_test_clients(vec![
        client("alpha", Arc::clone(&alpha_calls)),
        client("beta", Arc::clone(&beta_calls)),
    ]));
    let alpha_name = runtime
        .tool_names_for_servers(&["alpha".to_owned()])?
        .into_iter()
        .next()
        .ok_or("alpha fixture exposed no tool")?;
    let beta_name = runtime
        .tool_names_for_servers(&["beta".to_owned()])?
        .into_iter()
        .next()
        .ok_or("beta fixture exposed no tool")?;
    let mut registry = ToolRegistry::new();
    runtime.register_tools(&mut registry)?;
    runtime.restrict_registry_to_servers(&mut registry, &["alpha".to_owned()])?;
    assert!(registry.get(&alpha_name).is_some());
    assert!(registry.get(&beta_name).is_none());
    assert!(registry.get_registered(&beta_name).is_some());

    let provider = Arc::new(MockProvider::new(vec![
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: "beta-call".to_owned(),
                call_id: None,
                name: Some(beta_name.clone()),
                arguments_delta: json!({
                    "tool_use_description": "exercise the beta MCP server"
                })
                .to_string(),
                kind: ToolCallKind::Function,
            },
            done(StopReason::ToolUse),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "complete".to_owned(),
            },
            done(StopReason::EndTurn),
        ],
    ]));
    let (context, handles) = parent_context(
        Arc::clone(&provider) as Arc<dyn Provider>,
        &Arc::new(registry),
        Arc::clone(&runtime),
    );
    let mut variants = BTreeMap::new();
    variants.insert(
        "beta-child".to_owned(),
        VariantSettings {
            prompt: Some("Use the selected MCP server.".to_owned()),
            mcp_servers: Some(vec!["beta".to_owned()]),
            ..VariantSettings::default()
        },
    );
    let workspace = tempfile::tempdir()?;
    let catalog = crate::agent::variants::VariantCatalog::build(Some(&variants), workspace.path())?;
    context.insert_extension(Arc::new(catalog));

    let output = SpawnAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "spawn-beta".to_owned(),
                tool_name: "spawn_agent".to_owned(),
                model_args: json!({
                    "task": "call beta",
                    "variant": "beta-child",
                    "model": crate::model_catalog::default_selection().model
                }),
                metadata: serde_json::Value::Null,
            },
            &context,
        )
        .await?;
    let child_id = output
        .content
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("spawn result omitted agent_id")?;
    let child_id = Uuid::parse_str(child_id)?;
    let mut status = handles
        .status_rx(child_id)
        .ok_or("spawned child had no status receiver")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        status
            .wait_for(|value| *value == AgentStatus::Idle || value.is_terminal())
            .await
            .map(|_| ())
    })
    .await??;

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    for request in requests {
        let names: Vec<_> = request
            .tools
            .iter()
            .filter_map(|definition| match definition {
                ProviderToolDefinition::Function(function) => Some(function.name.as_str()),
                ProviderToolDefinition::Hosted(_) => None,
            })
            .collect();
        assert!(names.contains(&beta_name.as_str()));
        assert!(!names.contains(&alpha_name.as_str()));
    }
    assert!(
        alpha_calls
            .lock()
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .is_empty()
    );
    assert_eq!(
        *beta_calls
            .lock()
            .map_err(|error| std::io::Error::other(error.to_string()))?,
        ["tools/call"]
    );
    Ok(())
}
