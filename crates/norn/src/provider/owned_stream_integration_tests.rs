use std::io;
use std::sync::Arc;
use std::time::Duration;

use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
use crate::provider::openai::OpenAiProvider;
use crate::provider::openai_compatible::OpenAiCompatibleProvider;
use crate::provider::owned_stream_test_support::{StallPoint, StalledServer, TestResult};
use crate::provider::request::{
    Message, MessageRole, ProviderConfig, ProviderRequest, SecretString,
};
use crate::provider::traits::Provider;

const CASE_REPETITIONS: usize = 20;
const PROVIDER_STALL_DEADLINE: Duration = Duration::from_secs(86_400);

fn request() -> ProviderRequest {
    ProviderRequest {
        messages: vec![Message {
            response_items: Vec::new(),
            reasoning: Vec::new(),
            role: MessageRole::User,
            content: Some("ownership probe".to_owned()),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        }],
        tools: Vec::new(),
        model: "test-model".to_owned(),
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

fn config(base_url: &str, max_retries: u32) -> ProviderConfig {
    ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("unused-test-key"),
        },
        base_url: Some(base_url.to_owned()),
        timeout: PROVIDER_STALL_DEADLINE,
        max_retries,
        provider_options: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    }
}

fn responses_provider(base_url: &str, max_retries: u32) -> TestResult<OpenAiProvider> {
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
    Ok(OpenAiProvider::with_auth_provider(
        config(base_url, max_retries),
        auth,
    )?)
}

fn compatible_provider(base_url: &str) -> TestResult<OpenAiCompatibleProvider> {
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("test-key"));
    let mut provider_config = config(&format!("{base_url}/v1"), 0);
    provider_config.retry_backoff = None;
    Ok(OpenAiCompatibleProvider::with_auth_provider(
        provider_config,
        auth,
    )?)
}

async fn dropped_responses_stream_closes_socket(point: StallPoint) -> TestResult {
    let mut server = StalledServer::spawn(point).await?;
    let provider = responses_provider(&server.base_url, 0)?;
    let stream = provider.stream(request())?;
    server.wait_ready().await?;
    drop(stream);
    server.wait_peer_closed().await
}

#[tokio::test]
async fn receiver_drop_cancels_response_header_wait() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        dropped_responses_stream_closes_socket(StallPoint::ResponseHeaders).await?;
    }
    Ok(())
}

#[tokio::test]
async fn receiver_drop_cancels_silent_sse_read() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        dropped_responses_stream_closes_socket(StallPoint::SilentSse).await?;
    }
    Ok(())
}

#[tokio::test]
async fn receiver_drop_cancels_error_body_drain() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        dropped_responses_stream_closes_socket(StallPoint::ErrorBody).await?;
    }
    Ok(())
}

#[tokio::test]
async fn compatible_provider_receiver_drop_closes_socket() -> TestResult {
    for _ in 0..CASE_REPETITIONS {
        let mut server = StalledServer::spawn(StallPoint::ResponseHeaders).await?;
        let provider = compatible_provider(&server.base_url)?;
        let stream = provider.stream(request())?;
        server.wait_ready().await?;
        drop(stream);
        server.wait_peer_closed().await?;
    }
    Ok(())
}

#[tokio::test]
async fn real_loop_cancellation_closes_provider_socket() -> TestResult {
    use tokio_util::sync::CancellationToken;

    use crate::r#loop::LoopContext;
    use crate::r#loop::runner::{
        AgentLoopConfig, AgentStepRequest, AgentStepResult, MockToolExecutor, run_agent_step,
    };
    use crate::session::store::EventStore;

    for _ in 0..CASE_REPETITIONS {
        let mut server = StalledServer::spawn(StallPoint::ResponseHeaders).await?;
        let provider = responses_provider(&server.base_url, 0)?;
        let executor = MockToolExecutor::empty();
        let store = EventStore::new();
        let config = AgentLoopConfig::default();
        let mut loop_context = LoopContext::new("system");
        let token = CancellationToken::new();
        let trigger = token.clone();
        let cancel = async {
            server.wait_ready().await?;
            trigger.cancel();
            TestResult::Ok(())
        };
        let step = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: "prompt",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_context,
            cancel: Some(token),
        });
        let (result, cancel_result) = tokio::join!(step, cancel);
        cancel_result?;
        let result = result?;
        if !matches!(result, AgentStepResult::Cancelled { .. }) {
            return Err(io::Error::other(format!("expected Cancelled, got {result:?}")).into());
        }
        server.wait_peer_closed().await?;
    }
    Ok(())
}

#[tokio::test]
async fn real_step_timeout_closes_provider_socket() -> TestResult {
    use crate::r#loop::LoopContext;
    use crate::r#loop::runner::{
        AgentLoopConfig, AgentStepRequest, AgentStepResult, MockToolExecutor, run_agent_step,
    };
    use crate::session::store::EventStore;

    for _ in 0..CASE_REPETITIONS {
        let mut server = StalledServer::spawn(StallPoint::ResponseHeaders).await?;
        let provider = responses_provider(&server.base_url, 0)?;
        let executor = MockToolExecutor::empty();
        let store = EventStore::new();
        let config = AgentLoopConfig {
            step_timeout: Some(Duration::from_millis(100)),
            ..AgentLoopConfig::default()
        };
        let mut loop_context = LoopContext::new("system");
        let observe_request = server.wait_ready();
        let step = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: "prompt",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: None,
            inbound: None,
            loop_context: &mut loop_context,
            cancel: None,
        });
        let (result, observe_result) = tokio::join!(step, observe_request);
        observe_result?;
        let result = result?;
        if !matches!(result, AgentStepResult::TimedOut { .. }) {
            return Err(io::Error::other(format!("expected TimedOut, got {result:?}")).into());
        }
        server.wait_peer_closed().await?;
    }
    Ok(())
}
