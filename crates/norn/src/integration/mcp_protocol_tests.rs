use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;

#[test]
fn roots_require_absolute_uris_and_redact_debug_output() -> Result<(), TestError> {
    let root = McpRoot::new(
        "file:///workspace/ROOT_URI_SENTINEL",
        Some("ROOT_NAME_SENTINEL".to_owned()),
    )?;
    let rendered = format!("{root:?}");

    assert!(!rendered.contains("ROOT_URI_SENTINEL"));
    assert!(!rendered.contains("ROOT_NAME_SENTINEL"));
    assert!(McpRoot::new("relative/path", None).is_err());
    Ok(())
}

#[test]
fn hostile_server_message_version_is_rejected_without_payload_disclosure() -> Result<(), TestError>
{
    let state = ClientProtocolState::new(Vec::new());
    let error = state
        .inspect(&serde_json::json!({
            "jsonrpc": "1.0",
            "id": "HOSTILE_ID_SENTINEL",
            "method": "roots/list",
            "params": {"secret": "PAYLOAD_SENTINEL"}
        }))
        .err()
        .ok_or("hostile server request was accepted")?;
    let rendered = error.to_string();

    assert!(!rendered.contains("HOSTILE_ID_SENTINEL"));
    assert!(!rendered.contains("PAYLOAD_SENTINEL"));
    assert!(rendered.contains("JSON-RPC 2.0"));
    Ok(())
}

#[test]
fn compound_server_request_ids_are_rejected() -> Result<(), TestError> {
    let state = ClientProtocolState::new(Vec::new());
    let error = state
        .inspect(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": {"nested": "ID_PAYLOAD_SENTINEL"},
            "method": "roots/list"
        }))
        .err()
        .ok_or("compound server request id was accepted")?;

    assert!(error.to_string().contains("invalid JSON-RPC id"));
    assert!(!error.to_string().contains("ID_PAYLOAD_SENTINEL"));
    Ok(())
}

#[test]
fn tool_change_notifications_advance_a_monotonic_watch_revision() -> Result<(), TestError> {
    let state = ClientProtocolState::new(Vec::new());
    let receiver = state.subscribe_tool_list_changes();
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed"
    });

    assert!(matches!(
        state.inspect(&notification)?,
        InboundMessage::Consumed
    ));
    assert_eq!(*receiver.borrow(), 1);
    assert!(matches!(
        state.inspect(&notification)?,
        InboundMessage::Consumed
    ));
    assert_eq!(*receiver.borrow(), 2);
    Ok(())
}
