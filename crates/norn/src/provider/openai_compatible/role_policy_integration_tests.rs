use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;

use super::OpenAiCompatibleProvider;
use super::role_policy::OPTION_KEY;
use crate::error::ProviderError;
use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
use crate::provider::request::{
    Message, MessageRole, ProviderConfig, ProviderRequest, SecretString, ToolCallCaller,
};
use crate::provider::traits::Provider as _;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn provider_config(policy: &str) -> ProviderConfig {
    ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("unused-direct-auth"),
        },
        base_url: Some("http://127.0.0.1:1/v1".to_owned()),
        timeout: Duration::from_millis(100),
        max_retries: 0,
        provider_options: Some(serde_json::json!({(OPTION_KEY): policy})),
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    }
}

fn developer_request() -> ProviderRequest {
    ProviderRequest {
        messages: vec![Message {
            response_items: Vec::new(),
            role: MessageRole::Developer,
            content: Some("developer policy".to_owned()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: ToolCallCaller::Absent,
        }],
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

#[tokio::test]
async fn provider_pins_reject_policy_before_any_dispatch() -> TestResult {
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
    let provider = OpenAiCompatibleProvider::with_auth_provider(provider_config("reject"), auth)?;
    let mut stream = provider.stream(developer_request())?;

    let result = stream
        .next()
        .await
        .ok_or("provider stream ended without the policy error")?;
    assert!(matches!(
        result,
        Err(ProviderError::UnsupportedFeature { .. })
    ));
    assert!(stream.next().await.is_none());
    Ok(())
}

#[test]
fn provider_rejects_invalid_policy_during_construction() {
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
    let result =
        OpenAiCompatibleProvider::with_auth_provider(provider_config("downgrade_to_system"), auth);

    assert!(matches!(result, Err(ProviderError::InvalidRequest { .. })));
}
