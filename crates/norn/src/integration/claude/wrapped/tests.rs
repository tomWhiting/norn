#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::wildcard_enum_match_arm
)]

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;

use claude_runner::ClaudeEvent;
#[cfg(unix)]
use claude_runner::control::{
    SDKInitializeOptions, UserContentBlock, UserMessage, UserMessageContent,
};
use claude_runner::events::{ClaudeMessage, ContentItem, SystemEventData};

#[cfg(unix)]
use crate::resource::{DescriptorGovernor, TWO_PIPE_SPAWN_PEAK};

#[cfg(unix)]
use super::NornWrappedClaudeSession;
use super::legacy::validate_terminal_result;
use super::{NornWrappedClaudeCode, NornWrappedClaudeConfig, NornWrappedClaudeError};

fn wrapper_config(claude_code_path: PathBuf) -> NornWrappedClaudeConfig {
    NornWrappedClaudeConfig {
        claude_code_path,
        norn_tools: vec!["read".to_owned(), "write".to_owned()],
        system_prompt: "you are norn-wrapped".to_owned(),
        mcp_server_address:
            r#""/opt/Norn MCP/bin/server" --mode "stdio fast" 'literal value' path\ with\ spaces"#
                .to_owned(),
    }
}

fn argument_value<'a>(arguments: &'a [String], flag: &str) -> Option<&'a str> {
    arguments
        .iter()
        .position(|argument| argument == flag)
        .and_then(|index| arguments.get(index + 1))
        .map(String::as_str)
}

#[test]
fn sdk_command_is_minimal_strict_and_parses_mcp_command_arguments() {
    let wrapper =
        NornWrappedClaudeCode::new(wrapper_config(PathBuf::from("/usr/local/bin/claude")));
    let command = wrapper.config.build_sdk_command().unwrap();
    let arguments = command.build_args();

    assert!(command.is_sdk_mode());
    assert_eq!(argument_value(&arguments, "--tools"), Some(""));
    assert_eq!(argument_value(&arguments, "--setting-sources"), Some(""));
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--strict-mcp-config")
    );
    assert_eq!(
        argument_value(&arguments, "--system-prompt"),
        Some("you are norn-wrapped")
    );
    assert_eq!(
        argument_value(&arguments, "--allowedTools"),
        Some("mcp__norn__read,mcp__norn__write")
    );

    let mcp: serde_json::Value =
        serde_json::from_str(argument_value(&arguments, "--mcp-config").unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["norn"]["command"],
        "/opt/Norn MCP/bin/server"
    );
    assert_eq!(
        mcp["mcpServers"]["norn"]["args"],
        serde_json::json!(["--mode", "stdio fast", "literal value", "path with spaces"])
    );
}

#[test]
fn legacy_command_uses_the_same_non_transport_isolation() {
    let wrapper =
        NornWrappedClaudeCode::new(wrapper_config(PathBuf::from("/usr/local/bin/claude")));
    let command = wrapper.build_command("task").unwrap();
    let arguments = command.build_args();

    assert!(!command.is_sdk_mode());
    assert_eq!(argument_value(&arguments, "--tools"), Some(""));
    assert_eq!(argument_value(&arguments, "--setting-sources"), Some(""));
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--disable-slash-commands")
    );
    assert!(arguments.iter().any(|argument| argument == "--settings"));
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--strict-mcp-config")
    );
    for name in [
        "CLAUDE_CODE_DISABLE_AGENT_VIEW",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
        "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS",
        "CLAUDE_CODE_DISABLE_BUNDLED_SKILLS",
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
        "CLAUDE_CODE_DISABLE_CRON",
    ] {
        assert_eq!(command.get_env().get(name).map(String::as_str), Some("1"));
    }
}

#[test]
fn empty_tool_list_auto_approves_only_the_strict_norn_server_namespace() {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.norn_tools.clear();
    let wrapper = NornWrappedClaudeCode::new(config);
    let arguments = wrapper.config.build_sdk_command().unwrap().build_args();

    assert_eq!(
        argument_value(&arguments, "--allowedTools"),
        Some("mcp__norn__*")
    );
}

