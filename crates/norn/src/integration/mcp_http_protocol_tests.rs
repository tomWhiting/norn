use std::collections::HashMap;

use crate::integration::{
    DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient, McpClientConfig, McpRoot, McpTransport,
};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn sse_answers_roots_observes_tools_change_and_emits_root_change() -> Result<(), TestError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "initialize"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fixture", "version": "1"}
                    }
                }))
                .insert_header("mcp-session-id", "protocol-session"),
        )
        .mount(&server)
        .await;
    mount_empty(&server, "notifications/initialized").await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "tools/list"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"jsonrpc\":\"2.0\",\"id\":\"roots-http\",\"method\":\"roots/list\"}\n\n",
                "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n",
            ),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({"id": "roots-http"})))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    mount_empty(&server, "notifications/roots/list_changed").await;

    let root = McpRoot::new(
        "file:///workspace/http-initial",
        Some("workspace".to_owned()),
    )?;
    let client = McpClient::connect_with_roots(config(&server), vec![root]).await?;
    let revision = client.subscribe_tool_list_changes();
    assert_eq!(*revision.borrow(), 1);
    assert!(
        client
            .set_roots(vec![McpRoot::new("file:///workspace/http-next", None)?])
            .await?
    );

    let requests = server
        .received_requests()
        .await
        .ok_or("wiremock request recording unavailable")?;
    let roots_reply = requests
        .iter()
        .find_map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
            (body.get("id") == Some(&serde_json::json!("roots-http"))).then_some(body)
        })
        .ok_or("roots/list response was not posted")?;
    assert_eq!(
        roots_reply.pointer("/result/roots/0/uri"),
        Some(&serde_json::json!("file:///workspace/http-initial"))
    );
    Ok(())
}

fn config(server: &MockServer) -> McpClientConfig {
    McpClientConfig {
        name: "http-protocol".to_owned(),
        transport: McpTransport::Http { url: server.uri() },
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    }
}

async fn mount_empty(server: &MockServer, method_name: &'static str) {
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": method_name}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .mount(server)
        .await;
}
