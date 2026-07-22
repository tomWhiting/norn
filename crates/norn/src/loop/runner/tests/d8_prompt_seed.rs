use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::context::ContextLoader;
use crate::r#loop::config::ConversationStateMode;
use crate::profile::PromptCommand;
use crate::provider::auth::{AuthProvider, AuthSource, MockAuthProvider};
use crate::provider::openai::OpenAiProvider;
use crate::provider::request::{ProviderConfig, SecretString};
use crate::session::{
    ProviderStateProvenance, committed_response_publication, response_publication_fixture,
};
use crate::system_prompt::{PromptPlan, PromptSeedFingerprint, PromptSource};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

mod legacy_rule_cut;
mod prompt_command;
mod reasoning_cut;

fn wire_provider(server: &MockServer) -> Result<OpenAiProvider, ProviderError> {
    let config = ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("d8-prompt-seed-key"),
        },
        base_url: Some(format!("{}/v1", server.uri())),
        timeout: Duration::from_secs(5),
        max_retries: 0,
        provider_options: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    };
    let auth: Arc<dyn AuthProvider> = Arc::new(MockAuthProvider::single("d8-prompt-seed-key"));
    OpenAiProvider::with_auth_provider(config, auth)
}

fn completed_stream(response_id: &str) -> String {
    let terminal = json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": response_id,
            "status": "completed",
            "output": [{
                "id": format!("msg_{response_id}"),
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": format!("answer from {response_id}"),
                    "annotations": [],
                    "logprobs": []
                }]
            }],
            "incomplete_details": null,
            "usage": {
                "input_tokens": 4,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens": 2,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 6
            }
        }
    });
    format!("event: response.completed\ndata: {terminal}\n\n")
}

async fn mount_response_sequence(
    server: &MockServer,
    response_ids: &[&str],
) -> Result<(), io::Error> {
    let response_ids = Arc::new(
        response_ids
            .iter()
            .map(|response_id| (*response_id).to_owned())
            .collect::<Vec<_>>(),
    );
    let response_count = u64::try_from(response_ids.len())
        .map_err(|error| io::Error::other(format!("response count overflow: {error}")))?;
    let next = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |_request: &wiremock::Request| {
            let index = next.fetch_add(1, Ordering::SeqCst);
            response_ids.get(index).map_or_else(
                || ResponseTemplate::new(500).set_body_string("response sequence exhausted"),
                |response_id| {
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(completed_stream(response_id))
                },
            )
        })
        .expect(response_count)
        .mount(server)
        .await;
    Ok(())
}

async fn received_payloads(server: &MockServer, expected: usize) -> Result<Vec<Value>, io::Error> {
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is unavailable"))?;
    if requests.len() != expected {
        return Err(io::Error::other(format!(
            "expected {expected} provider requests, observed {}",
            requests.len()
        )));
    }
    requests
        .iter()
        .map(|request| {
            serde_json::from_slice(&request.body)
                .map_err(|error| io::Error::other(format!("invalid request JSON: {error}")))
        })
        .collect()
}

fn append_legacy_v1_anchor(store: &EventStore, response_id: &str) -> TestResult {
    let fixture = response_publication_fixture(store.last_event_id(), true)?;
    let assistant = SessionEvent::AssistantMessage {
        base: fixture.assistant_base,
        response_items: Vec::new(),
        content: "legacy answer".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    };
    let publication =
        committed_response_publication(fixture.boundary, fixture.provenance, assistant)?;
    store.append_batch(&publication)?;
    Ok(())
}

async fn run_wire_step(
    provider: &OpenAiProvider,
    store: &EventStore,
    loop_context: &mut LoopContext,
    user_prompt: &str,
) -> Result<AgentStepResult, NornError> {
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt,
        tools: &[],
        output_schema: None,
        model: "gpt-test",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context,
        cancel: None,
    })
    .await
}

