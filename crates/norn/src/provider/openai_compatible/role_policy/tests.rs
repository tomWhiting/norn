//! Developer-role policy tests.

use super::*;
use crate::provider::request::{
    Message, MessageRole, ProviderOptions, ProviderRequest, ToolCallCaller,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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

fn request() -> ProviderRequest {
    ProviderRequest {
        messages: vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "user"),
            message(MessageRole::Developer, "developer tail"),
        ],
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

fn roles(payload: &Value) -> TestResult<Vec<&str>> {
    payload["messages"]
        .as_array()
        .ok_or_else(|| std::io::Error::other("messages must be an array"))?
        .iter()
        .map(|message| {
            message["role"]
                .as_str()
                .ok_or_else(|| std::io::Error::other("message role must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[test]
fn native_policy_preserves_developer_wire_role() -> TestResult {
    let payload = super::super::request::build_payload(&request(), DeveloperRolePolicy::Native)?;

    assert_eq!(roles(&payload)?, ["system", "user", "developer"]);
    Ok(())
}

#[test]
fn legacy_reject_policy_fails_with_typed_unsupported_feature() {
    let result = super::super::request::build_payload(&request(), DeveloperRolePolicy::Reject);

    assert!(matches!(
        result,
        Err(ProviderError::UnsupportedFeature { .. })
    ));
}

#[test]
fn explicit_legacy_downgrade_uses_user_never_system() -> TestResult {
    let payload =
        super::super::request::build_payload(&request(), DeveloperRolePolicy::DowngradeToUser)?;

    assert_eq!(roles(&payload)?, ["system", "user", "user"]);
    Ok(())
}

#[test]
fn scoped_policy_is_typed_and_never_forwarded() -> TestResult {
    let options = serde_json::json!({
        "api_options": {
            "openai_chat_completions": {
                (OPTION_KEY): "downgrade_to_user",
                "temperature": 0.25
            }
        }
    });
    let policy = DeveloperRolePolicy::from_provider_options(Some(&options))?;
    let mut request = request();
    request.config = Some(ProviderOptions(options));

    let payload = super::super::request::build_payload(&request, policy)?;

    assert_eq!(roles(&payload)?, ["system", "user", "user"]);
    assert_eq!(payload["temperature"], 0.25);
    assert!(payload.get(OPTION_KEY).is_none());
    Ok(())
}

#[test]
fn invalid_or_ambiguous_policy_fails_typed() {
    for options in [
        serde_json::json!({(OPTION_KEY): "downgrade_to_system"}),
        serde_json::json!({
            (OPTION_KEY): "native",
            "openai_chat_completions": {(OPTION_KEY): "reject"}
        }),
    ] {
        assert!(matches!(
            DeveloperRolePolicy::from_provider_options(Some(&options)),
            Err(ProviderError::InvalidRequest { .. })
        ));
    }
}

#[test]
fn reserved_policy_key_is_rejected_outside_supported_locations() {
    for options in [
        serde_json::json!({
            "api_options": {
                "openai_responses": {(OPTION_KEY): "reject"}
            }
        }),
        serde_json::json!({
            "openai_chat_completions": {
                "nested": {(OPTION_KEY): "reject"}
            }
        }),
        serde_json::json!({
            "metadata": [{(OPTION_KEY): "reject"}]
        }),
        serde_json::json!({
            "api_options": {"openai_chat_completions": {"temperature": 0.25}},
            "openai_chat_completions": {(OPTION_KEY): "reject"}
        }),
        serde_json::json!({
            "api_options.openai_chat_completions": {(OPTION_KEY): "reject"}
        }),
        serde_json::json!({
            "openai_chat_completions.nested": {(OPTION_KEY): "reject"}
        }),
    ] {
        let result = DeveloperRolePolicy::from_provider_options(Some(&options));
        assert!(matches!(result, Err(ProviderError::InvalidRequest { .. })));
    }
}
