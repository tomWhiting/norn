//! Opt-in linger-await at the agent loop's would-stop boundaries.
//!
//! Wave 3, DECISION M3: by default the loop's inbound and child-result
//! drains are non-blocking and `run_agent_step` returns the moment the
//! model would stop — there is no idle state, and a child result that
//! arrives after the parent returned is sent into a dropped channel
//! (error-logged, never delivered). When an agent is configured to
//! linger ([`AgentLoopConfig::linger`](super::config::AgentLoopConfig)),
//! its would-stop boundaries await (child-result channel ∪ inbound
//! steer ∪ cancellation token) up to the configured deadline instead of
//! returning, so late child results and steer messages are delivered
//! and processed instead of orphaned.
//!
//! One codepath: everything that arrives is injected by exactly the
//! same `super::delivery` helpers a mid-run drain uses —
//! `flush_inbound_messages` for inbound messages and
//! `drain_child_results` for child results. The linger only decides
//! *when* those run again; it never delivers anything itself.
//!
//! Deadline semantics: **total per linger entry**. Each time the loop
//! reaches a would-stop boundary with nothing buffered, a fresh
//! deadline of `policy.deadline` is armed and the await runs at most
//! that long. A wake that injects work continues the loop, and the
//! *next* would-stop boundary arms a fresh deadline. (The Wave 3
//! design doc does not specify fresh-per-entry vs. total-across-
//! entries; total-per-entry is the choice here, flagged in the
//! implementation report.)
//!
//! Inbound wake (DECISION M2): a
//! [`MessageKind::Steer`](super::inbound::MessageKind) wakes the linger
//! the moment it is enqueued, via
//! [`InboundChannel::steer_ready`] — a peek-buffer readiness await that
//! never consumes a message, so the boundary sweep stays the one
//! consumer. A [`MessageKind::Update`](super::inbound::MessageKind)
//! deliberately does **not** wake the linger; it buffers and is
//! injected by the final sweep when the deadline expires (the linger's
//! stop time), exactly matching Update's established
//! buffer-until-the-model-would-stop semantics.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::agent::result_channel::ChildAgentResult;
use crate::error::SessionError;
use crate::r#loop::inbound::{ChannelMessage, InboundChannel};
use crate::r#loop::loop_context::LoopContext;
use crate::provider::agent_event::AgentEventSender;
use crate::provider::request::Message;
use crate::session::store::EventStore;

use super::delivery::{drain_child_results, flush_active_inputs, flush_inbound_messages};

/// Opt-in policy making an agent wait at its would-stop boundaries for
/// late inbound messages and child results instead of returning
/// immediately (Wave 3, DECISION M3).
///
/// There is no default duration: the policy is builder/spawn-configured
/// (it rides on [`AgentLoopConfig`](super::config::AgentLoopConfig),
/// which [`AgentBuilder::agent_config`](crate::agent::AgentBuilder::agent_config)
/// carries), and an unset policy (`None`) preserves the
/// return-immediately behavior exactly.
///
/// The linger await runs *inside* the step, so it counts toward a
/// configured `step_timeout` (wall-clock inside the step) and a fired
/// cancellation token ends it promptly with the established
/// `Cancelled` semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LingerPolicy {
    /// Maximum wall-clock time to wait at one would-stop boundary
    /// before stopping with the same semantics as an unset policy.
    /// Total per linger entry: each boundary that lingers arms a fresh
    /// deadline of this duration.
    pub deadline: Duration,
}

/// What ended one linger await.
enum LingerWake {
    /// A child result arrived; it must be injected through
    /// [`drain_child_results`] (as the seed of the drained batch).
    ChildResult(Box<ChildAgentResult>),
    /// A steer message is buffered on the inbound channel
    /// ([`InboundChannel::steer_ready`] resolved); the boundary sweep
    /// drains and injects it.
    InboundSteer,
    /// The deadline elapsed with nothing delivered to the wake set.
    DeadlineExpired,
    /// The cancellation token fired.
    Cancelled,
}

