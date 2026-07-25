//! Worker survival across a non-transient turn failure (retry-forever
//! design D6).
//!
//! After the unbounded retry brain, a `run_agent_step` error means a
//! failure no replay can fix. That must fail the TURN loudly — never
//! kill a persistent worker, whose mailbox and route are the reason it
//! exists. A panic is the deliberate exception: a poisoned worker is
//! worker-fatal and must not idle-park.

use super::*;

use crate::agent::output::AgentStopReason;
use crate::tools::agent::coord::{SignalAgentTool, WakeAgentTool};
use crate::tools::agent::spawn_outcome::TURN_FAILED_EVENT_TYPE;

/// Provider driving one scripted outcome per turn: either a stream of
/// events, or a hard `stream()` failure standing in for a non-transient
/// provider fault (an expired credential here — [`ErrorClass::Auth`],
/// which no retry policy may ever retry).
///
/// [`ErrorClass::Auth`]: crate::error::ErrorClass::Auth
struct ScriptedTurnProvider {
    turns: StdMutex<Vec<Result<Vec<ProviderEvent>, ProviderError>>>,
    requests: Arc<StdMutex<Vec<ProviderRequest>>>,
}

impl ScriptedTurnProvider {
    fn new(turns: Vec<Result<Vec<ProviderEvent>, ProviderError>>) -> Self {
        Self {
            turns: StdMutex::new(turns),
            requests: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    /// Scripted turns not yet consumed — proof that a worker that was
    /// supposed to stop really stopped.
    fn remaining(&self) -> usize {
        self.turns.lock().len()
    }

    /// Every request the worker actually sent, in order.
    fn requests(&self) -> Vec<ProviderRequest> {
        self.requests.lock().clone()
    }
}

impl Provider for ScriptedTurnProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.requests.lock().push(request);
        let mut turns = self.turns.lock();
        if turns.is_empty() {
            return Err(ProviderError::StreamError {
                reason: "scripted turn provider exhausted".to_string(),
                transient: None,
            });
        }
        match turns.remove(0) {
            Ok(events) => Ok(Box::pin(stream::iter(events.into_iter().map(Ok)))),
            Err(error) => Err(error),
        }
    }
}

fn auth_failure() -> ProviderError {
    ProviderError::AuthenticationFailed {
        reason: "test credential rejected".to_string(),
    }
}