#[test]
fn malformed_mcp_command_and_tool_names_fail_closed() {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.mcp_server_address = "'unterminated".to_owned();
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .unwrap_err(),
        NornWrappedClaudeError::UnterminatedMcpSingleQuote
    ));

    let mut config = wrapper_config(PathBuf::from("claude"));
    config.norn_tools = vec!["read,write".to_owned()];
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .unwrap_err(),
        NornWrappedClaudeError::InvalidNornToolName { index: 0, .. }
    ));

    let mut config = wrapper_config(PathBuf::from("claude"));
    config.norn_tools = vec!["*".to_owned()];
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .unwrap_err(),
        NornWrappedClaudeError::InvalidNornToolName { index: 0, .. }
    ));

    let mut config = wrapper_config(PathBuf::from("claude"));
    config.mcp_server_address = "''".to_owned();
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .unwrap_err(),
        NornWrappedClaudeError::EmptyMcpCommand
    ));
}

#[test]
fn shell_line_continuations_do_not_create_mcp_argument_bytes() {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.mcp_server_address = "norn-mcp --label foo\\\nbar".to_owned();
    let arguments = config.build_sdk_command().unwrap().build_args();
    let mcp: serde_json::Value =
        serde_json::from_str(argument_value(&arguments, "--mcp-config").unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["norn"]["args"],
        serde_json::json!(["--label", "foobar"])
    );
}

#[test]
fn capture_session_events_maps_assistant_and_result() {
    let wrapper =
        NornWrappedClaudeCode::new(wrapper_config(PathBuf::from("/usr/local/bin/claude")));
    let claude_events = vec![
        ClaudeEvent::System(Box::new(SystemEventData {
            subtype: Some("init".to_owned()),
            session_id: Some("claude-session-1".to_owned()),
            ..Default::default()
        })),
        ClaudeEvent::Assistant {
            message: ClaudeMessage {
                id: Some("m1".to_owned()),
                message_type: Some("message".to_owned()),
                role: "assistant".to_owned(),
                model: None,
                content: vec![ContentItem::Text {
                    text: "ok".to_owned(),
                }],
                stop_reason: Some("end_turn".to_owned()),
                usage: None,
            },
            session_id: Some("claude-session-1".to_owned()),
            parent_tool_use_id: None,
            uuid: None,
        },
        ClaudeEvent::Result {
            subtype: Some("success".to_owned()),
            is_error: Some(false),
            duration_ms: None,
            duration_api_ms: None,
            result: Some(serde_json::json!({"final": true})),
            error: None,
            num_turns: None,
            session_id: Some("claude-session-1".to_owned()),
            structured_output: None,
            stop_reason: Some("end_turn".to_owned()),
            total_cost_usd: None,
            usage: None,
            sdk_metadata: Box::default(),
        },
    ];

    let events = wrapper.capture_session_events(&claude_events);
    assert!(events.iter().any(|event| matches!(
        event,
        crate::session::events::SessionEvent::AssistantMessage { content, .. }
            if content == "ok"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        crate::session::events::SessionEvent::Custom { event_type, .. }
            if event_type == "claude_code.result"
    )));
    assert_eq!(
        wrapper.claude_session_id().as_deref(),
        Some("claude-session-1")
    );

    let second_capture =
        wrapper.capture_session_events(&[ClaudeEvent::System(Box::new(SystemEventData {
            subtype: Some("init".to_owned()),
            session_id: Some("claude-session-2".to_owned()),
            ..Default::default()
        }))]);
    assert!(second_capture.is_empty());
    assert_eq!(
        wrapper.claude_session_id().as_deref(),
        Some("claude-session-2")
    );
}