/// Resolution of a would-stop boundary, telling the runner how to
/// proceed.
pub(super) enum BoundaryOutcome {
    /// Inbound messages or child results were injected into the
    /// conversation; the loop must run another iteration.
    Continue,
    /// Nothing is pending (and the configured linger, if any, expired
    /// empty): the loop proceeds to its stop hooks and returns, exactly
    /// as it does with no linger configured.
    Stop,
    /// The cancellation token fired during the linger await; the loop
    /// must return `Cancelled` with the usage accumulated so far.
    Cancelled,
}

/// Borrowed loop state for resolving one would-stop boundary.
pub(super) struct StopBoundary<'a> {
    /// Session event store messages are persisted into.
    pub(super) store: &'a EventStore,
    /// Live conversation messages injected work is appended to.
    pub(super) messages: &'a mut Vec<Message>,
    /// Inbound channel drained by the boundary sweeps.
    pub(super) inbound: Option<&'a mut InboundChannel>,
    /// Buffered follow-up messages flushed at stop boundaries.
    pub(super) follow_up_buffer: &'a mut Vec<ChannelMessage>,
    /// Loop context carrying the child-result receiver and hooks.
    pub(super) loop_context: &'a mut LoopContext,
    /// The step's linger policy; `None` preserves return-immediately.
    pub(super) linger: Option<LingerPolicy>,
    /// The step's cooperative cancellation token, when configured.
    pub(super) cancel: Option<&'a CancellationToken>,
    /// The step's live event channel; injected messages broadcast their
    /// `agent_message.delivered` audit half on it when present.
    pub(super) event_tx: Option<&'a AgentEventSender>,
}

/// Resolve one would-stop boundary: run the pre-existing non-blocking
/// sweep (inbound flush, then child-result drain), and — only when a
/// [`LingerPolicy`] is configured and the sweep found nothing — await
/// the wake set up to the deadline.
///
/// With `linger: None` this is behavior-identical to the historical
/// inline boundary code: sweep, and stop if the sweep found nothing.
///
/// # Errors
///
/// Propagates [`SessionError`] from persisting injected messages.
pub(super) async fn resolve_stop_boundary(
    boundary: StopBoundary<'_>,
) -> Result<BoundaryOutcome, SessionError> {
    let StopBoundary {
        store,
        messages,
        mut inbound,
        follow_up_buffer,
        loop_context,
        linger,
        cancel,
        event_tx,
    } = boundary;

    if sweep(
        store,
        messages,
        inbound.as_deref_mut(),
        follow_up_buffer,
        loop_context,
        event_tx,
    )
    .await?
    {
        return Ok(BoundaryOutcome::Continue);
    }

    let Some(policy) = linger else {
        return Ok(BoundaryOutcome::Stop);
    };

    match await_linger_wake(
        loop_context.child_result_rx.as_mut(),
        &mut inbound,
        cancel,
        policy.deadline,
    )
    .await
    {
        LingerWake::Cancelled => Ok(BoundaryOutcome::Cancelled),
        LingerWake::ChildResult(first) => {
            // The awaited recv consumed one result; inject it as the
            // seed of the same drain call every other delivery uses,
            // batching any results that arrived behind it.
            drain_child_results(
                store,
                messages,
                loop_context.child_result_rx.as_mut(),
                loop_context.hooks.as_deref(),
                Some(*first),
                &loop_context.children_usage,
            )
            .await?;
            Ok(BoundaryOutcome::Continue)
        }
        LingerWake::InboundSteer => {
            // `steer_ready` buffered a steer without consuming it; this
            // sweep drains and injects it through the same flush path a
            // mid-run drain uses.
            if sweep(
                store,
                messages,
                inbound,
                follow_up_buffer,
                loop_context,
                event_tx,
            )
            .await?
            {
                Ok(BoundaryOutcome::Continue)
            } else {
                // Structurally unreachable: `steer_ready` resolves true
                // only after buffering a steer, which this sweep's flush
                // injects unconditionally. Stop rather than loop on a
                // broken invariant — and say so.
                tracing::error!(
                    "linger inbound wake found nothing to inject; \
                     stopping as if the deadline expired",
                );
                Ok(BoundaryOutcome::Stop)
            }
        }
        LingerWake::DeadlineExpired => {
            // Updates deliberately do not wake the linger (DECISION
            // M2): anything that buffered while we waited is delivered
            // by this final sweep — the linger's stop time — instead of
            // being orphaned. An empty sweep stops exactly as if the
            // linger were unset.
            if sweep(
                store,
                messages,
                inbound,
                follow_up_buffer,
                loop_context,
                event_tx,
            )
            .await?
            {
                Ok(BoundaryOutcome::Continue)
            } else {
                Ok(BoundaryOutcome::Stop)
            }
        }
    }
}

