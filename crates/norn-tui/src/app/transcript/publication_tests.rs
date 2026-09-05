//! Real runner publication order tests; no forged receipt cells or content-based associations.

use std::collections::BTreeMap;
use std::sync::Arc;

use norn::agent_loop::LoopContext;
use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::runner::{AgentStepRequest, AgentStepResult, run_agent_step};
use norn::provider::Provider;
use norn::provider::mock::MockProvider;
use norn::session::SessionBinding;
use norn::session_view::ViewItemKind;
use norn::tool::ToolRegistry;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    view: Transcript,
    store: Arc<EventStore>,
    sender: AgentEventSender,
    receiver: broadcast::Receiver<AgentEvent>,
    model: ModelRuntime,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let store = Arc::new(EventStore::new());
        let agent = Uuid::new_v4();
        let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
        // Explicit fixture capacity exceeds this one-response script's native event count.
        let (tx, receiver) = broadcast::channel(32);
        Ok(Self {
            view: Transcript::new(source),
            store,
            sender: AgentEventSender::new(tx, agent, "root".to_owned()),
            receiver,
            model: ModelRuntime::new(None, "gpt-5.5", Some(272_000), None, None, BTreeMap::new())?,
        })
    }

    fn admit(&mut self, human: bool) -> Result<AgentEventSender, TuiError> {
        let input = if human {
            Some(
                self.view
                    .notice(ViewItemKind::Input, "You · submitted", Some("same prompt"))?,
            )
        } else {
            None
        };
        self.view
            .observe_execution(&self.sender, &self.store, &self.model, input)
    }

    fn apply_queued(&mut self) -> TestResult {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    assert!(self.view.observe_event(&event)?);
                    let reduction = self.view.apply_live(&event)?;
                    self.view.note_completion(&event, &reduction);
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(error) => return Err(error.into()),
            }
        }
        self.view.drain_publications()?;
        Ok(())
    }

    fn assert_canonical(&self) {
        assert_eq!(
            self.view
                .projection
                .items()
                .filter(|item| matches!(item.kind, ViewItemKind::Input))
                .count(),
            1
        );
        assert_eq!(
            self.view
                .projection
                .items()
                .filter(|item| matches!(item.kind, ViewItemKind::Text))
                .count(),
            1
        );
    }
}

fn provider() -> MockProvider {
    MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "same answer".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            response_id: None,
            usage: Usage {
                input_tokens: 11,
                output_tokens: 7,
                ..Usage::default()
            },
        },
    ]])
}

async fn run(
    store: &EventStore,
    sender: &AgentEventSender,
    provider: &dyn Provider,
) -> Result<AgentStepResult, norn::error::NornError> {
    let executor: Arc<dyn norn::agent_loop::runner::ToolExecutor> = Arc::new(ToolRegistry::new());
    let mut context = LoopContext::new("deterministic publication fixture");
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: "same prompt",
        tools: &[],
        output_schema: None,
        model: "gpt-5.5",
        config: &AgentLoopConfig::default(),
        event_tx: Some(sender),
        inbound: None,
        loop_context: &mut context,
        cancel: None,
    })
    .await
}

