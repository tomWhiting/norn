use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::child_tool_snapshot;
use crate::error::IntegrationError;
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::integration::{McpClient, McpRuntime, McpRuntimeStore, McpToolDef};
use crate::r#loop::config::ToolExecutor;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolOutput};
use crate::tool::{ToolContext, ToolGeneration, ToolRegistry};

struct StableProbe(Arc<AtomicUsize>);

#[async_trait]
impl Tool for StableProbe {
    fn name(&self) -> &'static str {
        "stable_probe"
    }

    fn description(&self) -> &'static str {
        "stable identity probe"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, crate::error::ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::success(serde_json::json!({"ok": true})))
    }
}

struct EchoTransport(Arc<AtomicUsize>);

#[async_trait]
impl Transport for EchoTransport {
    async fn request(
        &self,
        _payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: Some(serde_json::json!(request_id)),
            result: Some(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "isError": false
            })),
            error: None,
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

fn client(name: &str, calls: Arc<AtomicUsize>) -> McpClient {
    McpClient::from_transport(name, Box::new(EchoTransport(calls))).with_test_tools(vec![
        McpToolDef {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        },
    ])
}

fn source_generation(
    context: Arc<ToolContext>,
    stable_calls: Arc<AtomicUsize>,
    runtime: &McpRuntime,
    revision: u64,
) -> Result<Arc<ToolGeneration>, IntegrationError> {
    let mut registry = ToolRegistry::with_context(context);
    registry.register(Box::new(StableProbe(stable_calls)));
    runtime.register_tools(&mut registry)?;
    Ok(Arc::new(ToolGeneration::from_registry(&registry, revision)))
}

fn names(snapshot: &super::ChildToolSnapshot) -> Vec<String> {
    snapshot
        .definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect()
}

#[tokio::test]
async fn selected_child_uses_full_startup_pool_and_retains_stable_tool_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let stable_calls = Arc::new(AtomicUsize::new(0));
    let alpha_calls = Arc::new(AtomicUsize::new(0));
    let beta_calls = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(McpRuntime::from_test_clients(vec![
        client("alpha", Arc::clone(&alpha_calls)),
        client("beta", Arc::clone(&beta_calls)),
    ]));
    let parent_context = Arc::new(ToolContext::empty());
    let source = source_generation(
        Arc::clone(&parent_context),
        Arc::clone(&stable_calls),
        runtime.as_ref(),
        0,
    )?;
    parent_context.insert_extension(Arc::new(McpRuntimeStore::new(source, Arc::clone(&runtime))));
    let parent_registry = Arc::new(ToolRegistry::new());
    let child = child_tool_snapshot(
        &parent_context,
        &parent_registry,
        None,
        Some(vec!["beta".to_owned()]),
        Arc::new(ToolContext::empty()),
    )?;
    let alpha = runtime.tool_names_for_servers(&["alpha".to_owned()])?[0].clone();
    let beta = runtime.tool_names_for_servers(&["beta".to_owned()])?[0].clone();
    let child_names = names(&child);
    assert!(child_names.contains(&"stable_probe".to_owned()));
    assert!(child_names.contains(&beta));
    assert!(!child_names.contains(&alpha));

    child
        .executor
        .execute("stable_probe", "stable", serde_json::json!({}))
        .await?;
    child
        .executor
        .execute(&beta, "beta", serde_json::json!({}))
        .await?;
    assert_eq!(stable_calls.load(Ordering::SeqCst), 1);
    assert_eq!(alpha_calls.load(Ordering::SeqCst), 0);
    assert_eq!(beta_calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn new_child_observes_replaced_pool_while_existing_child_keeps_lease()
-> Result<(), Box<dyn std::error::Error>> {
    let stable_calls = Arc::new(AtomicUsize::new(0));
    let alpha_calls = Arc::new(AtomicUsize::new(0));
    let beta_calls = Arc::new(AtomicUsize::new(0));
    let first_runtime = Arc::new(McpRuntime::from_test_clients(vec![client(
        "alpha",
        Arc::clone(&alpha_calls),
    )]));
    let parent_context = Arc::new(ToolContext::empty());
    let first_generation = source_generation(
        Arc::clone(&parent_context),
        Arc::clone(&stable_calls),
        first_runtime.as_ref(),
        0,
    )?;
    let store = Arc::new(McpRuntimeStore::new(
        first_generation,
        Arc::clone(&first_runtime),
    ));
    parent_context.insert_extension(Arc::clone(&store));
    let first_child = child_tool_snapshot(
        &parent_context,
        &Arc::new(ToolRegistry::new()),
        None,
        None,
        Arc::new(ToolContext::empty()),
    )?;
    let first_lease = first_child
        .executor
        .execution_snapshot()
        .ok_or("live child did not provide a request lease")?;

    let second_runtime = Arc::new(McpRuntime::from_test_clients(vec![client(
        "beta",
        Arc::clone(&beta_calls),
    )]));
    let second_generation = source_generation(
        Arc::clone(&parent_context),
        Arc::clone(&stable_calls),
        second_runtime.as_ref(),
        1,
    )?;
    store.replace(second_generation, Arc::clone(&second_runtime));
    let second_child = child_tool_snapshot(
        &parent_context,
        &Arc::new(ToolRegistry::new()),
        None,
        None,
        Arc::new(ToolContext::empty()),
    )?;
    let alpha = first_runtime.tool_names_for_servers(&["alpha".to_owned()])?[0].clone();
    let beta = second_runtime.tool_names_for_servers(&["beta".to_owned()])?[0].clone();
    let second_lease = first_child
        .executor
        .execution_snapshot()
        .ok_or("live child did not refresh its request lease")?;
    let lease_names = |lease: &crate::r#loop::config::ToolExecutionSnapshot| {
        lease
            .definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };

    assert!(lease_names(&first_lease).contains(&alpha));
    assert!(!lease_names(&first_lease).contains(&beta));
    assert!(lease_names(&second_lease).contains(&beta));
    assert!(!lease_names(&second_lease).contains(&alpha));
    assert!(names(&second_child).contains(&beta));
    assert!(!names(&second_child).contains(&alpha));
    first_lease
        .executor
        .execute(&alpha, "old-lease", serde_json::json!({}))
        .await?;
    second_lease
        .executor
        .execute(&beta, "new-lease", serde_json::json!({}))
        .await?;
    assert_eq!(alpha_calls.load(Ordering::SeqCst), 1);
    assert_eq!(beta_calls.load(Ordering::SeqCst), 1);
    Ok(())
}