#[test]
fn legacy_terminal_results_fail_closed() {
    let failed = ClaudeEvent::Result {
        subtype: Some("error_during_execution".to_owned()),
        is_error: Some(true),
        duration_ms: None,
        duration_api_ms: None,
        result: None,
        error: Some("permission denied".to_owned()),
        num_turns: None,
        session_id: None,
        structured_output: None,
        stop_reason: None,
        total_cost_usd: None,
        usage: None,
        sdk_metadata: Box::default(),
    };
    assert!(validate_terminal_result(&[failed]).is_err());
    let ambiguous = ClaudeEvent::Result {
        subtype: None,
        is_error: None,
        duration_ms: None,
        duration_api_ms: None,
        result: None,
        error: None,
        num_turns: None,
        session_id: None,
        structured_output: None,
        stop_reason: None,
        total_cost_usd: None,
        usage: None,
        sdk_metadata: Box::default(),
    };
    assert!(validate_terminal_result(&[ambiguous]).is_err());
    assert!(validate_terminal_result(&[]).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn persistent_session_initializes_sends_interrupts_and_reaps() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("fake-claude");
    write_fake_claude(&executable);
    let mut config = wrapper_config(executable);
    config.mcp_server_address = "norn-mcp --stdio".to_owned();
    let wrapper = NornWrappedClaudeCode::new(config);
    let governor = Arc::new(DescriptorGovernor::with_capacity(TWO_PIPE_SPAWN_PEAK));
    let available_before_spawn = governor.available();

    let mut session = NornWrappedClaudeSession::spawn_with_governor(
        &wrapper.config,
        SDKInitializeOptions {
            title: Some("Norn test".to_owned()),
            ..SDKInitializeOptions::default()
        },
        Arc::clone(&governor),
    )
    .await
    .unwrap();
    assert!(session.id() > 0);
    assert_eq!(session.retained_descriptor_count(), 2);
    assert_eq!(
        governor.available(),
        available_before_spawn - session.retained_descriptor_count()
    );
    assert_eq!(
        session
            .initialization_result()
            .unwrap()
            .unwrap()
            .output_style,
        "norn-test"
    );

    session.send_user_message("hello").unwrap();
    let first = session.next_event().await.unwrap().unwrap();
    assert!(matches!(first, ClaudeEvent::System(_)));
    assert_eq!(session.claude_session_id(), Some("fake-session"));
    let second = session.next_event().await.unwrap().unwrap();
    assert!(matches!(
        second,
        ClaudeEvent::Assistant { ref message, .. }
            if message.text_items().collect::<Vec<_>>() == ["echo:hello"]
    ));

    let rich_message =
        UserMessage::from_content(UserMessageContent::blocks(vec![UserContentBlock::text(
            "rich",
        )]))
        .with_generated_uuid();
    let control = session.control_handle();
    assert_eq!(control.id(), session.id());
    session.send_typed_user_message(&rich_message).unwrap();
    let interrupt = tokio::spawn(async move { control.interrupt().await });
    let third = session.next_event().await.unwrap().unwrap();
    assert!(matches!(
        third,
        ClaudeEvent::Assistant { ref message, .. }
            if message.text_items().collect::<Vec<_>>() == ["echo:rich"]
    ));
    assert!(interrupt.await.unwrap().unwrap().is_none());

    let control = session.control_handle();
    control.close_stdin().unwrap();
    assert!(session.next_event().await.unwrap().is_none());
    let observed_status = control.wait_for_exit().await.unwrap();
    let status = session.wait().await.unwrap();
    assert_eq!(status, observed_status);
    assert!(status.success());
    assert_eq!(governor.available(), available_before_spawn);
}

#[cfg(unix)]
fn write_fake_claude(path: &Path) {
    let source = r#"#!/usr/bin/env python3
import json
import sys

print(json.dumps({
    "type": "system",
    "subtype": "init",
    "session_id": "fake-session"
}), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    if message["type"] == "control_request":
        subtype = message["request"]["subtype"]
        if subtype == "initialize":
            assert message["request"]["systemPrompt"] == ["you are norn-wrapped"]
            payload = {
                "commands": [],
                "agents": [],
                "output_style": "norn-test",
                "available_output_styles": [],
                "models": [],
                "account": {}
            }
        else:
            payload = None
        print(json.dumps({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": message["request_id"],
                "response": payload
            }
        }), flush=True)
    elif message["type"] == "user":
        content = message["message"]["content"]
        if isinstance(content, list):
            content = "|".join(
                block["text"] for block in content if block["type"] == "text"
            )
        print(json.dumps({
            "type": "assistant",
            "session_id": "fake-session",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "echo:" + content}]
            }
        }), flush=True)
"#;
    fs::write(path, source).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).unwrap();
}
