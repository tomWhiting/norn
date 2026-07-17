//! Auto-compaction trigger and shared timeout state (N-023 R4 + R2).
//!
//! `maybe_auto_compact` ties the client-side token estimate from
//! [`crate::agent_loop::tokens`] to the plan/commit compaction API on
//! [`ContextEdits`]. When the trigger fires, the loop asks the step's own
//! provider and model for an LLM-written summary of the events being
//! elided (the loop summarization module); the summary becomes the
//! compaction record's content so the model keeps semantic continuity. A
//! summarization failure never aborts the step: it is logged at `warn`
//! and the mechanical event digest is committed instead, explicitly
//! marked as a non-semantic fallback. The trigger fires once per
//! `run_agent_step` call — the runner threads a [`CompactionState`]
//! through the loop body so a second over-threshold iteration is a no-op.
//!
//! `TimeoutState` is the shared mutable handle the runner exposes to the
//! `tokio::time::timeout` wrapper; on cancellation the outer scope reads
//! the most recent assistant text and iteration count to populate
//! [`crate::agent_loop::runner::AgentStepResult::TimedOut`].

use std::sync::Arc;

use parking_lot::Mutex;

use crate::error::SessionError;
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::summarization::request_compaction_summary;
use crate::r#loop::tokens::TokenEstimator;
use crate::provider::traits::Provider;
use crate::provider::usage::Usage;
use crate::session::context_edit::{AutoCompactionOutcome, ContextEdits, build_compaction_digest};
use crate::session::events::SessionEvent;
use crate::session::store::EventStore;

/// Where the committed compaction summary came from.
#[derive(Debug)]
pub enum CompactionSummarySource {
    /// The provider wrote a semantic summary of the elided events.
    Llm,
    /// LLM summarization failed or produced an unusable response; the
    /// mechanical event digest was committed instead, marked as a
    /// non-semantic fallback.
    MechanicalDigestFallback {
        /// Why the LLM summary was unavailable.
        error: String,
    },
}

/// Result of a fired auto-compaction trigger.
///
/// Carries the [`AutoCompactionOutcome`] (the appended compaction event ID
/// and the events it newly hid from the prompt view) so the runner can
/// apply the compaction to the in-flight request, the freed-token estimate
/// for logging, and the summarization outcome: where the summary came from
/// and what the summarization call cost. The caller must fold
/// `summarization_usage` into the step's usage accounting.
#[derive(Debug)]
pub struct AutoCompactionRun {
    /// The compaction event and newly superseded event IDs.
    pub outcome: AutoCompactionOutcome,
    /// Tokens the trigger estimated it freed (estimate minus threshold).
    pub freed_token_estimate: usize,
    /// Whether the committed summary is LLM-written or the digest fallback.
    pub summary_source: CompactionSummarySource,
    /// Usage of the summarization provider call. `Some` whenever the
    /// provider returned an assembled response — including truncated or
    /// empty responses that were rejected, because those tokens were
    /// still spent. `None` only when the call failed before assembly.
    pub summarization_usage: Option<Usage>,
}

/// Mechanical estimate for manual compaction surfaces such as `/compact`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualCompactionEstimate {
    /// Number of store events that would be superseded by compaction.
    pub compacted_events: usize,
    /// Token estimate for the events being superseded.
    pub token_estimate_freed: usize,
}

/// Estimate the event count and freed tokens for keeping recent assistant turns.
///
/// Returns `None` when the store does not contain more assistant turns
/// than `keep_recent_turns`. When no token estimator is available,
/// compaction can still proceed and the freed-token estimate is zero.
#[must_use]
pub fn estimate_manual_compaction(
    store: &EventStore,
    keep_recent_turns: usize,
    token_estimator: Option<&dyn TokenEstimator>,
) -> Option<ManualCompactionEstimate> {
    let events = store.events();
    let plan = ContextEdits::new().plan_auto_compaction(store, keep_recent_turns)?;
    let compacted_events = plan.cut_exclusive();
    let token_estimate_freed =
        estimate_event_tokens(token_estimator, &events[..plan.cut_exclusive()]);
    Some(ManualCompactionEstimate {
        compacted_events,
        token_estimate_freed,
    })
}

fn estimate_event_tokens(
    token_estimator: Option<&dyn TokenEstimator>,
    events: &[SessionEvent],
) -> usize {
    let Some(estimator) = token_estimator else {
        return 0;
    };
    let mut total: usize = 0;
    for event in events {
        let tokens = match event {
            // A fired rule contributes its content to the prompt whether it
            // renders as a message (ContextInjection/MessageDelivery) or as
            // a re-materialized system section (SystemContextAppend), so it
            // carries a real token cost the compaction planner must see —
            // the same shape as a user message.
            SessionEvent::UserMessage { content, .. }
            | SessionEvent::RuleInjection { content, .. } => estimator.estimate(content),
            SessionEvent::AssistantMessage {
                response_items,
                content,
                ..
            } => {
                if response_items.is_empty() {
                    if content.is_empty() {
                        0
                    } else {
                        estimator.estimate(content)
                    }
                } else {
                    response_items.iter().fold(0_usize, |total, entry| {
                        total.saturating_add(estimator.estimate(&entry.item.raw().to_string()))
                    })
                }
            }
            SessionEvent::ToolResult { output, .. } => estimator.estimate(&output.to_string()),
            SessionEvent::SpokenResponse { content, .. } => {
                estimator.estimate(&content.to_string())
            }
            SessionEvent::Compaction { summary, .. } => estimator.estimate(summary),
            SessionEvent::ModelChange { .. }
            | SessionEvent::ProviderEpochBoundary { .. }
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::Custom { .. }
            | SessionEvent::ContextMark { .. } => 0,
        };
        total = total.saturating_add(tokens);
    }
    total
}

