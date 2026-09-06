//! Configuration, event projection and same-process SDK lifecycle regressions.

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
use super::execution::validate_terminal_result;
use super::{NornWrappedClaudeCode, NornWrappedClaudeConfig, NornWrappedClaudeError};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn wrapper_config(claude_code_path: PathBuf) -> NornWrappedClaudeConfig {
    NornWrappedClaudeConfig {
        claude_code_path,
        norn_tools: vec!["read".to_owned(), "write".to_owned()],
        system_prompt: "you are norn-wrapped".to_owned(),
        model: Some("claude-opus-5".to_owned()),
        reasoning_effort: Some(crate::provider::request::ReasoningEffort::Max),
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
fn sdk_command_is_minimal_strict_and_parses_mcp_command_arguments() -> TestResult {
    let wrapper =
        NornWrappedClaudeCode::new(wrapper_config(PathBuf::from("/usr/local/bin/claude")));
    let command = wrapper.config.build_sdk_command()?;
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
    assert_eq!(argument_value(&arguments, "--model"), Some("claude-opus-5"));
    assert_eq!(argument_value(&arguments, "--effort"), Some("max"));

    let mcp: serde_json::Value = serde_json::from_str(
        argument_value(&arguments, "--mcp-config").ok_or("command omitted --mcp-config")?,
    )?;
    assert_eq!(
        mcp["mcpServers"]["norn"]["command"],
        "/opt/Norn MCP/bin/server"
    );
    assert_eq!(
        mcp["mcpServers"]["norn"]["args"],
        serde_json::json!(["--mode", "stdio fast", "literal value", "path with spaces"])
    );
    Ok(())
}

#[test]
fn explicit_none_effort_is_rejected_before_spawn() {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.reasoning_effort = Some(crate::provider::request::ReasoningEffort::None);
    assert!(matches!(
        config.build_sdk_command(),
        Err(NornWrappedClaudeError::UnsupportedReasoningEffort { effort: "none" })
    ));
}

#[test]
fn oneshot_command_uses_the_same_non_transport_isolation() -> TestResult {
    let wrapper =
        NornWrappedClaudeCode::new(wrapper_config(PathBuf::from("/usr/local/bin/claude")));
    let command = wrapper.build_command("task")?;
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
    Ok(())
}

#[test]
fn empty_tool_list_auto_approves_only_the_strict_norn_server_namespace() -> TestResult {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.norn_tools.clear();
    let wrapper = NornWrappedClaudeCode::new(config);
    let arguments = wrapper.config.build_sdk_command()?.build_args();

    assert_eq!(
        argument_value(&arguments, "--allowedTools"),
        Some("mcp__norn__*")
    );
    Ok(())
}

#[test]
fn malformed_mcp_command_and_tool_names_fail_closed() -> TestResult {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.mcp_server_address = "'unterminated".to_owned();
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .err()
            .ok_or("invalid SDK configuration unexpectedly succeeded")?,
        NornWrappedClaudeError::UnterminatedMcpSingleQuote
    ));

    let mut config = wrapper_config(PathBuf::from("claude"));
    config.norn_tools = vec!["read,write".to_owned()];
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .err()
            .ok_or("invalid SDK configuration unexpectedly succeeded")?,
        NornWrappedClaudeError::InvalidNornToolName { index: 0, .. }
    ));

    let mut config = wrapper_config(PathBuf::from("claude"));
    config.norn_tools = vec!["*".to_owned()];
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .err()
            .ok_or("invalid SDK configuration unexpectedly succeeded")?,
        NornWrappedClaudeError::InvalidNornToolName { index: 0, .. }
    ));

    let mut config = wrapper_config(PathBuf::from("claude"));
    config.mcp_server_address = "''".to_owned();
    assert!(matches!(
        NornWrappedClaudeCode::new(config)
            .config
            .build_sdk_command()
            .err()
            .ok_or("invalid SDK configuration unexpectedly succeeded")?,
        NornWrappedClaudeError::EmptyMcpCommand
    ));
    Ok(())
}

#[test]
fn shell_line_continuations_do_not_create_mcp_argument_bytes() -> TestResult {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.mcp_server_address = "norn-mcp --label foo\\\nbar".to_owned();
    let arguments = config.build_sdk_command()?.build_args();
    let mcp: serde_json::Value = serde_json::from_str(
        argument_value(&arguments, "--mcp-config").ok_or("command omitted --mcp-config")?,
    )?;
    assert_eq!(
        mcp["mcpServers"]["norn"]["args"],
        serde_json::json!(["--label", "foobar"])
    );
    Ok(())
}

