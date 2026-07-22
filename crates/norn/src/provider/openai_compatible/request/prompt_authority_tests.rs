use super::*;
use crate::tests::prompt_authority_support::{
    OPERATOR_OVERRIDE, WORKSPACE_PROFILE, root_prompt_messages,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn request(messages: Vec<Message>) -> ProviderRequest {
    ProviderRequest {
        messages,
        tools: Vec::new(),
        model: "local-model".to_owned(),
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
fn root_builder_plan_preserves_native_chat_authority() -> TestResult {
    let messages = root_prompt_messages()?;
    let payload = build_payload(&request(messages.clone()), DeveloperRolePolicy::Native)?;
    let wire_messages = payload["messages"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("Chat messages must be an array"))?;

    assert_eq!(wire_messages.len(), messages.len());
    for (source, wire) in messages.iter().zip(wire_messages) {
        let expected_role = match source.role {
            MessageRole::System => "system",
            MessageRole::Developer => "developer",
            MessageRole::User => "user",
            MessageRole::Assistant | MessageRole::ToolResult => {
                return Err("root stable plan contained a non-prompt role".into());
            }
        };
        assert_eq!(wire["role"], expected_role);
        assert_eq!(
            wire["content"],
            source.content.as_deref().unwrap_or_default()
        );
    }
    assert!(wire_messages.iter().any(|message| {
        message["role"] == "developer" && message["content"] == OPERATOR_OVERRIDE
    }));
    assert!(
        wire_messages.iter().any(|message| {
            message["role"] == "user" && message["content"] == WORKSPACE_PROFILE
        })
    );
    Ok(())
}