/// The non-blocking stop-boundary sweep: flush inbound steer messages
/// and buffered follow-ups, then drain pending child results — the
/// exact calls (and order) the runner's stop boundaries have always
/// made. Returns `true` when anything was injected.
async fn sweep(
    store: &EventStore,
    messages: &mut Vec<Message>,
    inbound: Option<&mut InboundChannel>,
    follow_up_buffer: &mut Vec<ChannelMessage>,
    loop_context: &mut LoopContext,
    event_tx: Option<&AgentEventSender>,
) -> Result<bool, SessionError> {
    if !flush_inbound_messages(
        store,
        messages,
        inbound,
        follow_up_buffer,
        loop_context.hooks.as_deref(),
        event_tx,
    )
    .await?
    .is_empty()
    {
        return Ok(true);
    }
    if !flush_active_inputs(
        store,
        messages,
        loop_context.active_input_rx.as_mut(),
        loop_context.hooks.as_deref(),
    )
    .await?
    .is_empty()
    {
        return Ok(true);
    }
    drain_child_results(
        store,
        messages,
        loop_context.child_result_rx.as_mut(),
        loop_context.hooks.as_deref(),
        None,
        &loop_context.children_usage,
    )
    .await
}

/// Await the linger wake set: cancellation, child-result arrival,
/// inbound steer arrival, or deadline expiry — a biased select matching
/// the runner's existing cancellation-first style. No busy-waiting: a
/// closed child-result channel (every sender dropped) disables that
/// arm, an exhausted closed inbound channel disables its arm, and the
/// await keeps sleeping toward the deadline.
async fn await_linger_wake(
    mut child_rx: Option<&mut tokio::sync::mpsc::Receiver<ChildAgentResult>>,
    inbound: &mut Option<&mut InboundChannel>,
    cancel: Option<&CancellationToken>,
    deadline: Duration,
) -> LingerWake {
    // Structurally empty wake set: no cancel token, no child-result
    // channel, no inbound channel — nothing can EVER arrive, so the
    // sleep would be pure dead wall-clock at every would-stop boundary
    // (acute on the rhai surface, whose script children run with none
    // of the three). Expire immediately instead of serving the full
    // deadline; the boundary sweep after the wake is still a no-op-safe
    // pass (REVIEW R5 MEDIUM-1). The grant itself stays visible in the
    // registry — this short-circuits the wait, not the policy.
    if cancel.is_none() && child_rx.is_none() && inbound.is_none() {
        return LingerWake::DeadlineExpired;
    }
    let sleep = tokio::time::sleep(deadline);
    tokio::pin!(sleep);
    // Disabling the inbound arm must not consume `*inbound` — the
    // boundary sweep that follows the wake still drains the channel's
    // peek buffer (a closed channel can still hold buffered updates).
    let mut inbound_open = inbound.is_some();
    loop {
        tokio::select! {
            biased;
            () = cancelled_or_pending(cancel) => return LingerWake::Cancelled,
            result = recv_or_pending(&mut child_rx) => match result {
                Some(result) => return LingerWake::ChildResult(Box::new(result)),
                // Closed channel: every sender is gone and no result
                // will ever arrive. Disable the arm (recv would keep
                // resolving `None` instantly, spinning this loop) and
                // keep waiting on the other wake sources.
                None => child_rx = None,
            },
            ready = steer_or_pending(inbound, inbound_open) => {
                if ready {
                    return LingerWake::InboundSteer;
                }
                // Exhausted closed inbound channel: no steer will ever
                // arrive. Same disable-don't-spin treatment.
                inbound_open = false;
            },
            () = &mut sleep => return LingerWake::DeadlineExpired,
        }
    }
}

