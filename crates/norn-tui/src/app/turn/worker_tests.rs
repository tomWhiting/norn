//! Execution-owner preservation on completion/error and explicit panic refusal.

use std::sync::Arc;

use norn::agent_loop::inbound::{ChannelMessage, InboundSender, MessageKind, inbound_channel};
use norn::provider::events::{ProviderEvent, StopReason};
use norn::provider::mock::MockProvider;
use norn::provider::request::ReasoningEffort;
use norn::provider::usage::Usage;
use norn::provider::{ProviderCapabilities, ProviderError, ProviderRequest, ProviderStream};
use norn::tool::ToolRegistry;

use super::{OwnedTurn, TurnSeed, launch};

fn owned(
    provider: Arc<dyn norn::provider::Provider>,
) -> Result<(OwnedTurn, InboundSender), Box<dyn std::error::Error>> {
    let store = Arc::new(norn::session::EventStore::new());
    let id = uuid::Uuid::new_v4();
    store.bind_view_source(&norn::session::SessionBinding::ephemeral_root(), id, None)?;
    let (events, receiver) = tokio::sync::broadcast::channel(32);
    drop(receiver);
    let sender = norn::provider::AgentEventSender::new(events, id, "root".into());
    let (inbound_sender, inbound) = inbound_channel(2);
    let mut context = norn::agent_loop::LoopContext::new("retained operator instructions");
    context.reasoning_effort = Some(ReasoningEffort::High);
    Ok((
        OwnedTurn {
            context,
            inbound: Some(inbound),
            provider,
            executor: Arc::new(ToolRegistry::new()),
            store,
            model: "gpt-5.5".into(),
            config: norn::agent_loop::config::AgentLoopConfig::default(),
            tools: Vec::new(),
            sender,
            cancel: tokio_util::sync::CancellationToken::new(),
        },
        inbound_sender,
    ))
}

#[tokio::test]
async fn original_context_and_inbox_return_on_success_and_provider_error()
-> Result<(), Box<dyn std::error::Error>> {
    for success in [true, false] {
        let responses = if success {
            vec![vec![ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }]]
        } else {
            Vec::new()
        };
        let provider = Arc::new(MockProvider::new(responses));
        let retained_provider = Arc::clone(&provider);
        let (request, sender) = owned(retained_provider)?;
        let mut worker = launch(request, TurnSeed::ChildResult("fixture prompt".into()))?;
        let mut completed = worker.wait().await?;
        assert_eq!(completed.result.is_ok(), success, "{:?}", completed.result);
        assert_eq!(provider.call_count(), 1);
        assert_eq!(
            completed.context.reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert!(
            completed
                .context
                .system_sections
                .iter()
                .any(|part| part.contains("retained operator instructions"))
        );
        let message_id = uuid::Uuid::new_v4();
        sender
            .send(ChannelMessage {
                id: message_id,
                sender_id: uuid::Uuid::new_v4(),
                from: "child".into(),
                role: None,
                to_id: uuid::Uuid::new_v4(),
                content: "after completion".into(),
                kind: MessageKind::Steer,
                seq: Some(1),
                timestamp: chrono::Utc::now(),
            })
            .await?;
        let messages = completed
            .inbound
            .as_mut()
            .ok_or("inbox lost")?
            .drain_if_steer_ready()
            .ok_or("original sender no longer reaches inbox")?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, message_id);
    }
    Ok(())
}

struct UnwindingProvider;

impl norn::provider::Provider for UnwindingProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        drop(request);
        std::panic::resume_unwind(Box::new("intentional worker unwind"))
    }
}

#[tokio::test]
async fn worker_panic_is_fatal_instead_of_returning_replacement_context()
-> Result<(), Box<dyn std::error::Error>> {
    let (request, sender) = owned(Arc::new(UnwindingProvider))?;
    drop(sender);
    let mut worker = launch(request, TurnSeed::ChildResult("fixture prompt".into()))?;
    assert!(
        matches!(worker.wait().await, Err(crate::TuiError::ExecutionTask { source }) if source.to_string().contains("intentional worker unwind"))
    );
    Ok(())
}
