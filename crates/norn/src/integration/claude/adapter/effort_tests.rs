use std::path::PathBuf;

use super::*;
use crate::provider::request::{ReasoningEffort, ToolCallCaller, ToolDefinition};
use crate::provider::tools::ProviderToolDefinition;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn adapter() -> ClaudeRunnerAdapter {
    ClaudeRunnerAdapter::new(ClaudeRunnerConfig {
        runner_path: PathBuf::from("/usr/local/bin/claude"),
        model: "claude-opus-5".to_owned(),
        max_tokens: None,
    })
}

fn request(reasoning_effort: Option<ReasoningEffort>) -> ProviderRequest {
    ProviderRequest {
        messages: vec![Message {
            response_items: Vec::new(),
            role: MessageRole::User,
            content: Some("test prompt".to_owned()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: ToolCallCaller::Absent,
        }],
        tools: Vec::new(),
        model: "claude-opus-5".to_owned(),
        reasoning_effort,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    }
}

fn effort_argument(arguments: &[String]) -> Option<&str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == "--effort")
        .map(|pair| pair[1].as_str())
}

#[test]
fn supported_reasoning_efforts_use_exact_claude_cli_values() -> TestResult {
    for (effort, expected) in [
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::XHigh, "xhigh"),
        (ReasoningEffort::Max, "max"),
    ] {
        let command = adapter().build_command(&request(Some(effort)))?;
        let arguments = command.build_args();
        assert_eq!(effort_argument(&arguments), Some(expected));
    }
    Ok(())
}

#[test]
fn omitted_reasoning_effort_leaves_claude_default_untouched() -> TestResult {
    let command = adapter().build_command(&request(None))?;
    let arguments = command.build_args();

    assert_eq!(effort_argument(&arguments), None);
    assert!(!arguments.iter().any(|argument| argument == "--effort"));
    Ok(())
}

#[test]
fn explicit_none_reasoning_effort_fails_before_spawn() -> TestResult {
    let result = adapter().build_command(&request(Some(ReasoningEffort::None)));

    match result {
        Err(ProviderError::UnsupportedFeature { feature }) => {
            assert_eq!(feature, "reasoning effort 'none' through Claude Runner");
        }
        Err(other) => {
            return Err(format!("expected UnsupportedFeature, got {other:?}").into());
        }
        Ok(_) => return Err("explicit 'none' must fail before spawning Claude Runner".into()),
    }
    Ok(())
}

#[test]
fn adapter_is_isolated_and_rejects_unbound_norn_tools() -> TestResult {
    let command = adapter().build_command(&request(None))?;
    let arguments = command.build_args();

    assert_eq!(effort_argument(&arguments), None);
    assert!(arguments.windows(2).any(|pair| pair == ["--tools", ""]));
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--setting-sources", ""])
    );
    assert!(
        !arguments
            .iter()
            .any(|argument| argument == "--dangerously-skip-permissions")
    );
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--input-format", "text"])
    );

    let mut request = request(None);
    request
        .tools
        .push(ProviderToolDefinition::Function(ToolDefinition {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }));
    assert!(matches!(
        adapter().build_command(&request),
        Err(ProviderError::UnsupportedFeature { feature })
            if feature.contains("NornWrappedClaudeSession")
    ));
    Ok(())
}