/// One-shot guard against duplicate auto-compaction within a single
/// `run_agent_step` call. The runner constructs this state at the top of
/// the function and consults it before invoking [`maybe_auto_compact`].
#[derive(Debug, Default)]
pub struct CompactionState {
    fired: bool,
}

impl CompactionState {
    /// Construct a fresh state with `fired = false`.
    #[must_use]
    pub const fn new() -> Self {
        Self { fired: false }
    }

    /// True after [`maybe_auto_compact`] has fired exactly once.
    #[must_use]
    pub const fn has_fired(&self) -> bool {
        self.fired
    }
}

/// Borrowed inputs for [`maybe_auto_compact`].
///
/// Bundled into one struct (mirroring `ToolBatchRequest` and
/// `PreflightArgs`) so the trigger does not carry a nine-parameter
/// signature.
pub struct AutoCompactArgs<'a> {
    /// Once-per-step trigger guard.
    pub state: &'a mut CompactionState,
    /// Context-edits tracker; the trigger is inert without one.
    pub edits: Option<&'a mut ContextEdits>,
    /// Session event store.
    pub store: &'a EventStore,
    /// The step's provider, used for the summarization call.
    pub provider: &'a dyn Provider,
    /// The step's resolved model, used for the summarization call.
    pub model: &'a str,
    /// Client-side token estimate for the current prompt.
    pub estimated_tokens: usize,
    /// Provider-reported token floor from the last completed provider call
    /// (`input_tokens + output_tokens`), when one exists (see
    /// [`ContextEdits::usage_floor`]). The trigger anchors on
    /// `max(estimated_tokens, usage_floor)`: the character estimate cannot
    /// see request content the provider re-bills every call (replayed
    /// encrypted reasoning items), while the provider's own bill can never
    /// understate a request that only grew.
    pub usage_floor: Option<u64>,
    /// Configured context-window budget; the trigger is inert when unset.
    pub context_window_limit: Option<u64>,
    /// Reserve-token headroom below the budget at which to fire; inert when
    /// unset. Compaction fires once the estimate exceeds
    /// `context_window_limit − reserve_tokens`.
    pub reserve_tokens: Option<u64>,
    /// Assistant turns to retain when compacting.
    pub keep_recent_turns: usize,
    /// Hook registry consulted before compaction runs.
    pub hooks: Option<&'a HookRegistry>,
    /// The step's cooperative cancellation token, when configured. The
    /// summarization provider call is raced against it so a cancel that
    /// fires during preflight ends the call promptly instead of waiting
    /// out a full LLM completion; a cancelled trigger commits nothing and
    /// stays un-fired.
    pub cancel: Option<&'a tokio_util::sync::CancellationToken>,
}

/// Decide whether auto-compaction should fire and run it if so.
///
/// The trigger fires when:
///
/// - `reserve_tokens` is `Some` *and* `context_window_limit` is `Some`,
/// - `max(estimated_tokens, usage_floor) > context_window_limit −
///   reserve_tokens` (the floor is absent until the first live provider
///   call and after every prompt-view shrink, in which case the estimate
///   governs alone),
/// - the [`CompactionState`] has not yet fired in this step,
/// - the loop holds a [`ContextEdits`] tracker (caller-visible side
///   effect: the tracker mutates).
///
/// When it fires, the events below the cut are summarized through the
/// step's provider and model; on summarization failure the mechanical
/// digest is committed instead (logged, marked — see
/// [`CompactionSummarySource`]). Returns `Ok(Some(AutoCompactionRun))` if
/// compaction ran, otherwise `Ok(None)`.
///
/// # Errors
///
/// Propagates any [`SessionError`] from committing the compaction plan.
/// Summarization-call failures are *not* errors: they degrade to the
/// digest fallback.
pub async fn maybe_auto_compact(
    args: AutoCompactArgs<'_>,
) -> Result<Option<AutoCompactionRun>, SessionError> {
    if args.state.fired {
        return Ok(None);
    }
    let Some(limit) = args.context_window_limit else {
        return Ok(None);
    };
    let Some(reserve) = args.reserve_tokens else {
        return Ok(None);
    };
    if limit == 0 {
        tracing::warn!(
            context_window_limit = limit,
            "auto-compaction context_window_limit is 0; the trigger cannot \
             compute a threshold — trigger disabled",
        );
        return Ok(None);
    }
    if reserve >= limit {
        tracing::warn!(
            reserve_tokens = reserve,
            context_window_limit = limit,
            "auto-compaction reserve_tokens is at or above context_window_limit; \
             every step would trigger compaction — trigger disabled",
        );
        return Ok(None);
    }
    // `reserve < limit` here, so the subtraction cannot underflow.
    let threshold = limit - reserve;
    let estimated = u64::try_from(args.estimated_tokens).unwrap_or(u64::MAX);
    let effective = args
        .usage_floor
        .map_or(estimated, |floor| estimated.max(floor));
    if effective <= threshold {
        return Ok(None);
    }
    let Some(edits) = args.edits else {
        return Ok(None);
    };
    let Some(plan) = edits.plan_auto_compaction(args.store, args.keep_recent_turns) else {
        return Ok(None);
    };

    // NH-006 R6 / C58: CompactionHook fires before the compaction event is
    // appended (and before any summarization tokens are spent). The event
    // count passed to the hook is the current number of events in the
    // store. A Block returns Ok(None) without compacting — this is not an
    // error, the operator explicitly chose to skip this trigger.
    if let Some(hooks) = args.hooks
        && let HookOutcome::Block { reason } = hooks.run_pre_compaction(args.store.len()).await
    {
        tracing::info!(
            reason = %reason,
            "compaction skipped by CompactionHook block",
        );
        return Ok(None);
    }

    // Freed estimate anchors on the same effective count as the trigger:
    // with a floor above the estimate (the incident shape), an
    // estimate-based value would be zero despite a genuinely oversized
    // request.
    let token_estimate_freed =
        usize::try_from(effective.saturating_sub(threshold)).unwrap_or(usize::MAX);

    let events = args.store.events();
    let elided = &events[..plan.cut_exclusive()];
    // Race the summarization call against the step's cancellation token:
    // the inline LLM call runs in preflight, outside the runner's own
    // provider-call select, and must not hold a cancelled step hostage
    // for a full completion. On cancel the trigger aborts without
    // committing and without consuming the once-per-step guard — the
    // runner observes the token at its next gate and returns `Cancelled`.
    let summarized = match args.cancel {
        Some(token) => {
            tokio::select! {
                biased;
                () = token.cancelled() => None,
                result = summarize_or_fall_back(
                    args.provider,
                    args.model,
                    elided,
                    token_estimate_freed,
                ) => Some(result?),
            }
        }
        None => Some(
            summarize_or_fall_back(args.provider, args.model, elided, token_estimate_freed).await?,
        ),
    };
    let Some((summary, summary_source, summarization_usage)) = summarized else {
        tracing::info!(
            "auto-compaction summarization cancelled mid-call; \
             compaction skipped and the trigger left armed",
        );
        return Ok(None);
    };

    let outcome = edits.commit_compaction_plan(args.store, plan, summary)?;
    args.state.fired = true;
    Ok(Some(AutoCompactionRun {
        outcome,
        freed_token_estimate: token_estimate_freed,
        summary_source,
        summarization_usage,
    }))
}