fn three_authority_plan(product: &str) -> PromptPlan {
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, product);
    plan.set(PromptSource::OperatorProfile, "operator-policy");
    plan.set(PromptSource::WorkspaceProfile, "workspace-policy");
    plan
}

fn expected_instructions(product: &str) -> String {
    format!(
        "{product}\n\n{}",
        crate::system_prompt::CollaborationMode::default().format_section()
    )
}

#[tokio::test]
async fn v1_anchor_bootstraps_seed_once_and_publishes_v2() -> TestResult {
    let server = MockServer::start().await;
    mount_response_sequence(&server, &["resp_v2", "resp_v3"]).await?;
    let provider = wire_provider(&server)?;
    let store = EventStore::new();
    store.validate_or_bind_provider_state_identity(provider.state_identity())?;
    append_legacy_v1_anchor(&store, "resp_v1")?;
    let plan = three_authority_plan("product-policy");
    let command_context = "# bootstrap-runtime\nbootstrap-runtime";
    let expected_seed =
        PromptSeedFingerprint::from_plan(&plan).with_operator_runtime_context(command_context);
    let mut loop_context = LoopContext::new("legacy");
    loop_context.install_stable_prompt_plan(plan);
    loop_context.prompt_commands.push(PromptCommand {
        name: "bootstrap-runtime".to_owned(),
        command: "printf bootstrap-runtime".to_owned(),
        cache_ttl: Some(Duration::from_secs(60)),
    });

    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "first-new-task").await?);
    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "second-new-task").await?);

    let payloads = received_payloads(&server, 2).await?;
    assert_eq!(payloads[0]["previous_response_id"], "resp_v1");
    assert_eq!(
        payloads[0]["instructions"],
        expected_instructions("product-policy")
    );
    assert_eq!(
        payloads[0]["input"],
        json!([
            {"type": "message", "role": "developer", "content": "operator-policy"},
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "workspace-policy"}]
            },
            {"type": "message", "role": "developer", "content": command_context},
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "first-new-task"}]
            }
        ])
    );
    assert_eq!(payloads[1]["previous_response_id"], "resp_v2");
    assert_eq!(
        payloads[1]["instructions"],
        expected_instructions("product-policy")
    );
    assert_eq!(
        payloads[1]["input"],
        json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "second-new-task"}]
        }])
    );

    let seeds = store
        .events()
        .iter()
        .filter_map(|event| ProviderStateProvenance::from_event(event).ok().flatten())
        .map(|provenance| provenance.prompt_seed_fingerprint())
        .collect::<Vec<_>>();
    assert_eq!(seeds, [None, Some(expected_seed), Some(expected_seed)]);
    Ok(())
}

#[tokio::test]
async fn system_only_change_preserves_anchor_and_replaces_instructions() -> TestResult {
    let server = MockServer::start().await;
    mount_response_sequence(&server, &["resp_1", "resp_2"]).await?;
    let provider = wire_provider(&server)?;
    let store = EventStore::new();
    let mut loop_context = LoopContext::new("legacy");
    loop_context.install_stable_prompt_plan(three_authority_plan("product-v1"));

    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "first-task").await?);
    loop_context.install_stable_prompt_plan(three_authority_plan("product-v2"));
    assert_completed(run_wire_step(&provider, &store, &mut loop_context, "second-task").await?);

    let payloads = received_payloads(&server, 2).await?;
    assert_eq!(
        payloads[0]["instructions"],
        expected_instructions("product-v1")
    );
    assert_eq!(payloads[1]["previous_response_id"], "resp_1");
    assert_eq!(
        payloads[1]["instructions"],
        expected_instructions("product-v2")
    );
    assert_eq!(
        payloads[1]["input"],
        json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "second-task"}]
        }])
    );
    assert!(!serde_json::to_string(&payloads[1])?.contains("product-v1"));
    assert!(!serde_json::to_string(&payloads[1])?.contains("operator-policy"));
    assert!(!serde_json::to_string(&payloads[1])?.contains("workspace-policy"));

    let seeds = store
        .events()
        .iter()
        .filter_map(|event| ProviderStateProvenance::from_event(event).ok().flatten())
        .map(|provenance| provenance.prompt_seed_fingerprint())
        .collect::<Vec<_>>();
    assert_eq!(seeds.len(), 2);
    assert_eq!(seeds[0], seeds[1]);
    Ok(())
}

