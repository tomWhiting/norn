use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio::sync::Notify;

use super::mcp_client::{JsonRpcResponse, Transport};
use super::{McpClient, McpRoot, McpToolDef};
use crate::error::IntegrationError;
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::Tool;

struct BlockingTransport {
    notifications: AtomicUsize,
    request_started: Notify,
    release_request: Notify,
}

#[async_trait]
impl Transport for BlockingTransport {
    async fn request(
        &self,
        _payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        self.request_started.notify_one();
        self.release_request.notified().await;
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
        self.notifications.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

struct FailingNotificationTransport;

#[async_trait]
impl Transport for FailingNotificationTransport {
    async fn request(
        &self,
        _payload: String,
        _request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Err(IntegrationError::McpError {
            reason: "unexpected request".to_owned(),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Err(IntegrationError::McpError {
            reason: "notification failed".to_owned(),
        })
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

#[tokio::test]
async fn public_root_update_cannot_split_contextual_tool_call()
-> Result<(), Box<dyn std::error::Error>> {
    let transport = Arc::new(BlockingTransport {
        notifications: AtomicUsize::new(0),
        request_started: Notify::new(),
        release_request: Notify::new(),
    });
    let client = Arc::new(
        McpClient::from_transport("shared", Box::new(ArcTransport(Arc::clone(&transport))))
            .with_test_tools(vec![McpToolDef {
                name: "echo".to_owned(),
                description: "echo".to_owned(),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
    );
    let tool: Arc<dyn Tool + Send + Sync> = Arc::from(
        client
            .proxy_tools()
            .into_iter()
            .next()
            .ok_or("proxy tool fixture was not registered")?,
    );
    let temp = tempfile::tempdir()?;
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    std::fs::create_dir(&first)?;
    std::fs::create_dir(&second)?;
    let first_context = ToolContext::with_working_dir(SharedWorkingDir::new(first));
    let envelope = ToolEnvelope {
        tool_call_id: "call".to_owned(),
        tool_name: tool.name().to_owned(),
        model_args: serde_json::json!({}),
        metadata: serde_json::Value::Null,
    };
    let call = tokio::spawn(async move { tool.execute(&envelope, &first_context).await });
    transport.request_started.notified().await;

    let second_root = McpRoot::from_path(&second)?;
    let update_client = Arc::clone(&client);
    let update = tokio::spawn(async move { update_client.set_roots(vec![second_root]).await });
    tokio::task::yield_now().await;
    assert_eq!(transport.notifications.load(Ordering::SeqCst), 1);
    assert!(!update.is_finished());

    transport.release_request.notify_one();
    call.await??;
    assert!(update.await??);
    assert_eq!(transport.notifications.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn failed_root_notification_restores_previous_local_view()
-> Result<(), Box<dyn std::error::Error>> {
    let client = McpClient::from_transport("shared", Box::new(FailingNotificationTransport));
    let root = McpRoot::new("file:///private/example", Some("example".to_owned()))?;

    assert!(client.set_roots(vec![root]).await.is_err());
    assert!(client.roots()?.is_empty());
    Ok(())
}

struct ArcTransport(Arc<BlockingTransport>);

#[async_trait]
impl Transport for ArcTransport {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        self.0.request(payload, request_id).await
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        self.0.notify(payload).await
    }

    fn supports_protocol_version(&self, version: &str) -> bool {
        self.0.supports_protocol_version(version)
    }
}
