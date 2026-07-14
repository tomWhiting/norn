use std::collections::HashMap;
use std::time::Duration;

use crate::integration::{
    DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient, McpClientConfig, McpRoot, McpTransport,
};

type TestError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(unix)]
#[tokio::test]
async fn pump_answers_roots_observes_tools_change_and_emits_root_change() -> Result<(), TestError> {
    let temp = tempfile::tempdir()?;
    let script = concat!(
        "read initialize; printf '%s' \"$initialize\" > initialize.json; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"roots-1\",\"method\":\"roots/list\"}'; ",
        "read roots; printf '%s' \"$roots\" > roots.json; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}'; ",
        "read initialized; ",
        "read list; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}'; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}'; ",
        "read changed; printf '%s' \"$changed\" > changed.json; ",
        "sleep 1"
    );
    let initial = McpRoot::new("file:///workspace/initial", Some("initial".to_owned()))?;
    let client = McpClient::connect_with_roots(
        McpClientConfig {
            name: "stdio-protocol".to_owned(),
            transport: McpTransport::Stdio {
                command: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), script.to_owned()],
            },
            env: HashMap::new(),
            headers: HashMap::new(),
            working_dir: Some(temp.path().to_path_buf()),
            max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
            request_timeout_ms: None,
        },
        vec![initial],
    )
    .await?;
    let revision = client.subscribe_tool_list_changes();
    assert_eq!(*revision.borrow(), 1);

    let replacement = McpRoot::new("file:///workspace/replacement", None)?;
    assert!(client.set_roots(vec![replacement]).await?);
    wait_for_file(&temp.path().join("changed.json")).await?;

    let initialize: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        temp.path().join("initialize.json"),
    )?)?;
    assert_eq!(
        initialize.pointer("/params/capabilities/roots/listChanged"),
        Some(&serde_json::json!(true))
    );
    let roots: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(temp.path().join("roots.json"))?)?;
    assert_eq!(
        roots.pointer("/result/roots/0/uri"),
        Some(&serde_json::json!("file:///workspace/initial"))
    );
    let changed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(temp.path().join("changed.json"))?)?;
    assert_eq!(
        changed.get("method"),
        Some(&serde_json::json!("notifications/roots/list_changed"))
    );
    Ok(())
}

async fn wait_for_file(path: &std::path::Path) -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}