/// Produce the compaction summary: the LLM-written summary when the
/// provider call succeeds, otherwise the mechanical digest explicitly
/// marked as a non-semantic fallback (with the failure logged at `warn`).
///
/// # Errors
///
/// Returns [`SessionError::EventAppendFailed`] only if the fallback
/// digest cannot be serialised to JSON.
async fn summarize_or_fall_back(
    provider: &dyn Provider,
    model: &str,
    elided: &[SessionEvent],
    token_estimate_freed: usize,
) -> Result<(String, CompactionSummarySource, Option<Usage>), SessionError> {
    let (failure, usage) = match request_compaction_summary(provider, model, elided).await {
        Ok(response) => {
            let usage = response.usage.clone();
            if let Some(summary) = response.usable_summary() {
                return Ok((
                    summary.to_string(),
                    CompactionSummarySource::Llm,
                    Some(usage),
                ));
            }
            (
                format!(
                    "summarization response unusable (stop_reason={:?}, {} text chars)",
                    response.stop_reason,
                    response.text.chars().count(),
                ),
                Some(usage),
            )
        }
        Err(error) => (format!("summarization call failed: {error}"), None),
    };

    tracing::warn!(
        error = %failure,
        elided_events = elided.len(),
        "auto-compaction LLM summarization failed; committing the \
         mechanical digest as a non-semantic fallback",
    );
    let digest = fallback_digest(elided, token_estimate_freed, &failure)?;
    Ok((
        digest,
        CompactionSummarySource::MechanicalDigestFallback { error: failure },
        usage,
    ))
}

/// Build the marked fallback digest for a failed summarization.
fn fallback_digest(
    elided: &[SessionEvent],
    token_estimate_freed: usize,
    failure: &str,
) -> Result<String, SessionError> {
    let mut digest = build_compaction_digest(elided, token_estimate_freed);
    if let Some(object) = digest.as_object_mut() {
        object.insert(
            "summary_kind".to_string(),
            serde_json::Value::String("mechanical_digest_fallback".to_string()),
        );
        object.insert(
            "summarization_error".to_string(),
            serde_json::Value::String(failure.to_string()),
        );
        object.insert(
            "note".to_string(),
            serde_json::Value::String(
                "non-semantic fallback: LLM summarization failed, so this is a \
                 mechanical digest of the elided events, not a semantic summary"
                    .to_string(),
            ),
        );
    }
    serde_json::to_string(&digest).map_err(|e| SessionError::EventAppendFailed {
        reason: format!("failed to serialise fallback compaction digest: {e}"),
    })
}

/// Text, refusal, and thinking deltas accumulated by an **in-flight** provider
/// call — content the stream has produced but no assembled response has
/// yet persisted.
///
/// Maintained by the runner's provider-call collector
/// ([`call_provider`](crate::agent_loop::runner) via the shared
/// [`TimeoutState`]): reset at the start of every stream attempt (a retry
/// discards the failed attempt's partials, mirroring the live
/// `StreamRetry` marker) and cleared only once the `AssistantMessage`
/// event is durably appended (`persist_assistant_turn`) — assembly alone
/// does not disarm it, because the post-LLM hook window between assembly
/// and the append can still be hard-cut. When a step timeout or
/// cancellation cuts the call mid-stream or in that window, this is the
/// only surviving copy of what the model had said — the exit path
/// persists it as a `loop.partial_output` record.
#[derive(Clone, Debug, Default)]
pub struct InFlightPartial {
    /// Assistant text deltas accumulated so far, in stream order.
    pub text: String,
    /// Thinking/reasoning-summary deltas accumulated so far, in stream
    /// order.
    pub thinking: String,
    /// Refusal content accumulated so far. `Some("")` is distinct from
    /// absence: an explicitly empty refusal is still a refusal outcome.
    pub refusal: Option<String>,
}

