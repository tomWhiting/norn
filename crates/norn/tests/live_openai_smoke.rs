//! Explicit opt-in live `OpenAI` smoke; normal release gates compile but do not run it.

use std::error::Error;
use std::io;
use std::time::Duration;

use futures_util::StreamExt;
use norn::provider::openai::OpenAiProvider;
use norn::provider::{
    AuthSource, Message, MessageRole, Provider, ProviderConfig, ProviderEvent, ProviderRequest,
    SecretString, ToolCallCaller,
};
use norn::test_prerequisite;

#[tokio::test]
async fn openai_live_hello_smoke() -> Result<(), Box<dyn Error>> {
    let api_key = match std::env::var("OPENAI_TEST_KEY") {
        Ok(value) if !value.is_empty() => SecretString::new(value),
        _ => {
            return Err(test_prerequisite::missing(
                "openai_live_hello_smoke",
                "OPENAI_TEST_KEY must be set and nonempty for this opt-in live lane",
            )
            .into());
        }
    };
    // Preserve the existing smoke's model and request policy; these are not
    // catalogue recommendations or newly chosen runtime defaults.
    let provider = OpenAiProvider::new(ProviderConfig {
        auth_source: AuthSource::ApiKey { key: api_key },
        base_url: None,
        timeout: Duration::from_secs(30),
        max_retries: 2,
        provider_options: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    })
    .await?;
    let mut stream = provider.stream(ProviderRequest {
        messages: vec![Message {
            response_items: Vec::new(),
            reasoning: Vec::new(),
            role: MessageRole::User,
            content: Some("Say hello in exactly one word.".to_owned()),
            thinking: String::new(),
            tool_calls: vec![],
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: ToolCallCaller::Absent,
        }],
        tools: vec![],
        model: "gpt-4.1-mini".to_owned(),
        reasoning_effort: None,
        reasoning_summary: None,
        service_tier: None,
        config: None,
        cache_key: None,
        previous_response_id: None,
        store: false,
        context_management: None,
    })?;
    let mut saw_text_delta = false;
    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        match event? {
            ProviderEvent::TextDelta { .. } => saw_text_delta = true,
            ProviderEvent::Done { .. } => saw_done = true,
            _ => {}
        }
    }
    if !saw_text_delta {
        return Err(io::Error::other("openai_live_hello_smoke: no TextDelta event").into());
    }
    if !saw_done {
        return Err(io::Error::other("openai_live_hello_smoke: no Done event").into());
    }
    Ok(())
}
