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
        model: "gpt-5".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: true,
        context_management: None,
    }
}

#[test]
fn distinct_system_fragments_keep_blank_line_boundaries() -> TestResult {
    let request = request(vec![
        message(MessageRole::System, "PRODUCT-SYSTEM"),
        message(MessageRole::System, "BUILTIN-SYSTEM"),
        message(MessageRole::System, "MANAGED-SYSTEM"),
        message(MessageRole::User, "HUMAN-USER"),
    ]);

    let payload = build_payload(&request, CATALOG_BACKEND_RESPONSES_API)?;

    assert_eq!(
        payload["instructions"],
        "PRODUCT-SYSTEM\n\nBUILTIN-SYSTEM\n\nMANAGED-SYSTEM"
    );
    Ok(())
}

#[test]
fn root_builder_plan_preserves_authority_on_responses_wire() -> TestResult {
    let messages = root_prompt_messages()?;
    let expected_system = messages
        .iter()
        .filter(|message| message.role == MessageRole::System)
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n\n");

    let payload = build_payload(&request(messages), CATALOG_BACKEND_RESPONSES_API)?;
    let input = payload["input"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("Responses input must be an array"))?;

    assert_eq!(payload["instructions"], expected_system);
    assert!(!expected_system.contains(OPERATOR_OVERRIDE));
    assert!(!expected_system.contains(WORKSPACE_PROFILE));
    assert!(
        input
            .iter()
            .any(|item| { item["role"] == "developer" && item["content"] == OPERATOR_OVERRIDE })
    );
    assert!(
        input.iter().any(|item| {
            item["role"] == "user" && item["content"][0]["text"] == WORKSPACE_PROFILE
        })
    );
    Ok(())
}
