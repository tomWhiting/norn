use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{StreamExt, stream};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::{
    AgentLoopConfig, AgentStepRequest, AgentStepResult, ToolExecutionSnapshot, ToolExecutor,
    run_agent_step,
};
use crate::error::{ProviderError, ToolError};
use crate::r#loop::loop_context::LoopContext;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::request::{MessageRole, ProviderRequest, ToolCallKind, ToolDefinition};
use crate::provider::tools::ProviderToolDefinition;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::usage::Usage;
use crate::session::store::EventStore;
use crate::system_prompt::builder::ToolPromptEntry;
use crate::tool::traits::ToolCategory;

struct RevisionExecutor {
    revision: u64,
    calls: Arc<Mutex<Vec<u64>>>,
}

#[async_trait::async_trait]
impl ToolExecutor for RevisionExecutor {
    async fn execute(
        &self,
        _name: &str,
        _call_id: &str,
        _arguments: Value,
    ) -> Result<Value, ToolError> {
        self.calls
            .lock()
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!("revision call log lock poisoned: {error}"),
            })?
            .push(self.revision);
        Ok(serde_json::json!({ "revision": self.revision }))
    }
}

struct SwappableExecutor {
    current: Mutex<ToolExecutionSnapshot>,
}

impl SwappableExecutor {
    fn publish(&self, snapshot: ToolExecutionSnapshot) -> Result<(), ToolError> {
        let mut current = self
            .current
            .lock()
            .map_err(|error| ToolError::ExecutionFailed {
                reason: format!("tool snapshot lock poisoned: {error}"),
            })?;
        *current = snapshot;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ToolExecutor for SwappableExecutor {
    async fn execute(
        &self,
        name: &str,
        _call_id: &str,
        _arguments: Value,
    ) -> Result<Value, ToolError> {
        Err(ToolError::ToolNotFound {
            name: name.to_owned(),
        })
    }

    fn execution_snapshot(&self) -> Option<ToolExecutionSnapshot> {
        self.current.lock().ok().map(|snapshot| snapshot.clone())
    }
}

struct BlockingFirstProvider {
    requests: Mutex<Vec<ProviderRequest>>,
    call_count: AtomicUsize,
    first_release: Mutex<Option<oneshot::Receiver<()>>>,
    first_started: mpsc::UnboundedSender<()>,
}

impl BlockingFirstProvider {
    fn requests(&self) -> Result<Vec<ProviderRequest>, ProviderError> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .map_err(|error| provider_lock_error(&error))
    }
}

impl Provider for BlockingFirstProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.requests
            .lock()
            .map_err(|error| provider_lock_error(&error))?
            .push(request);
        let call = self.call_count.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let release = self
                .first_release
                .lock()
                .map_err(|error| provider_lock_error(&error))?
                .take()
                .ok_or_else(|| provider_error("first provider release already consumed"))?;
            self.first_started
                .send(())
                .map_err(|error| provider_error(&format!("first-start signal failed: {error}")))?;
            let gated_call = stream::once(async move {
                release
                    .await
                    .map_err(|error| provider_error(&format!("release signal failed: {error}")))?;
                Ok(ProviderEvent::ToolCallComplete {
                    call_id: "call-generation-one".to_owned(),
                    name: "revision_tool".to_owned(),
                    arguments: "{}".to_owned(),
                    kind: ToolCallKind::Function,
                })
            });
            let done = stream::iter([Ok(done_event(StopReason::ToolUse))]);
            return Ok(Box::pin(gated_call.chain(done)));
        }

        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta {
                text: "finished".to_owned(),
            }),
            Ok(done_event(StopReason::EndTurn)),
        ])))
    }
}

fn provider_lock_error<T>(error: &std::sync::PoisonError<T>) -> ProviderError {
    provider_error(&format!("provider test lock poisoned: {error}"))
}

fn provider_error(reason: &str) -> ProviderError {
    ProviderError::StreamError {
        reason: reason.to_owned(),
        transient: None,
    }
}

fn done_event(stop_reason: StopReason) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason,
        usage: Usage::default(),
        response_id: None,
    }
}

fn snapshot(
    revision: u64,
    description: &str,
    calls: Arc<Mutex<Vec<u64>>>,
) -> ToolExecutionSnapshot {
    ToolExecutionSnapshot {
        revision,
        executor: Arc::new(RevisionExecutor { revision, calls }),
        definitions: Arc::from(vec![ToolDefinition {
            name: "revision_tool".to_owned(),
            description: description.to_owned(),
            parameters: serde_json::json!({ "type": "object" }),
        }]),
        dynamic_prompt_entries: Arc::from(vec![ToolPromptEntry {
            name: "revision_tool".to_owned(),
            category: ToolCategory::General,
            description: description.to_owned(),
            usage_guidance: None,
        }]),
    }
}

fn function_description(request: &ProviderRequest) -> Option<&str> {
    request.tools.iter().find_map(|tool| match tool {
        ProviderToolDefinition::Function(definition) if definition.name == "revision_tool" => {
            Some(definition.description.as_str())
        }
        ProviderToolDefinition::Function(_) | ProviderToolDefinition::Hosted(_) => None,
    })
}

fn dynamic_prompt(request: &ProviderRequest) -> Option<&str> {
    request.messages.iter().rev().find_map(|message| {
        (message.role == MessageRole::Developer)
            .then_some(message.content.as_deref())
            .flatten()
    })
}

#[tokio::test]
async fn blocked_provider_response_keeps_lease_and_next_request_refreshes()
-> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = Arc::new(SwappableExecutor {
        current: Mutex::new(snapshot(1, "generation one", Arc::clone(&calls))),
    });
    let executor: Arc<dyn ToolExecutor> = Arc::clone(&source) as Arc<dyn ToolExecutor>;
    let (release_tx, release_rx) = oneshot::channel();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let provider = BlockingFirstProvider {
        requests: Mutex::new(Vec::new()),
        call_count: AtomicUsize::new(0),
        first_release: Mutex::new(Some(release_rx)),
        first_started: started_tx,
    };
    let store = EventStore::new();
    let mut loop_context = LoopContext::new("base system");
    let config = AgentLoopConfig {
        max_iterations: Some(3),
        ..AgentLoopConfig::default()
    };

    let mut run = Box::pin(run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "exercise a leased tool",
        tools: &[],
        output_schema: None,
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_context,
        cancel: None,
    }));

    tokio::select! {
        marker = started_rx.recv() => {
            if marker.is_none() {
                return Err(std::io::Error::other("provider start channel closed").into());
            }
        }
        result = &mut run => {
            return Err(std::io::Error::other(format!(
                "step completed before the first provider call was released: {result:?}",
            )).into());
        }
    }

    source.publish(snapshot(2, "generation two", Arc::clone(&calls)))?;
    release_tx
        .send(())
        .map_err(|()| std::io::Error::other("provider release receiver dropped"))?;
    let result = run.await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));

    let recorded_calls = calls
        .lock()
        .map_err(|error| std::io::Error::other(format!("call log lock poisoned: {error}")))?;
    assert_eq!(recorded_calls.as_slice(), &[1]);
    let requests = provider.requests()?;
    assert_eq!(requests.len(), 2);
    assert_eq!(function_description(&requests[0]), Some("generation one"));
    assert_eq!(function_description(&requests[1]), Some("generation two"));
    assert!(dynamic_prompt(&requests[0]).is_some_and(|prompt| prompt.contains("generation one")));
    assert!(dynamic_prompt(&requests[1]).is_some_and(|prompt| prompt.contains("generation two")));
    Ok(())
}