/// Resolve when the token fires; never resolve when no token is
/// configured.
async fn cancelled_or_pending(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Receive from the child-result channel; never resolve when the arm
/// is absent or has been disabled. `Receiver::recv` is cancel-safe, so
/// re-creating this future on every select iteration loses nothing.
async fn recv_or_pending(
    rx: &mut Option<&mut tokio::sync::mpsc::Receiver<ChildAgentResult>>,
) -> Option<ChildAgentResult> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Await inbound-steer readiness; never resolve when no channel is
/// configured or the arm has been disabled (`open == false`).
/// [`InboundChannel::steer_ready`] is cancel-safe (received messages
/// land in its peek buffer before any await point), so re-creating
/// this future on every select iteration loses nothing.
async fn steer_or_pending(inbound: &mut Option<&mut InboundChannel>, open: bool) -> bool {
    match inbound {
        Some(ch) if open => ch.steer_ready().await,
        _ => std::future::pending().await,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use chrono::Utc;
    use serde_json::Value;
    use uuid::Uuid;

    use crate::r#loop::config::{AgentLoopConfig, AgentStepResult, MockToolExecutor};
    use crate::r#loop::inbound::{MessageKind, inbound_channel};
    use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::usage::Usage;
    use crate::session::events::SessionEvent;
    use crate::session::store::EventStore;

    // -- Harness -----------------------------------------------------

    fn text_turn(text: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::TextDelta {
                text: text.to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]
    }

    fn schema_turn(answer: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::ToolCallDelta {
                item_id: format!("tc-{answer}"),
                name: Some("structured_output".to_string()),
                arguments_delta: format!(r#"{{"answer":"{answer}"}}"#),
                kind: crate::provider::request::ToolCallKind::Function,
            },
            ProviderEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]
    }

    fn child_result(role: &str, message: &str) -> ChildAgentResult {
        ChildAgentResult {
            agent_id: Uuid::new_v4(),
            agent_role: role.to_string(),
            succeeded: true,
            formatted_message: message.to_string(),
            error: None,
            stop: None,
            usage: Usage::default(),
            subtree_usage: Usage::default(),
        }
    }

    fn linger_config(deadline: Duration) -> AgentLoopConfig {
        AgentLoopConfig {
            linger: Some(LingerPolicy { deadline }),
            ..AgentLoopConfig::default()
        }
    }

    struct Run<'a> {
        provider: &'a MockProvider,
        store: &'a EventStore,
        config: &'a AgentLoopConfig,
        loop_context: &'a mut crate::r#loop::loop_context::LoopContext,
        schema: Option<&'a Value>,
        inbound: Option<&'a mut InboundChannel>,
        cancel: Option<CancellationToken>,
    }

    async fn run_step(run: Run<'_>) -> AgentStepResult {
        let executor = MockToolExecutor::empty();
        run_agent_step(AgentStepRequest {
            provider: run.provider,
            executor: &executor,
            store: run.store,
            user_prompt: "prompt",
            tools: &[],
            output_schema: run.schema,
            model: "test-model",
            config: run.config,
            event_tx: None,
            inbound: run.inbound,
            loop_context: run.loop_context,
            cancel: run.cancel,
        })
        .await
        .expect("run_agent_step")
    }

    fn store_has_user_message_containing(store: &EventStore, needles: &[&str]) -> bool {
        store.events().iter().any(|e| {
            matches!(
                e,
                SessionEvent::UserMessage { content, .. }
                    if needles.iter().all(|n| content.contains(n))
            )
        })
    }

    // -- Unit tests on the wake primitive ------------------------------

    #[tokio::test]
    async fn cancelled_token_wins_over_buffered_child_result() {
        let token = CancellationToken::new();
        token.cancel();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send(child_result("spawn/worker", "done"))
            .await
            .expect("send");
        let wake = await_linger_wake(
            Some(&mut rx),
            &mut None,
            Some(&token),
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(wake, LingerWake::Cancelled));
    }

    #[tokio::test]
    async fn buffered_child_result_is_returned() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tx.send(child_result("spawn/worker", "done"))
            .await
            .expect("send");
        let wake = await_linger_wake(Some(&mut rx), &mut None, None, Duration::from_secs(5)).await;
        let LingerWake::ChildResult(result) = wake else {
            panic!("expected ChildResult wake");
        };
        assert_eq!(result.agent_role, "spawn/worker");
    }

    #[tokio::test(start_paused = true)]
    async fn child_result_arriving_mid_await_wakes_before_deadline() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(child_result("spawn/worker", "late")).await
        });
        let wake = await_linger_wake(Some(&mut rx), &mut None, None, Duration::from_hours(1)).await;
        assert!(matches!(wake, LingerWake::ChildResult(_)));
        sender.await.expect("join").expect("send");
    }

    /// REVIEW R5 MEDIUM-1: a structurally empty wake set (no cancel
    /// token, no child channel, no inbound) can never be woken — the
    /// linger expires IMMEDIATELY, consuming none of the deadline,
    /// instead of serving an hour of uninterruptible dead wall-clock at
    /// every would-stop boundary (the rhai surface hits exactly this
    /// shape). Pinned via virtual time: zero clock advance.
    #[tokio::test(start_paused = true)]
    async fn no_wake_sources_expires_at_deadline() {
        let before = tokio::time::Instant::now();
        let wake = await_linger_wake(None, &mut None, None, Duration::from_hours(1)).await;
        assert!(matches!(wake, LingerWake::DeadlineExpired));
        assert_eq!(
            tokio::time::Instant::now(),
            before,
            "an unwakeable linger must not consume the deadline",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn closed_child_channel_disables_arm_and_expires() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChildAgentResult>(4);
        drop(tx);
        let wake =
            await_linger_wake(Some(&mut rx), &mut None, None, Duration::from_millis(100)).await;
        assert!(matches!(wake, LingerWake::DeadlineExpired));
    }

    fn steer_message(content: &str, kind: MessageKind) -> ChannelMessage {
        ChannelMessage {
            id: Uuid::new_v4(),
            sender_id: Uuid::new_v4(),
            from: "/p/sender".to_string(),
            role: None,
            to_id: Uuid::new_v4(),
            content: content.to_string(),
            kind,
            seq: None,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn buffered_steer_wakes_inbound_arm_before_deadline() {
        let (tx, mut ch) = inbound_channel(4);
        tx.send(steer_message("act", MessageKind::Steer))
            .await
            .expect("send");

        let start = tokio::time::Instant::now();
        let wake = await_linger_wake(None, &mut Some(&mut ch), None, Duration::from_hours(1)).await;
        assert!(matches!(wake, LingerWake::InboundSteer));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a buffered steer must wake immediately, not at the deadline",
        );
        assert_eq!(
            ch.drain().len(),
            1,
            "the wake must not consume the message — the sweep drains it",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn steer_arriving_mid_await_wakes_inbound_arm() {
        let (tx, mut ch) = inbound_channel(4);
        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(steer_message("late act", MessageKind::Steer)).await
        });
        let wake = await_linger_wake(None, &mut Some(&mut ch), None, Duration::from_hours(1)).await;
        assert!(matches!(wake, LingerWake::InboundSteer));
        sender.await.expect("join").expect("send");
    }

    #[tokio::test(start_paused = true)]
    async fn update_does_not_wake_inbound_arm() {
        let (tx, mut ch) = inbound_channel(4);
        tx.send(steer_message("fyi", MessageKind::Update))
            .await
            .expect("send");

        let start = tokio::time::Instant::now();
        let wake =
            await_linger_wake(None, &mut Some(&mut ch), None, Duration::from_millis(100)).await;
        assert!(
            matches!(wake, LingerWake::DeadlineExpired),
            "an update must not wake the linger (DECISION M2)",
        );
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert_eq!(
            ch.drain().len(),
            1,
            "the buffered update rides out with the post-expiry sweep",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn closed_inbound_channel_disables_arm_and_expires() {
        let (tx, mut ch) = inbound_channel(4);
        drop(tx);
        let wake =
            await_linger_wake(None, &mut Some(&mut ch), None, Duration::from_millis(100)).await;
        assert!(matches!(wake, LingerWake::DeadlineExpired));
    }

    // -- Step-level tests -------------------------------------------

    /// Regression test on the orphaned-late-child-result gap: with a
    /// linger configured the parent's loop is still alive (receiver
    /// not dropped) when the late result is sent, the send succeeds,
    /// and the result is injected and processed — another iteration
    /// runs and the final output reflects it.
    #[tokio::test(start_paused = true)]
    async fn child_result_during_linger_is_delivered_and_loop_continues() {
        let provider = MockProvider::new(vec![text_turn("first"), text_turn("second")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        ctx.child_result_rx = Some(rx);

        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut late = child_result("spawn/worker", "child finished");
            late.subtree_usage = Usage {
                input_tokens: 9,
                output_tokens: 4,
                ..Usage::default()
            };
            tx.send(late).await
        });

        let config = linger_config(Duration::from_mins(1));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: None,
            cancel: None,
        })
        .await;

        // The late send found a live receiver — the doc's cited gap
        // (spawn.rs error-logged send into a dropped channel) is closed.
        sender
            .await
            .expect("sender task")
            .expect("child-result send must succeed while the parent lingers");

        let AgentStepResult::Completed {
            output,
            children_usage,
            ..
        } = result
        else {
            panic!("expected Completed");
        };
        assert_eq!(output, Value::String("second".to_string()));
        // W3.6: the linger's seeded delivery goes through the same drain
        // path as a mid-run delivery, so the seed's subtree usage is
        // folded into the step's children_usage.
        assert_eq!(
            children_usage.input_tokens, 9,
            "the linger-seeded result's subtree usage must be folded",
        );
        assert_eq!(children_usage.output_tokens, 4);
        assert_eq!(
            provider.call_count(),
            2,
            "loop must continue after the wake"
        );
        assert!(
            store_has_user_message_containing(&store, &["spawn/worker", "child finished"]),
            "child result must be injected through the normal drain path",
        );
    }

    /// A message arriving during the linger is processed through the
    /// same flush path as a mid-run message and the loop continues,
    /// then lingers again until its (fresh) deadline.
    #[tokio::test(start_paused = true)]
    async fn message_during_linger_is_processed_and_loop_continues() {
        let provider = MockProvider::new(vec![text_turn("first"), text_turn("second")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let (tx, mut inbound) = inbound_channel(4);

        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(ChannelMessage {
                id: Uuid::new_v4(),
                sender_id: Uuid::new_v4(),
                from: "parent".to_string(),
                role: None,
                to_id: Uuid::new_v4(),
                content: "steer now".to_string(),
                kind: MessageKind::Steer,
                seq: None,
                timestamp: Utc::now(),
            })
            .await
        });

        let config = linger_config(Duration::from_millis(200));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: Some(&mut inbound),
            cancel: None,
        })
        .await;

        sender.await.expect("sender task").expect("send");

        let AgentStepResult::Completed { output, .. } = result else {
            panic!("expected Completed");
        };
        assert_eq!(output, Value::String("second".to_string()));
        assert_eq!(
            provider.call_count(),
            2,
            "the flushed message must trigger another iteration",
        );
        assert!(
            store_has_user_message_containing(&store, &["from=\"parent\"", "steer now"]),
            "message must be injected through the same framed flush path as mid-run",
        );
    }

    /// An update arriving during the linger does not wake it: the
    /// deadline runs its full course, then the expiry sweep injects the
    /// buffered update (the linger's stop time is Update's established
    /// injection point) and the loop continues.
    #[tokio::test(start_paused = true)]
    async fn update_during_linger_is_delivered_at_deadline_and_loop_continues() {
        let provider = MockProvider::new(vec![text_turn("first"), text_turn("second")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let (tx, mut inbound) = inbound_channel(4);

        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(ChannelMessage {
                id: Uuid::new_v4(),
                sender_id: Uuid::new_v4(),
                from: "/p/observer".to_string(),
                role: None,
                to_id: Uuid::new_v4(),
                content: "fyi only".to_string(),
                kind: MessageKind::Update,
                seq: None,
                timestamp: Utc::now(),
            })
            .await
        });

        let start = tokio::time::Instant::now();
        let config = linger_config(Duration::from_millis(200));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: Some(&mut inbound),
            cancel: None,
        })
        .await;
        sender.await.expect("sender task").expect("send");

        assert!(
            start.elapsed() >= Duration::from_millis(200),
            "an update must not cut the linger short",
        );
        let AgentStepResult::Completed { output, .. } = result else {
            panic!("expected Completed");
        };
        assert_eq!(output, Value::String("second".to_string()));
        assert_eq!(
            provider.call_count(),
            2,
            "the expiry sweep must inject the update and continue the loop",
        );
        assert!(
            store_has_user_message_containing(&store, &["/p/observer", "fyi only"]),
            "the update must be injected through the framed flush path",
        );
    }

    /// The live event carrier: a router-sequenced message flushed at a
    /// stop boundary broadcasts its `agent_message.delivered` half on
    /// the step's event channel, mirroring the store-side audit event.
    #[tokio::test]
    async fn routed_message_delivery_broadcasts_live_delivered_event() {
        use crate::provider::agent_event::{
            AgentEvent, AgentEventKind, AgentEventSender, AgentMessageLifecycle,
        };

        let provider = MockProvider::new(vec![text_turn("first"), text_turn("second")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let (tx, mut inbound) = inbound_channel(4);

        let recipient_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        let sender_id = Uuid::new_v4();
        tx.send(ChannelMessage {
            id: message_id,
            sender_id,
            from: "/p/peer".to_string(),
            role: None,
            to_id: recipient_id,
            content: "routed steer".to_string(),
            kind: MessageKind::Steer,
            seq: Some(7),
            timestamp: Utc::now(),
        })
        .await
        .expect("send");

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<AgentEvent>(16);
        let sender = AgentEventSender::new(event_tx, recipient_id, "root".to_string());

        let executor = MockToolExecutor::empty();
        let config = AgentLoopConfig::default();
        let result = run_agent_step(AgentStepRequest {
            provider: &provider,
            executor: &executor,
            store: &store,
            user_prompt: "prompt",
            tools: &[],
            output_schema: None,
            model: "test-model",
            config: &config,
            event_tx: Some(&sender),
            inbound: Some(&mut inbound),
            loop_context: &mut ctx,
            cancel: None,
        })
        .await
        .expect("run_agent_step");
        assert!(matches!(result, AgentStepResult::Completed { .. }));

        let mut delivered = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let AgentEventKind::Message(AgentMessageLifecycle::Delivered {
                message_id: mid,
                seq,
                to_id,
                ..
            }) = event.event
            {
                delivered.push((mid, seq, to_id));
            }
        }
        assert_eq!(
            delivered,
            vec![(message_id, 7, recipient_id)],
            "exactly one live Delivered must broadcast, paired to the routed send",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_during_linger_returns_cancelled() {
        let provider = MockProvider::new(vec![text_turn("only")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let token = CancellationToken::new();
        let trigger = token.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });

        // Deadline far beyond the cancel: a prompt return proves the
        // token ended the linger, not the deadline.
        let config = linger_config(Duration::from_hours(1));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: None,
            cancel: Some(token),
        })
        .await;
        canceller.await.expect("canceller task");

        let AgentStepResult::Cancelled { usage, .. } = result else {
            panic!("expected Cancelled");
        };
        assert_eq!(
            usage.input_tokens, 10,
            "accumulated usage rides on Cancelled"
        );
        assert_eq!(provider.call_count(), 1);
    }

    /// W3.5 cascade × linger: the run holds a `child_token()` of an
    /// ancestor's token (exactly how spawn/fork parent every child's run
    /// token), and the *ancestor's* token is the one cancelled while the
    /// run lingers at its stop boundary. The cascaded cancel must end
    /// the linger promptly — long before the deadline — with the
    /// established `Cancelled` semantics (accumulated usage attached),
    /// so a cancelled mid-tree agent never sits out its linger window.
    #[tokio::test(start_paused = true)]
    async fn cascaded_ancestor_cancel_during_linger_returns_cancelled_promptly() {
        let provider = MockProvider::new(vec![text_turn("only")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let ancestor = CancellationToken::new();
        let own = ancestor.child_token();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ancestor.cancel();
        });

        // Deadline far beyond the cancel: a prompt Cancelled return
        // proves the cascaded token ended the linger, not the deadline
        // (deadline expiry would return Completed here).
        let config = linger_config(Duration::from_hours(1));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: None,
            cancel: Some(own),
        })
        .await;
        canceller.await.expect("canceller task");

        let AgentStepResult::Cancelled { usage, .. } = result else {
            panic!("expected Cancelled, got {result:?}");
        };
        assert_eq!(
            usage.input_tokens, 10,
            "accumulated usage rides on the cascaded Cancelled too"
        );
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expiry_stops_exactly_like_unset_linger() {
        // Lingering run: deadline expires with nothing arriving.
        let provider = MockProvider::new(vec![text_turn("only")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let config = linger_config(Duration::from_millis(50));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: None,
            cancel: None,
        })
        .await;

        // Baseline run: identical script, no linger.
        let base_provider = MockProvider::new(vec![text_turn("only")]);
        let base_store = EventStore::new();
        let mut base_ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let base_config = AgentLoopConfig::default();
        let base_result = run_step(Run {
            provider: &base_provider,
            store: &base_store,
            config: &base_config,
            loop_context: &mut base_ctx,
            schema: None,
            inbound: None,
            cancel: None,
        })
        .await;

        let AgentStepResult::Completed { output, usage, .. } = result else {
            panic!("expected Completed");
        };
        let AgentStepResult::Completed {
            output: base_output,
            usage: base_usage,
            ..
        } = base_result
        else {
            panic!("expected Completed baseline");
        };
        assert_eq!(output, base_output);
        assert_eq!(usage.input_tokens, base_usage.input_tokens);
        assert_eq!(usage.output_tokens, base_usage.output_tokens);
        assert_eq!(provider.call_count(), base_provider.call_count());
        assert_eq!(
            store.len(),
            base_store.len(),
            "expired linger must leave an identical event stream",
        );
    }

    /// One-codepath across boundary arms: the schema-mode stop boundary
    /// (`SchemaValid`) lingers and continues on a child result exactly
    /// like the plain-text boundary.
    #[tokio::test(start_paused = true)]
    async fn schema_valid_boundary_lingers_and_continues_on_child_result() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"]
        });
        let provider = MockProvider::new(vec![schema_turn("first"), schema_turn("second")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        ctx.child_result_rx = Some(rx);

        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(child_result("fork/reviewer", "review done")).await
        });

        let config = linger_config(Duration::from_mins(1));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: Some(&schema),
            inbound: None,
            cancel: None,
        })
        .await;

        sender.await.expect("sender task").expect("send");

        let AgentStepResult::Completed { output, .. } = result else {
            panic!("expected Completed");
        };
        assert_eq!(output["answer"], "second");
        assert_eq!(provider.call_count(), 2);
        assert!(store_has_user_message_containing(
            &store,
            &["fork/reviewer", "review done"],
        ));
    }

    /// A child whose result channel closes while the parent lingers
    /// (every sender dropped, nothing in flight) must not wedge or spin
    /// the boundary: the deadline expires and the step completes.
    #[tokio::test(start_paused = true)]
    async fn closed_child_channel_during_step_expires_cleanly() {
        let provider = MockProvider::new(vec![text_turn("only")]);
        let store = EventStore::new();
        let mut ctx = crate::r#loop::loop_context::LoopContext::new("system");
        let (tx, rx) = tokio::sync::mpsc::channel::<ChildAgentResult>(4);
        ctx.child_result_rx = Some(rx);
        drop(tx);

        let config = linger_config(Duration::from_millis(100));
        let result = run_step(Run {
            provider: &provider,
            store: &store,
            config: &config,
            loop_context: &mut ctx,
            schema: None,
            inbound: None,
            cancel: None,
        })
        .await;

        let AgentStepResult::Completed { output, .. } = result else {
            panic!("expected Completed");
        };
        assert_eq!(output, Value::String("only".to_string()));
    }
}