impl InFlightPartial {
    /// Whether the stream had produced any content before the cut.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.thinking.is_empty() && self.refusal.is_none()
    }
}

/// Mutable handle shared between the runner's main body and the outer
/// `tokio::time::timeout` wrapper used by R2 (`step_timeout`). Stored
/// behind a [`parking_lot::Mutex`] so the timeout closure can capture an
/// `Arc` clone and read the latest values when the budget elapses.
#[derive(Debug, Default)]
pub struct TimeoutState {
    /// Iterations completed so far in this step.
    pub iterations: usize,
    /// Most recent non-empty assistant text observed by the runner.
    pub last_assistant_text: Option<String>,
    /// Token usage accumulated so far in this step, kept in sync with the
    /// runner's running total after every usage-bearing provider call so a
    /// timed-out step still reports the spend it incurred.
    pub usage: crate::provider::usage::Usage,
    /// Partial content of the in-flight provider call, when one is
    /// mid-stream. `None` between calls and after every completed
    /// assembly; `Some` (possibly empty) while a stream attempt runs.
    pub in_flight_partial: Option<InFlightPartial>,
}

/// Convenience alias for `Arc<Mutex<TimeoutState>>`.
pub type SharedTimeoutState = Arc<Mutex<TimeoutState>>;

/// Construct a fresh shared timeout-state handle.
#[must_use]
pub fn shared_timeout_state() -> SharedTimeoutState {
    Arc::new(Mutex::new(TimeoutState::default()))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn
)]
mod tests {
    use super::*;
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::request::ToolCallKind;
    use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};

    fn user(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    fn assistant_tool_call(call_id: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "tool".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: call_id.to_owned(),
                name: "read".to_owned(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                kind: ToolCallKind::Function,
                caller: crate::provider::request::ToolCallCaller::Absent,
            }],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    fn tool_result(call_id: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: call_id.to_owned(),
            tool_name: "read".to_owned(),
            output: serde_json::json!({"contents": "workspace"}),
            spool_ref: None,
            duration_ms: 1,
        }
    }

    struct OneTokenEstimator;

    impl TokenEstimator for OneTokenEstimator {
        fn estimate(&self, text: &str) -> usize {
            usize::from(!text.is_empty())
        }
    }

    #[test]
    fn manual_compaction_estimate_keeps_recent_assistant_turns() {
        let store = EventStore::new();
        for i in 0..3 {
            store.append(user(&format!("u{i:03}"))).expect("append");
            store
                .append(assistant(&format!("a{i:03}")))
                .expect("append");
        }
        let estimator = crate::r#loop::tokens::SimpleTokenEstimator;

        let estimate = estimate_manual_compaction(&store, 1, Some(&estimator))
            .expect("three assistant turns with keep=1 should compact");

        assert_eq!(estimate.compacted_events, 4);
        assert_eq!(estimate.token_estimate_freed, 4);
    }

    #[test]
    fn manual_compaction_estimate_is_none_without_enough_turns() {
        let store = EventStore::new();
        store.append(user("u000")).expect("append");
        store.append(assistant("a000")).expect("append");

        assert_eq!(estimate_manual_compaction(&store, 1, None), None);
    }

    #[test]
    fn manual_compaction_estimate_allows_missing_token_estimator() {
        let store = EventStore::new();
        for i in 0..3 {
            store.append(user(&format!("u{i:03}"))).expect("append");
            store
                .append(assistant(&format!("a{i:03}")))
                .expect("append");
        }

        let estimate = estimate_manual_compaction(&store, 1, None)
            .expect("missing estimator must not block compaction");

        assert_eq!(estimate.compacted_events, 4);
        assert_eq!(estimate.token_estimate_freed, 0);
    }

    #[test]
    fn manual_compaction_estimate_includes_tool_results_for_cut_turn() {
        let store = EventStore::new();
        store.append(user("u000")).expect("append");
        store
            .append(assistant_tool_call("call_old"))
            .expect("append");
        store.append(tool_result("call_old")).expect("append");
        store.append(user("u001")).expect("append");
        store.append(assistant("a001")).expect("append");

        let estimate = estimate_manual_compaction(&store, 1, Some(&OneTokenEstimator))
            .expect("two assistant turns with keep=1 should compact");

        assert_eq!(
            estimate.compacted_events, 3,
            "manual estimates must match ContextEdits' tool-result-aware compaction boundary",
        );
        assert_eq!(
            estimate.token_estimate_freed, 3,
            "user message, assistant text, and tool result should all be counted",
        );
    }

    struct ByteCountEstimator;

    impl TokenEstimator for ByteCountEstimator {
        fn estimate(&self, text: &str) -> usize {
            text.len()
        }
    }

    #[test]
    fn event_estimate_uses_only_canonical_item_bytes_when_present() {
        use crate::provider::response_item::{
            ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
        };

        let raw_items = [
            serde_json::json!({
                "type": "custom_tool_call",
                "id": "ct_1",
                "call_id": "call_1",
                "name": "shell",
                "input": "pwd"
            }),
            serde_json::json!({
                "type": "future_output",
                "id": "future_1",
                "payload": {"binary_ref": "artifact-1"}
            }),
        ];
        let response_items = raw_items
            .iter()
            .cloned()
            .map(|raw| ResponseTranscriptItem {
                item: ResponseItem::from_value(raw).expect("valid response item"),
                provenance: ResponseStreamProvenance {
                    item_id: Some("provenance is excluded".repeat(20)),
                    ..ResponseStreamProvenance::default()
                },
            })
            .collect();
        let event = SessionEvent::AssistantMessage {
            response_items,
            base: EventBase::new(None),
            content: "stale projection".repeat(50),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        };
        let expected = raw_items
            .iter()
            .map(|raw| raw.to_string().len())
            .sum::<usize>();

        assert_eq!(
            estimate_event_tokens(Some(&ByteCountEstimator), &[event]),
            expected,
        );
    }

    fn summary_events(text: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::TextDelta {
                text: text.to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 30,
                    output_tokens: 9,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]
    }

    /// `maybe_auto_compact` with default-ish wiring against `store`.
    async fn run_trigger(
        state: &mut CompactionState,
        edits: &mut ContextEdits,
        store: &EventStore,
        provider: &MockProvider,
        estimated_tokens: usize,
        context_window_limit: Option<u64>,
        hooks: Option<&HookRegistry>,
    ) -> Result<Option<AutoCompactionRun>, SessionError> {
        maybe_auto_compact(AutoCompactArgs {
            state,
            edits: Some(edits),
            store,
            provider,
            model: "test-model",
            estimated_tokens,
            usage_floor: None,
            context_window_limit,
            // limit 8_000 − reserve 2_000 = 6_000 trigger point, matching
            // the fire/no-fire boundaries these tests were written against.
            reserve_tokens: Some(2_000),
            keep_recent_turns: 10,
            hooks,
            cancel: None,
        })
        .await
    }

    #[tokio::test]
    async fn does_nothing_when_not_configured() {
        let store = EventStore::new();
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = run_trigger(
            &mut state, &mut edits, &store, &provider, 10_000, None, None,
        )
        .await
        .expect("ok");
        assert!(n.is_none());
        assert!(!state.has_fired());
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn fires_once_with_llm_summary_then_short_circuits() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![summary_events("the conversation so far")]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();

        let first = run_trigger(
            &mut state,
            &mut edits,
            &store,
            &provider,
            10_000,
            Some(8_000),
            None,
        )
        .await
        .expect("first compaction");
        let run = first.expect("compaction fires on first over-threshold call");
        assert!(run.freed_token_estimate > 0);
        assert!(
            !run.outcome.newly_superseded.is_empty(),
            "the fired compaction must report what it hid",
        );
        assert!(matches!(run.summary_source, CompactionSummarySource::Llm));
        let usage = run.summarization_usage.expect("summarization usage");
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(usage.output_tokens, 9);
        assert!(state.has_fired());

        // The committed compaction record carries the LLM summary verbatim.
        let compaction = store
            .get(&run.outcome.compaction_id)
            .expect("compaction stored");
        let SessionEvent::Compaction { summary, .. } = compaction else {
            panic!("expected Compaction variant");
        };
        assert_eq!(summary, "the conversation so far");

        let second = run_trigger(
            &mut state,
            &mut edits,
            &store,
            &provider,
            10_000,
            Some(8_000),
            None,
        )
        .await
        .expect("second invocation");
        assert!(
            second.is_none(),
            "expected no second compaction in same step"
        );
        assert_eq!(
            provider.call_count(),
            1,
            "the short-circuited trigger must not issue another summarization call",
        );
    }

    /// Provider whose stream never yields — a summarization call against
    /// it can only end through cancellation.
    struct HangingProvider;

    impl crate::provider::traits::Provider for HangingProvider {
        fn stream(
            &self,
            _request: crate::provider::request::ProviderRequest,
        ) -> Result<crate::provider::traits::ProviderStream, crate::error::ProviderError> {
            Ok(Box::pin(futures_util::stream::pending()))
        }

        fn capabilities(&self) -> crate::provider::tools::ProviderCapabilities {
            crate::provider::tools::ProviderCapabilities::default()
        }
    }

    /// Regression (summarization ran outside the cancellation select): a
    /// cancel firing while the inline summarization call is in flight
    /// must end the trigger promptly — committing nothing and leaving
    /// the once-per-step guard un-fired.
    #[tokio::test]
    async fn cancellation_aborts_in_flight_summarization_without_committing() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = HangingProvider;
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let token = tokio_util::sync::CancellationToken::new();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            trigger.cancel();
        });

        let run = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            maybe_auto_compact(AutoCompactArgs {
                state: &mut state,
                edits: Some(&mut edits),
                store: &store,
                provider: &provider,
                model: "test-model",
                estimated_tokens: 10_000,
                usage_floor: None,
                context_window_limit: Some(8_000),
                reserve_tokens: Some(2_000),
                keep_recent_turns: 10,
                hooks: None,
                cancel: Some(&token),
            }),
        )
        .await
        .expect("a cancelled summarization must end the trigger promptly")
        .expect("cancellation is not an error");

        assert!(run.is_none(), "a cancelled trigger commits nothing");
        assert!(
            !state.has_fired(),
            "the once-per-step guard stays un-fired so a later step can compact",
        );
        assert!(
            store
                .events()
                .iter()
                .all(|e| !matches!(e, SessionEvent::Compaction { .. })),
            "no compaction event may be committed after a cancelled summarization",
        );
    }

    #[tokio::test]
    async fn provider_failure_falls_back_to_marked_digest() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        // No scripted responses: the summarization call errors.
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();

        let run = run_trigger(
            &mut state,
            &mut edits,
            &store,
            &provider,
            10_000,
            Some(8_000),
            None,
        )
        .await
        .expect("trigger must not abort the step on summarization failure")
        .expect("compaction still fires with the fallback digest");

        let CompactionSummarySource::MechanicalDigestFallback { error } = &run.summary_source
        else {
            panic!("expected fallback source, got {:?}", run.summary_source);
        };
        assert!(error.contains("summarization call failed"), "{error}");
        assert!(
            run.summarization_usage.is_none(),
            "no usage when the call failed before assembly",
        );

        let compaction = store
            .get(&run.outcome.compaction_id)
            .expect("compaction stored");
        let SessionEvent::Compaction { summary, .. } = compaction else {
            panic!("expected Compaction variant");
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&summary).expect("fallback digest is JSON");
        assert_eq!(parsed["summary_kind"], "mechanical_digest_fallback");
        assert!(
            parsed["summarization_error"]
                .as_str()
                .is_some_and(|e| e.contains("summarization call failed")),
            "digest must carry the failure: {parsed}",
        );
        assert!(
            parsed["note"]
                .as_str()
                .is_some_and(|n| n.contains("non-semantic fallback")),
            "digest must be marked for humans: {parsed}",
        );
        assert_eq!(parsed["event_count_suppressed"], 20);
        assert!(state.has_fired());
    }

    #[tokio::test]
    async fn truncated_summary_falls_back_but_accounts_usage() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![vec![
            ProviderEvent::TextDelta {
                text: "cut off mid-sent".to_string(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::MaxTokens,
                usage: Usage {
                    input_tokens: 25,
                    output_tokens: 4,
                    ..Usage::default()
                },
                response_id: None,
            },
        ]]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();

        let run = run_trigger(
            &mut state,
            &mut edits,
            &store,
            &provider,
            10_000,
            Some(8_000),
            None,
        )
        .await
        .expect("ok")
        .expect("compaction fires");

        assert!(matches!(
            run.summary_source,
            CompactionSummarySource::MechanicalDigestFallback { .. }
        ));
        let usage = run
            .summarization_usage
            .expect("truncated responses still spent tokens");
        assert_eq!(usage.input_tokens, 25);
        assert_eq!(usage.output_tokens, 4);
    }

    #[tokio::test]
    async fn below_threshold_does_not_fire() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = run_trigger(
            &mut state,
            &mut edits,
            &store,
            &provider,
            100,
            Some(8_000),
            None,
        )
        .await
        .expect("ok");
        assert!(n.is_none());
        assert!(!state.has_fired());
        assert_eq!(provider.call_count(), 0);
    }

    /// Helper mirroring [`run_trigger`] but with explicit `reserve_tokens`
    /// and `usage_floor` so the boundary/pathological/floor cases can pin
    /// the exact trigger math.
    #[allow(clippy::too_many_arguments)]
    async fn run_trigger_with_reserve(
        state: &mut CompactionState,
        edits: &mut ContextEdits,
        store: &EventStore,
        provider: &MockProvider,
        estimated_tokens: usize,
        usage_floor: Option<u64>,
        context_window_limit: Option<u64>,
        reserve_tokens: Option<u64>,
    ) -> Result<Option<AutoCompactionRun>, SessionError> {
        maybe_auto_compact(AutoCompactArgs {
            state,
            edits: Some(edits),
            store,
            provider,
            model: "test-model",
            estimated_tokens,
            usage_floor,
            context_window_limit,
            reserve_tokens,
            keep_recent_turns: 10,
            hooks: None,
            cancel: None,
        })
        .await
    }

    /// The trigger fires strictly *above* `limit − reserve`: at exactly the
    /// boundary (estimate == limit − reserve) it must not fire; one token
    /// over it must.
    #[tokio::test]
    async fn fires_strictly_above_limit_minus_reserve() {
        let make_store = || {
            let store = EventStore::new();
            for i in 0..30 {
                store.append(assistant(&format!("t{i}"))).expect("append");
            }
            store
        };

        // limit 10_000 − reserve 3_000 = 7_000 trigger point.
        // At the boundary: no fire.
        let store = make_store();
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let at_boundary = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            7_000,
            None,
            Some(10_000),
            Some(3_000),
        )
        .await
        .expect("ok");
        assert!(
            at_boundary.is_none(),
            "estimate == limit − reserve must not fire"
        );
        assert!(!state.has_fired());

        // One token over the boundary: fires.
        let store = make_store();
        let provider = MockProvider::new(vec![summary_events("summary")]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let over = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            7_001,
            None,
            Some(10_000),
            Some(3_000),
        )
        .await
        .expect("ok");
        assert!(over.is_some(), "one token over the boundary must fire");
        assert!(state.has_fired());
    }

    /// A reserve at or above the window would make every step trigger
    /// compaction; the trigger warns and disables instead of looping.
    #[tokio::test]
    async fn reserve_at_or_above_limit_disables_the_trigger() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        // reserve == limit and reserve > limit both disable.
        for reserve in [Some(8_000_u64), Some(9_000_u64)] {
            let n = run_trigger_with_reserve(
                &mut state,
                &mut edits,
                &store,
                &provider,
                10_000,
                None,
                Some(8_000),
                reserve,
            )
            .await
            .expect("ok");
            assert!(n.is_none(), "reserve >= limit must not fire");
            assert!(!state.has_fired());
        }
        assert_eq!(provider.call_count(), 0);
    }

    /// A `None` reserve disables the trigger regardless of the estimate.
    #[tokio::test]
    async fn none_reserve_disables_the_trigger() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            1_000_000,
            None,
            Some(8_000),
            None,
        )
        .await
        .expect("ok");
        assert!(n.is_none(), "None reserve disables the trigger");
        assert!(!state.has_fired());
        assert_eq!(provider.call_count(), 0);
    }

    /// The owner-incident scenario (2026-07): a driven-mode agent overflowed
    /// a 272k window at 269k provider-reported input while the client
    /// estimate saw only ~236k (replayed encrypted reasoning items are
    /// invisible to the estimator). With the trigger anchored on
    /// `max(estimate, usage_floor)`, the floor fires the compaction the
    /// estimate alone never would have.
    #[tokio::test]
    async fn usage_floor_above_threshold_fires_despite_low_estimate() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![summary_events("summary")]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        // Estimate 236_000 is below the 242_000 trigger point
        // (272_000 − 30_000); the floor 271_200 is above it.
        let run = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            236_000,
            Some(271_200),
            Some(272_000),
            Some(30_000),
        )
        .await
        .expect("ok")
        .expect("the usage floor must fire the trigger");
        assert!(state.has_fired());
        // Freed estimate anchors on the effective count, not the estimate:
        // 271_200 − 242_000 = 29_200 (estimate-based would be zero).
        assert_eq!(run.freed_token_estimate, 29_200);
    }

    /// Death-spiral regression: a fired compaction must clear the usage
    /// floor (the estimate then reflects the compacted conversation but a
    /// stale floor would not), so the next check with a low estimate does
    /// NOT re-fire; a fresh provider report re-establishes the floor.
    #[tokio::test]
    async fn compaction_clears_the_usage_floor_and_does_not_refire() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![summary_events("summary")]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        edits.set_usage_floor(271_200);

        let floor = edits.usage_floor();
        let first = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            236_000,
            floor,
            Some(272_000),
            Some(30_000),
        )
        .await
        .expect("ok");
        assert!(first.is_some(), "floor above threshold fires");
        assert_eq!(
            edits.usage_floor(),
            None,
            "committing the compaction must clear the floor",
        );

        // Next step (fresh once-per-step guard): the estimate reflects the
        // compacted conversation and the floor is gone — no re-fire.
        let mut next_state = CompactionState::new();
        let floor = edits.usage_floor();
        let second = run_trigger_with_reserve(
            &mut next_state,
            &mut edits,
            &store,
            &provider,
            5_000,
            floor,
            Some(272_000),
            Some(30_000),
        )
        .await
        .expect("ok");
        assert!(
            second.is_none(),
            "a cleared floor must not re-fire on a small conversation",
        );
        assert!(!next_state.has_fired());

        // A fresh provider step re-establishes the floor.
        edits.set_usage_floor(50_000);
        assert_eq!(edits.usage_floor(), Some(50_000));
    }

    /// A floor at or below the trigger point defers to the estimate — the
    /// effective count is the max of the two, not the floor alone.
    #[tokio::test]
    async fn low_usage_floor_defers_to_the_estimate() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        // Neither number over the 6_000 trigger point: no fire.
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            5_000,
            Some(4_000),
            Some(8_000),
            Some(2_000),
        )
        .await
        .expect("ok");
        assert!(n.is_none(), "max(5_000, 4_000) <= 6_000 must not fire");

        // Estimate over the trigger point with a low floor: fires on the
        // estimate exactly as without a floor.
        let provider = MockProvider::new(vec![summary_events("summary")]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            6_001,
            Some(10),
            Some(8_000),
            Some(2_000),
        )
        .await
        .expect("ok");
        assert!(n.is_some(), "the estimate governs when it is the max");
    }

    // NH-006 R6 / C58: a CompactionHook returning Block must prevent
    // compaction from running — `maybe_auto_compact` returns Ok(None),
    // the [`CompactionState`] never flips to fired, and no summarization
    // tokens are spent.
    #[tokio::test]
    async fn block_from_compaction_hook_skips_compaction() {
        use crate::integration::hooks::{CompactionHook, Hook, HookOutcome, HookRegistry};

        struct BlockAll;

        #[async_trait::async_trait]
        impl CompactionHook for BlockAll {
            async fn before_compaction(&self, _event_count: usize) -> HookOutcome {
                HookOutcome::Block {
                    reason: "skip this trigger".to_owned(),
                }
            }
        }

        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();

        let mut registry = HookRegistry::new();
        registry.register(Hook::Compaction(Box::new(BlockAll)));

        let n = run_trigger(
            &mut state,
            &mut edits,
            &store,
            &provider,
            10_000,
            Some(8_000),
            Some(&registry),
        )
        .await
        .expect("ok");
        assert!(n.is_none(), "Block must suppress compaction");
        assert!(
            !state.has_fired(),
            "Block must not mark CompactionState as fired",
        );
        assert_eq!(
            provider.call_count(),
            0,
            "a blocked compaction must not spend summarization tokens",
        );
    }

    /// Finding 6 regression: a `context_window_limit` of 0 cannot yield a
    /// threshold, so the trigger disables (and now warns). The disabled
    /// behaviour is unchanged — no compaction fires, no summarization call —
    /// which this pins; the accompanying `warn!` is not asserted here (the
    /// sibling `reserve >= limit` test does not assert its warn either).
    #[tokio::test]
    async fn zero_limit_disables_the_trigger() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            1_000_000,
            None,
            Some(0),
            Some(0),
        )
        .await
        .expect("ok");
        assert!(n.is_none(), "a zero window must not fire the trigger");
        assert!(!state.has_fired());
        assert_eq!(provider.call_count(), 0);
    }

    /// Finding 1 (incident recurrence): a resumed session replays persisted
    /// reasoning items the client estimate must count. Here the estimate
    /// *without* reasoning sits below the `limit − reserve` threshold (a
    /// reasoning-blind estimator would never fire, and the usage floor is
    /// `None` on a fresh resume) while the estimate *with* reasoning crosses
    /// it — so the trigger fires only because the estimator now sees the
    /// replayed reasoning. This is the original `ContextWindowExceeded`
    /// shape.
    #[tokio::test]
    async fn resumed_reasoning_estimate_crosses_threshold_and_fires() {
        use crate::r#loop::tokens::{SimpleTokenEstimator, estimate_prompt_tokens};
        use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
        use crate::provider::request::{Message, MessageRole};

        // A resumed conversation: base content sits below the trigger, but
        // each assistant turn also replays a large encrypted reasoning blob.
        let est = SimpleTokenEstimator;
        let messages = vec![
            // ~260k chars of content → ~65k blind tokens (below the 70k trigger).
            Message {
                response_items: Vec::new(),
                reasoning: Vec::new(),
                role: MessageRole::User,
                content: Some("u".repeat(260_000)),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            },
            // ~140k chars of replayed encrypted reasoning → ~35k extra tokens.
            Message {
                response_items: Vec::new(),
                reasoning: vec![ReasoningItem {
                    id: "rs_resumed".to_string(),
                    summary: vec![ReasoningSummaryPart::SummaryText {
                        text: "recap".to_string(),
                    }],
                    content: None,
                    encrypted_content: Some("e".repeat(140_000)),
                }],
                role: MessageRole::Assistant,
                content: Some(String::new()),
                thinking: String::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
                tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
            },
        ];

        let with_reasoning = estimate_prompt_tokens(&est, &messages, &[]);
        let blind: Vec<Message> = messages
            .iter()
            .cloned()
            .map(|m| Message {
                response_items: Vec::new(),
                reasoning: Vec::new(),
                ..m
            })
            .collect();
        let without_reasoning = estimate_prompt_tokens(&est, &blind, &[]);

        // limit 100k − reserve 30k = 70k threshold. The reasoning-blind
        // estimate stays under it; the reasoning-aware one clears it.
        let limit = 100_000_u64;
        let reserve = 30_000_u64;
        let threshold = limit - reserve;
        assert!(
            u64::try_from(without_reasoning).expect("fits") <= threshold,
            "reasoning-blind estimate {without_reasoning} must not cross {threshold}",
        );
        assert!(
            u64::try_from(with_reasoning).expect("fits") > threshold,
            "reasoning-aware estimate {with_reasoning} must cross {threshold}",
        );

        // Sanity: the reasoning-blind estimate, fed to the trigger with the
        // resume-shaped `None` floor, would NOT fire — the incident.
        {
            let store = EventStore::new();
            for i in 0..30 {
                store.append(assistant(&format!("t{i}"))).expect("append");
            }
            let provider = MockProvider::new(vec![]);
            let mut state = CompactionState::new();
            let mut edits = ContextEdits::new();
            let blind_run = run_trigger_with_reserve(
                &mut state,
                &mut edits,
                &store,
                &provider,
                without_reasoning,
                None,
                Some(limit),
                Some(reserve),
            )
            .await
            .expect("ok");
            assert!(
                blind_run.is_none(),
                "the reasoning-blind estimate must sail past the trigger — the incident",
            );
        }

        // The reasoning-aware estimate fires the compaction the blind one
        // missed, with the floor unseeded (`None`) as on a fresh resume.
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![summary_events("resumed recap")]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let run = run_trigger_with_reserve(
            &mut state,
            &mut edits,
            &store,
            &provider,
            with_reasoning,
            None,
            Some(limit),
            Some(reserve),
        )
        .await
        .expect("ok")
        .expect("the reasoning-aware estimate must fire the trigger");
        assert!(state.has_fired());
        assert!(run.freed_token_estimate > 0);
    }

    #[test]
    fn timeout_state_captures_latest() {
        let handle = shared_timeout_state();
        {
            let mut guard = handle.lock();
            guard.iterations = 3;
            guard.last_assistant_text = Some("partial".to_string());
            guard.usage = crate::provider::usage::Usage {
                input_tokens: 120,
                output_tokens: 45,
                ..crate::provider::usage::Usage::default()
            };
        }
        let snapshot = handle.lock();
        assert_eq!(snapshot.iterations, 3);
        assert_eq!(snapshot.last_assistant_text.as_deref(), Some("partial"));
        assert_eq!(snapshot.usage.input_tokens, 120);
        assert_eq!(snapshot.usage.output_tokens, 45);
    }
}
