use std::error::Error;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::McpRuntimeCandidateBuilder;
use crate::config::mcp::fingerprint;
use crate::config::{EffectiveMcpServer, McpConfigLayer, McpServerSettings};
use crate::error::{IntegrationError, ToolError};
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::integration::mcp_runtime::McpRuntimeServerState;
use crate::integration::{
    McpActivationRequest, McpCandidateBuilder, McpClient, McpRuntime, McpToolDef,
};
use crate::r#loop::config::ToolExecutor;
use crate::tool::{
    Tool, ToolContext, ToolEffect, ToolEnvelope, ToolGeneration, ToolGenerationStore, ToolOutput,
    ToolRegistry,
};

type TestResult = Result<(), Box<dyn Error>>;

struct DormantTransport;

#[async_trait]
impl Transport for DormantTransport {
    async fn request(
        &self,
        _payload: String,
        _request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Err(IntegrationError::McpError {
            reason: "dormant candidate transport was invoked".to_owned(),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

struct FixtureTool {
    name: String,
    dynamic: bool,
}

#[async_trait]
impl Tool for FixtureTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &'static str {
        "candidate builder fixture"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn runtime_dynamic(&self) -> bool {
        self.dynamic
    }

    async fn execute(
        &self,
        _envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::success(json!({"name": self.name})))
    }
}

fn effective(name: &str) -> Result<EffectiveMcpServer, crate::error::ConfigError> {
    let definition = McpServerSettings {
        command: Some(format!("{name}-server")),
        ..McpServerSettings::default()
    };
    let identity = fingerprint(name, &definition)?;
    Ok(EffectiveMcpServer::new(
        name.to_owned(),
        McpConfigLayer::User,
        definition,
        identity,
    ))
}

fn client(name: &str, tool_names: &[&str]) -> McpClient {
    let tools = tool_names
        .iter()
        .map(|tool_name| McpToolDef {
            name: (*tool_name).to_owned(),
            description: "candidate proxy fixture".to_owned(),
            input_schema: json!({"type": "object"}),
        })
        .collect();
    McpClient::from_transport(name, Box::new(DormantTransport)).with_test_tools(tools)
}

fn generation(stable_names: &[&str], dynamic_names: &[&str]) -> Arc<ToolGeneration> {
    let mut registry = ToolRegistry::new();
    for name in stable_names {
        registry.register(Box::new(FixtureTool {
            name: (*name).to_owned(),
            dynamic: false,
        }));
    }
    for name in dynamic_names {
        registry.register(Box::new(FixtureTool {
            name: (*name).to_owned(),
            dynamic: true,
        }));
    }
    Arc::new(ToolGeneration::from_registry(&registry, 6))
}

#[tokio::test]
async fn selected_view_rebuilds_tools_and_retains_full_runtime_status() -> TestResult {
    let alpha = effective("alpha")?;
    let beta = effective("beta")?;
    let runtime = Arc::new(McpRuntime::from_test_connected_servers(vec![
        (alpha.clone(), client("alpha", &["echo"])),
        (beta.clone(), client("beta", &["lookup"])),
    ]));
    let previous = generation(&["stable"], &["mcp_old"]);
    let context = previous.context();
    let alpha_name = runtime.tool_names_for_servers(&["alpha".to_owned()])?[0].clone();
    let beta_name = runtime.tool_names_for_servers(&["beta".to_owned()])?[0].clone();
    let builder = McpRuntimeCandidateBuilder::new("/project".into())
        .with_selected_servers(vec!["alpha".to_owned()]);

    let candidate = builder
        .build(McpActivationRequest::new(
            7,
            Arc::clone(&previous),
            runtime,
            Arc::from([alpha.clone(), beta.clone()]),
        ))
        .await?;
    let next = candidate.generation();
    let paired_runtime = candidate.runtime();

    assert_eq!(next.revision(), 7);
    assert!(Arc::ptr_eq(&context, &next.context()));
    assert!(next.names().any(|name| name == "stable"));
    assert!(next.names().any(|name| name == alpha_name));
    assert!(!next.names().any(|name| name == beta_name));
    assert!(!next.names().any(|name| name == "mcp_old"));
    assert_eq!(paired_runtime.len(), 2);
    for server in [&alpha, &beta] {
        let status = paired_runtime
            .server_status(server.name())
            .ok_or_else(|| std::io::Error::other("missing paired runtime status"))?;
        assert_eq!(status.state(), McpRuntimeServerState::Connected);
        assert_eq!(status.fingerprint(), server.fingerprint());
    }

    let store = ToolGenerationStore::new(Arc::clone(&previous));
    let old_lease = store
        .execution_snapshot()
        .ok_or_else(|| std::io::Error::other("missing old execution lease"))?;
    store.publish(next)?;
    assert_eq!(
        old_lease
            .executor
            .execute("mcp_old", "old-call", json!({}))
            .await?["name"],
        "mcp_old"
    );
    Ok(())
}

#[tokio::test]
async fn candidate_rejects_stable_and_duplicate_provider_names() -> TestResult {
    let server = effective("alpha")?;
    let runtime = Arc::new(McpRuntime::from_test_connected_servers(vec![(
        server.clone(),
        client("alpha", &["echo"]),
    )]));
    let provider_name = runtime.tool_names()[0].clone();
    let collision = McpRuntimeCandidateBuilder::new("/project".into())
        .build(McpActivationRequest::new(
            7,
            generation(&[provider_name.as_str()], &[]),
            Arc::clone(&runtime),
            Arc::from([server.clone()]),
        ))
        .await;
    assert!(collision.is_err());

    let duplicate_runtime = Arc::new(McpRuntime::from_test_connected_servers(vec![(
        server.clone(),
        client("alpha", &["echo", "echo"]),
    )]));
    let duplicate = McpRuntimeCandidateBuilder::new("/project".into())
        .build(McpActivationRequest::new(
            7,
            generation(&[], &[]),
            duplicate_runtime,
            Arc::from([server]),
        ))
        .await;
    assert!(duplicate.is_err());
    Ok(())
}

#[tokio::test]
async fn candidate_rejects_unknown_selected_server() -> TestResult {
    let server = effective("alpha")?;
    let runtime = Arc::new(McpRuntime::from_test_connected_servers(vec![(
        server.clone(),
        client("alpha", &["echo"]),
    )]));
    let result = McpRuntimeCandidateBuilder::new("/project".into())
        .with_selected_servers(vec!["missing".to_owned()])
        .build(McpActivationRequest::new(
            7,
            generation(&[], &[]),
            runtime,
            Arc::from([server]),
        ))
        .await;

    assert!(result.is_err());
    Ok(())
}