#[tokio::test]
async fn hot_project_context_change_cuts_v2_anchor_and_persists_new_seed() -> TestResult {
    let workspace = tempfile::tempdir()?;
    let context_path = workspace.path().join("NORN.md");
    std::fs::write(&context_path, "repository-v1")?;

    let mut loop_context = LoopContext::new("legacy");
    loop_context.context_loader = Some(ContextLoader::load(workspace.path()));
    let mut plan = PromptPlan::new();
    plan.set(PromptSource::ProductPolicy, "product");
    plan.set(PromptSource::ProjectContextFile, "repository-v1");
    loop_context.install_stable_prompt_plan(plan);

    let first = vec![
        tool_call_delta("rewrite_context", Some("rewrite_context"), "{}"),
        ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: Some("resp_before_context_change".to_owned()),
        },
    ];
    let second = vec![
        text_delta("done"),
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: Some("resp_after_context_change".to_owned()),
        },
    ];
    let provider = MockProvider::with_capabilities(
        vec![first, second],
        ProviderCapabilities::openai_responses(),
    )
    .with_state_identity(crate::provider::ProviderStateIdentity::derive(
        "norn.runner.hot-prompt-seed",
        b"hot-prompt-seed-fixture",
    ));

    let rewritten_path = context_path.clone();
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "rewrite_context".to_owned(),
        Box::new(move |_| {
            std::fs::write(&rewritten_path, "repository-v2").map_err(|error| {
                crate::error::ToolError::ExecutionFailed {
                    reason: format!("failed to rewrite project context fixture: {error}"),
                }
            })?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&rewritten_path)
                .map_err(|error| crate::error::ToolError::ExecutionFailed {
                    reason: format!("failed to reopen project context fixture: {error}"),
                })?;
            file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
                .map_err(|error| crate::error::ToolError::ExecutionFailed {
                    reason: format!("failed to advance project context mtime: {error}"),
                })?;
            Ok(serde_json::json!({"rewritten": true}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let tools = [ToolDefinition {
        name: "rewrite_context".to_owned(),
        description: "Rewrite project context".to_owned(),
        parameters: serde_json::json!({}),
    }];
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };
    let store = EventStore::new();

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &tools,
            schema: None,
            config: &config,
            event_tx: None,
            inbound: None,
        },
        &mut loop_context,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].previous_response_id, None,
        "the first request has no provider anchor",
    );
    assert_eq!(
        requests[1].previous_response_id, None,
        "a changed repository seed must cut the V2 anchor",
    );
    assert!(requests[1].messages.iter().any(|message| {
        message.role == MessageRole::User && message.content.as_deref() == Some("repository-v2")
    }));
    assert!(
        requests[1]
            .messages
            .iter()
            .all(|message| message.content.as_deref() != Some("repository-v1"))
    );

    let Some(current_plan) = loop_context.stable_prompt_plan() else {
        return Err(std::io::Error::other("typed prompt plan disappeared").into());
    };
    let expected_seed = PromptSeedFingerprint::from_plan(current_plan);
    let persisted = store
        .events()
        .iter()
        .filter_map(|event| ProviderStateProvenance::from_event(event).ok().flatten())
        .collect::<Vec<_>>();
    assert_eq!(persisted.len(), 2);
    assert_ne!(
        persisted[0].prompt_seed_fingerprint(),
        persisted[1].prompt_seed_fingerprint(),
    );
    assert_eq!(persisted[1].prompt_seed_fingerprint(), Some(expected_seed),);
    Ok(())
}