#[tokio::test]
async fn history_before_queued_live_has_one_input_and_answer_without_provider_ids() -> TestResult {
    let mut fixture = Fixture::new()?;
    let sender = fixture.admit(true)?;
    let result = run(&fixture.store, &sender, &provider()).await?;
    assert!(matches!(result, AgentStepResult::Completed { .. }));
    let page = fixture
        .store
        .history_page(&fixture.view.initial_history()?)?;
    fixture.view.accept_history(&page)?;
    fixture.apply_queued()?;
    fixture.assert_canonical();
    assert!(
        fixture
            .view
            .publication
            .active
            .as_ref()
            .ok_or("missing active publication")?
            .attempts
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn receipt_before_history_is_idempotent_and_completion_owns_full_details() -> TestResult {
    let mut fixture = Fixture::new()?;
    let sender = fixture.admit(true)?;
    run(&fixture.store, &sender, &provider()).await?;
    fixture.apply_queued()?;
    fixture.assert_canonical();
    let page = fixture
        .store
        .history_page(&fixture.view.initial_history()?)?;
    fixture.view.accept_history(&page)?;
    fixture.assert_canonical();
    let usage = Usage {
        input_tokens: 11,
        output_tokens: 7,
        ..Usage::default()
    };
    let completion = fixture.view.complete_publication(
        "Turn completed",
        Some(Duration::from_secs(1)),
        Some(&usage),
        true,
    )?;
    assert!(fixture.view.completion_compact(&completion));
    let item = fixture
        .view
        .projection
        .item(&completion)
        .ok_or("completion item missing")?;
    let reference = item.bodies.first().ok_or("completion details missing")?;
    let body = fixture.view.projection.read_provisional(
        reference,
        norn::session_view::BodyRange {
            offset: 0,
            max_bytes: std::num::NonZeroUsize::new(4096).ok_or("invalid fixture demand")?,
        },
    )?;
    assert!(body.original_text.contains("Stop reason: EndTurn"));
    assert!(body.original_text.contains("Provider response ID: None"));
    assert!(body.original_text.contains("Accepted model:"));
    assert!(body.original_text.contains("Source:"));
    assert!(fixture.view.publication.active.is_none());
    Ok(())
}

#[tokio::test]
async fn repeated_equal_turns_use_distinct_receipts_and_retired_execution_cannot_enter()
-> TestResult {
    let mut fixture = Fixture::new()?;
    let sender = fixture.admit(true)?;
    run(&fixture.store, &sender, &provider()).await?;
    let old = fixture.receiver.recv().await?;
    assert!(fixture.view.observe_event(&old)?);
    let reduction = fixture.view.apply_live(&old)?;
    fixture.view.note_completion(&old, &reduction);
    fixture.apply_queued()?;
    fixture
        .view
        .complete_publication("Turn completed", None, None, true)?;
    fixture.view.projection.end_execution(false)?;
    let next = fixture.admit(true)?;
    assert!(!fixture.view.observe_event(&old)?);
    run(&fixture.store, &next, &provider()).await?;
    fixture.apply_queued()?;
    assert_eq!(
        fixture
            .view
            .projection
            .items()
            .filter(|item| matches!(item.kind, ViewItemKind::Input))
            .count(),
        2
    );
    assert_eq!(
        fixture
            .view
            .projection
            .items()
            .filter(|item| matches!(item.kind, ViewItemKind::Text))
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn nonhuman_opening_receipt_never_mints_a_local_operator_item() -> TestResult {
    let mut fixture = Fixture::new()?;
    let sender = fixture.admit(false)?;
    run(&fixture.store, &sender, &provider()).await?;
    fixture.apply_queued()?;
    fixture.assert_canonical();
    assert!(
        fixture
            .view
            .publication
            .active
            .as_ref()
            .ok_or("active publication missing")?
            .input
            .is_none()
    );
    assert!(
        fixture
            .view
            .projection
            .items()
            .all(|item| !item.label.as_str().contains("You"))
    );
    Ok(())
}

struct PausedProvider {
    release: Arc<tokio::sync::Notify>,
}

impl Provider for PausedProvider {
    fn stream(
        &self,
        request: norn::provider::ProviderRequest,
    ) -> Result<norn::provider::ProviderStream, norn::provider::ProviderError> {
        use futures_util::StreamExt as _;
        assert_eq!(request.model, "gpt-5.5");
        let release = Arc::clone(&self.release);
        Ok(Box::pin(
            futures_util::stream::iter([Ok(ProviderEvent::TextDelta {
                text: "same answer".to_owned(),
            })])
            .chain(futures_util::stream::once(async move {
                release.notified().await;
                Ok(ProviderEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    response_id: None,
                    usage: Usage {
                        input_tokens: 11,
                        output_tokens: 7,
                        ..Usage::default()
                    },
                })
            })),
        ))
    }
}

#[tokio::test]
async fn observed_pending_ticket_wakes_to_canonical_body_before_late_done() -> TestResult {
    let mut fixture = Fixture::new()?;
    let sender = fixture.admit(true)?;
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = PausedProvider {
        release: Arc::clone(&release),
    };
    let store = Arc::clone(&fixture.store);
    let step = run(&store, &sender, &provider);
    tokio::pin!(step);
    loop {
        tokio::select! {
            result = &mut step => {
                result?;
                return Err("provider completed before explicit release".into());
            }
            event = fixture.receiver.recv() => {
                let event = event?;
                assert!(fixture.view.observe_event(&event)?);
                let reduction = fixture.view.apply_live(&event)?;
                fixture.view.note_completion(&event, &reduction);
                if fixture.view.projection.items().any(|item| matches!(item.kind, ViewItemKind::Text)) { break; }
            }
        }
    }
    assert_eq!(
        fixture
            .view
            .publication
            .active
            .as_ref()
            .ok_or("active publication missing")?
            .attempts
            .len(),
        1
    );
    release.notify_one();
    step.await?;
    fixture.view.drain_publications()?;
    fixture.assert_canonical();
    fixture.apply_queued()?;
    fixture.assert_canonical();
    assert!(
        fixture
            .view
            .publication
            .active
            .as_ref()
            .ok_or("active publication missing")?
            .attempts
            .is_empty()
    );
    Ok(())
}