/// D6: a persistent worker whose turn fails non-transiently keeps its
/// mailbox and route, parks idle, and handles the next message it is
/// given. Before the fix the failure mapped to `{Failed, stop: None}` —
/// exactly the controller's terminate predicate — so the worker died on
/// the first hard provider error and every later message had nowhere to
/// go.
#[tokio::test]
async fn persistent_worker_survives_non_transient_turn_failure_and_handles_next_message()
-> TestResult {
    let provider = Arc::new(ScriptedTurnProvider::new(vec![
        Err(auth_failure()),
        Ok(vec![
            ProviderEvent::TextDelta {
                text: "recovered and handled the follow-up".to_string(),
            },
            done_event(),
        ]),
    ]));
    let router = Arc::new(MessageRouter::new());
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        Arc::clone(&provider) as Arc<dyn Provider>,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
        Arc::clone(&router),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let spawn_tool = SpawnAgentTool::new();
    let child_id = spawn_and_join(
        &spawn_tool,
        &ctx,
        json!({"task": "hold the line", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    // The failed turn is reported to the parent with its typed class —
    // and the worker is alive, idle, and routed.
    let failure = rx.try_recv()?;
    assert_eq!(failure.agent_id, child_id);
    assert!(!failure.succeeded, "a failed turn is not a success");
    assert_eq!(
        failure.stop,
        Some(AgentStopReason::TurnFailed {
            class: "auth".to_string(),
        }),
        "the turn failure must carry its house taxonomy class",
    );
    assert!(
        failure
            .error
            .unwrap_or_default()
            .contains("authentication failed"),
        "the loud error keeps the typed reason",
    );
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;
    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Idle,
        "a hard turn failure must not terminate a persistent worker",
    );
    assert!(
        !agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status
            .is_terminal(),
        "the worker's registry entry must not be terminal",
    );

    // And the mailbox still works: the next message is accepted, wakes
    // the worker, and is processed. (The live route itself is registered
    // on demand and torn down at every turn boundary by
    // `transition_live_route`, so the durable proof of survival is that
    // the mailbox still accepts, the wake still re-activates the route,
    // and the queued message reaches the next request.)
    let signal_tool = SignalAgentTool::new();
    let signal_out = signal_tool
        .execute(
            &ToolEnvelope {
                tool_call_id: "signal-after-failure".to_owned(),
                tool_name: "signal_agent".to_owned(),
                model_args: json!({
                    "to": child_id.to_string(),
                    "kind": "steer",
                    "content": "try again please",
                }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert!(!signal_out.is_error(), "{:?}", signal_out.content);
    assert_eq!(signal_out.content["queued"], true);

    let wake_out = WakeAgentTool::new()
        .execute(
            &ToolEnvelope {
                tool_call_id: "wake-after-failure".to_owned(),
                tool_name: "wake_agent".to_owned(),
                model_args: json!({ "agent_id": child_id.to_string() }),
                metadata: serde_json::Value::Null,
            },
            &ctx,
        )
        .await?;
    assert!(!wake_out.is_error(), "{:?}", wake_out.content);

    let resumed = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await?
        .ok_or("required test value")?;
    assert_eq!(resumed.agent_id, child_id);
    assert!(
        resumed.succeeded,
        "the surviving worker completes the next turn: {:?}",
        resumed.error,
    );
    assert!(
        resumed
            .formatted_message
            .contains("recovered and handled the follow-up"),
        "{}",
        resumed.formatted_message,
    );
    assert_eq!(
        provider.remaining(),
        0,
        "both scripted turns must have run: the worker really resumed",
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "the failed turn plus the resumed turn");
    assert!(
        requests[1].messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("try again please"))
        }),
        "the queued message must reach the resumed turn through the live route: {:?}",
        requests[1].messages,
    );
    let infra = ctx
        .get_extension::<AgentToolInfra>()
        .ok_or("required test value")?;
    assert!(
        infra
            .pending_messages
            .messages_for_delivery(child_id)
            .is_empty(),
        "the resumed turn drains the durable mailbox",
    );
    Ok(())
}

/// D6 loudness ruling: a park that leaves no trace is banned. The turn
/// failure lands on the child's OWN durable timeline with its house
/// class label, on the parent's `subagent.completed` audit record, and
/// on the parent-visible result — all three, from disk.
#[tokio::test]
async fn turn_failure_is_recorded_on_the_child_timeline_and_parent_audit() -> TestResult {
    use crate::provider::agent_event::SUBAGENT_COMPLETED_EVENT_TYPE;

    let tmp = tempfile::tempdir()?;
    let provider: Arc<dyn Provider> =
        Arc::new(ScriptedTurnProvider::new(vec![Err(auth_failure())]));
    let agent_registry = AgentRegistry::shared();
    let (ctx, manager, root_session_id) = persistent_parent_ctx(
        tmp.path(),
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(ToolRegistry::new()),
    )?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let child_id = spawn_and_join(
        &SpawnAgentTool::new(),
        &ctx,
        json!({"task": "hold the line", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;
    wait_for_child_status(&ctx, child_id, AgentStatus::Idle).await;

    // The child's own timeline records the failed turn and its class.
    let child_events = events_on_disk(&manager, &child_id.to_string());
    let recorded = child_events
        .iter()
        .find_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == TURN_FAILED_EVENT_TYPE => Some(data.clone()),
            _ => None,
        })
        .ok_or("the parked worker must record its failed turn on its own timeline")?;
    assert_eq!(recorded["class"], "auth");
    assert!(
        recorded["error"]
            .as_str()
            .unwrap_or_default()
            .contains("authentication failed"),
        "the durable record keeps the typed reason: {recorded}",
    );

    // The parent's audit record carries the same typed stop.
    let parent_events = events_on_disk(&manager, &root_session_id);
    let completed = parent_events
        .iter()
        .find_map(|event| match event {
            SessionEvent::Custom {
                event_type, data, ..
            } if event_type == SUBAGENT_COMPLETED_EVENT_TYPE => Some(data.clone()),
            _ => None,
        })
        .ok_or("the parent must carry the subagent.completed audit record")?;
    assert_eq!(completed["succeeded"], false);
    assert_eq!(completed["stop"]["reason"], "turn_failed");
    assert_eq!(completed["stop"]["class"], "auth");

    // And the parent-visible result surface agrees.
    let failure = rx.try_recv()?;
    assert_eq!(
        failure.stop,
        Some(AgentStopReason::TurnFailed {
            class: "auth".to_string(),
        }),
    );
    Ok(())
}

/// D6 ruling 1: panics stay worker-fatal. A poisoned worker must never
/// idle-park — it terminates, its route is gone, and its result carries
/// no typed stop (the run did not stop early; it was cut down).
#[tokio::test]
async fn panicking_persistent_worker_stays_worker_fatal() -> TestResult {
    struct PanickingTool;

    #[async_trait]
    impl TestTool for PanickingTool {
        fn name(&self) -> &'static str {
            "explode"
        }
        fn description(&self) -> &'static str {
            "panics on execute (test stand-in for a panicking dependency)"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({})
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }
        async fn execute(
            &self,
            envelope: &ToolEnvelope,
            _ctx: &ToolContext,
        ) -> Result<TestToolOutput, ToolError> {
            assert!(
                envelope.tool_name.is_empty(),
                "dependency panic inside child tool",
            );
            Ok(TestToolOutput::success(json!({})))
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::ToolCallDelta {
            item_id: "tc-panic".to_string(),
            call_id: None,
            name: Some("explode".to_string()),
            arguments_delta: "{}".to_string(),
            kind: crate::provider::request::ToolCallKind::Function,
        },
        done_event_tool_use(),
    ]]));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(PanickingTool));
    let router = Arc::new(MessageRouter::new());
    let agent_registry = AgentRegistry::shared();
    let ctx = parent_ctx(
        provider,
        Uuid::new_v4(),
        &agent_registry,
        Arc::new(registry),
        Arc::clone(&router),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));

    let child_id = spawn_and_join(
        &SpawnAgentTool::new(),
        &ctx,
        json!({"task": "boom", "model": CATALOG_MODEL, "role": "worker"}),
    )
    .await;

    assert_eq!(
        agent_registry
            .read()
            .get(child_id)
            .ok_or("required test value")?
            .status,
        AgentStatus::Failed,
        "a panicked worker terminates — it must never idle-park",
    );
    assert!(
        !router.is_routed(child_id),
        "the terminated worker's route is deregistered",
    );
    let result = rx.try_recv()?;
    assert!(!result.succeeded);
    assert_eq!(
        result.stop, None,
        "a panic is not an early stop: it carries no typed stop reason",
    );
    assert!(
        result
            .error
            .unwrap_or_default()
            .contains("panicked before completing"),
    );
    Ok(())
}
