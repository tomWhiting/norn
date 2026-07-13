use async_trait::async_trait;

use super::*;
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::integration::{McpClient, McpToolDef};
use crate::tool::registry::ToolRegistry;

struct DormantTransport;

#[async_trait]
impl Transport for DormantTransport {
    async fn request(
        &self,
        _payload: String,
        _request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Err(IntegrationError::McpError {
            reason: "dormant test transport was invoked".to_owned(),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

pub(crate) fn runtime_with_servers(names: &[&str]) -> McpRuntime {
    let clients = names
        .iter()
        .map(|name| {
            McpClient::from_transport(*name, Box::new(DormantTransport)).with_test_tools(vec![
                McpToolDef {
                    name: "echo".to_owned(),
                    description: "fixture".to_owned(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
            ])
        })
        .collect();
    McpRuntime::from_test_clients(clients)
}

#[test]
fn registration_rejects_a_provider_name_already_in_the_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime_with_servers(&["alpha"]);
    let mut registry = ToolRegistry::new();
    assert_eq!(runtime.register_tools(&mut registry)?, 1);

    let second = runtime.register_tools(&mut registry);

    assert!(matches!(second, Err(IntegrationError::McpError { .. })));
    Ok(())
}
