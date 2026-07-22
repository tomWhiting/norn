use std::path::PathBuf;

use super::*;
use crate::provider::request::ToolCallCaller;
use crate::tests::prompt_authority_support::{
    OPERATOR_OVERRIDE, WORKSPACE_PROFILE, root_prompt_messages,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn message(role: MessageRole, content: &str) -> Message {
    Message {
        response_items: Vec::new(),
        role,
        content: Some(content.to_owned()),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
        tool_call_caller: ToolCallCaller::Absent,
    }
}

fn request(messages: Vec<Message>) -> ProviderRequest {
    ProviderRequest {
        messages,
        tools: Vec::new(),
        model: "sonnet".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    }
}

#[test]
fn developer_is_explicitly_downgraded_to_prompt_and_never_promoted() -> TestResult {
    let adapter = ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
        runner_path: PathBuf::from("/usr/local/bin/claude"),
        model: "sonnet".to_owned(),
        max_tokens: None,
    });
    let command = adapter.build_command(&request(vec![
        message(MessageRole::System, "PRODUCT-SYSTEM"),
        message(MessageRole::User, "HUMAN-USER"),
        message(MessageRole::Developer, "MANAGED-DEVELOPER-TAIL"),
    ]))?;
    let arguments = command.build_args();
    let Some(system_flag) = arguments
        .iter()
        .position(|argument| argument == "--system-prompt")
    else {
        return Err("Claude command omitted the System prompt flag".into());
    };
    let Some(system_prompt) = arguments.get(system_flag.saturating_add(1)) else {
        return Err("Claude command omitted the System prompt value".into());
    };
    let Some(prompt) = arguments.last() else {
        return Err("Claude command omitted its positional prompt".into());
    };

    assert_eq!(system_prompt, "PRODUCT-SYSTEM");
    assert!(!system_prompt.contains("MANAGED-DEVELOPER-TAIL"));
    assert_eq!(prompt, "HUMAN-USER\n\nMANAGED-DEVELOPER-TAIL");
    Ok(())
}

#[test]
fn distinct_system_fragments_keep_blank_line_boundaries() -> TestResult {
    let adapter = ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
        runner_path: PathBuf::from("/usr/local/bin/claude"),
        model: "sonnet".to_owned(),
        max_tokens: None,
    });
    let command = adapter.build_command(&request(vec![
        message(MessageRole::System, "PRODUCT-SYSTEM"),
        message(MessageRole::System, "BUILTIN-SYSTEM"),
        message(MessageRole::User, "HUMAN-USER"),
    ]))?;
    let arguments = command.build_args();
    let Some(system_flag) = arguments
        .iter()
        .position(|argument| argument == "--system-prompt")
    else {
        return Err("Claude command omitted the System prompt flag".into());
    };
    let Some(system_prompt) = arguments.get(system_flag.saturating_add(1)) else {
        return Err("Claude command omitted the System prompt value".into());
    };

    assert_eq!(system_prompt, "PRODUCT-SYSTEM\n\nBUILTIN-SYSTEM");
    Ok(())
}

#[test]
fn root_builder_plan_downgrades_developer_without_promoting_it() -> TestResult {
    let messages = root_prompt_messages()?;
    let expected_system = messages
        .iter()
        .filter(|message| message.role == MessageRole::System)
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n\n");
    let expected_prompt = messages
        .iter()
        .filter(|message| {
            matches!(
                message.role,
                MessageRole::Developer | MessageRole::User | MessageRole::ToolResult
            )
        })
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n\n");
    let adapter = ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
        runner_path: PathBuf::from("/usr/local/bin/claude"),
        model: "sonnet".to_owned(),
        max_tokens: None,
    });
    let command = adapter.build_command(&request(messages))?;
    let arguments = command.build_args();
    let Some(system_flag) = arguments
        .iter()
        .position(|argument| argument == "--system-prompt")
    else {
        return Err("Claude command omitted the System prompt flag".into());
    };
    let Some(system_prompt) = arguments.get(system_flag.saturating_add(1)) else {
        return Err("Claude command omitted the System prompt value".into());
    };
    let Some(prompt) = arguments.last() else {
        return Err("Claude command omitted its positional prompt".into());
    };

    assert_eq!(system_prompt, &expected_system);
    assert_eq!(prompt, &expected_prompt);
    assert!(!system_prompt.contains(OPERATOR_OVERRIDE));
    assert!(!system_prompt.contains(WORKSPACE_PROFILE));
    assert!(prompt.contains(OPERATOR_OVERRIDE));
    assert!(prompt.contains(WORKSPACE_PROFILE));
    Ok(())
}