#[test]
fn capture_session_events_maps_assistant_and_result() -> TestResult {
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

    let events = wrapper.capture_session_events(&claude_events)?;
    assert!(events.iter().any(|event| matches!(
        event,
        crate::session::events::SessionEvent::AssistantMessage { content, .. }
            if content == "ok"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        crate::session::events::SessionEvent::Custom { event_type, data, .. }
            if event_type == "claude_code.result" && data["result"] == serde_json::json!({"final": true})
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
        }))])?;
    assert!(second_capture.is_empty());
    assert_eq!(
        wrapper.claude_session_id().as_deref(),
        Some("claude-session-2")
    );
    Ok(())
}

#[test]
fn oneshot_terminal_results_fail_closed() {
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
async fn persistent_session_initializes_sends_interrupts_and_reaps() -> TestResult {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("fake-claude");
    write_fake_claude(&executable)?;
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
    .await?;
    let process_id = session.id();
    assert!(process_id > 0);
    assert_eq!(session.retained_descriptor_count(), 2);
    assert_eq!(
        governor.available(),
        available_before_spawn - session.retained_descriptor_count()
    );
    assert_eq!(
        session
            .initialization_result()?
            .ok_or("SDK initialization result was not retained")?
            .output_style,
        "norn-test"
    );

    session.send_user_message("hello")?;
    let first = session
        .next_event()
        .await?
        .ok_or("SDK stream ended before its expected event")?;
    assert!(matches!(first, ClaudeEvent::System(_)));
    assert_eq!(session.claude_session_id(), Some("fake-session"));
    let second = session
        .next_event()
        .await?
        .ok_or("SDK stream ended before its expected event")?;
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
    session.send_typed_user_message(&rich_message)?;
    let interrupt = tokio::spawn(async move { control.interrupt().await });
    let third = session
        .next_event()
        .await?
        .ok_or("SDK stream ended before its expected event")?;
    assert!(matches!(
        third,
        ClaudeEvent::Assistant { ref message, .. }
            if message.text_items().collect::<Vec<_>>() == ["echo:rich"]
    ));
    assert!(interrupt.await??.is_none());
    assert_eq!(session.id(), process_id);

    let control = session.control_handle();
    control.close_stdin()?;
    assert!(session.next_event().await?.is_none());
    let observed_status = control.wait_for_exit().await?;
    let status = session.wait().await?;
    assert_eq!(status, observed_status);
    assert!(status.success());
    assert_eq!(governor.available(), available_before_spawn);
    Ok(())
}

#[cfg(unix)]
fn write_fake_claude(path: &Path) -> TestResult {
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
    fs::write(path, source)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[test]
fn ultra_effort_is_rejected_by_both_execution_modes() {
    let mut config = wrapper_config(PathBuf::from("claude"));
    config.reasoning_effort = Some(crate::provider::request::ReasoningEffort::Ultra);
    for result in [
        config.build_sdk_command(),
        config.build_oneshot_command("prompt"),
    ] {
        assert!(matches!(
            result,
            Err(NornWrappedClaudeError::UnsupportedReasoningEffort { effort: "ultra" })
        ));
    }
}

#[test]
fn valid_tool_names_have_no_invented_length_cap() -> TestResult {
    let mut config = wrapper_config(PathBuf::from("claude"));
    let name = "read".repeat(64);
    config.norn_tools = vec![name.clone()];
    let arguments = config.build_sdk_command()?.build_args();
    assert_eq!(
        argument_value(&arguments, "--allowedTools"),
        Some(format!("mcp__norn__{name}").as_str())
    );
    Ok(())
}

#[test]
fn capture_session_events_rejects_a_nameless_tool_payload() -> TestResult {
    let wrapper = NornWrappedClaudeCode::new(wrapper_config(PathBuf::from("claude")));
    let event = ClaudeEvent::Assistant {
        message: ClaudeMessage {
            id: Some("m1".to_owned()),
            message_type: Some("message".to_owned()),
            role: "assistant".to_owned(),
            model: None,
            content: vec![ContentItem::ToolUse {
                id: "call_1".to_owned(),
                tool_data: claude_runner::events::ToolData::Unknown {
                    name: None,
                    input: Some(serde_json::json!({"a": 1})),
                    extra: std::collections::HashMap::new(),
                },
            }],
            stop_reason: None,
            usage: None,
        },
        session_id: Some("claude-session-1".to_owned()),
        parent_tool_use_id: None,
        uuid: None,
    };
    let error = wrapper
        .capture_session_events(&[event])
        .err()
        .ok_or("nameless tool payload was accepted")?;
    assert!(
        matches!(error, crate::error::IntegrationError::ClaudeRunnerError { reason } if reason.contains("no tool name"))
    );
    Ok(())
}
