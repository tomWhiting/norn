//! Auto-compaction trigger and shared timeout state (N-023 R4 + R2).
//!
//! `maybe_auto_compact` ties the client-side token estimate from
//! [`crate::r#loop::tokens`] to the plan/commit compaction API on
//! [`ContextEdits`]. When the trigger fires, the loop asks the step's own
//! provider and model for an LLM-written summary of the events being
//! elided ([`crate::r#loop::summarization`]); the summary becomes the
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
//! [`crate::r#loop::runner::AgentStepResult::TimedOut`].

use std::sync::Arc;

use parking_lot::Mutex;

use crate::error::SessionError;
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::r#loop::numeric::{
    f64_to_usize_token_count, token_count_to_f64, usize_token_count_to_f64,
};
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
            SessionEvent::UserMessage { content, .. } => estimator.estimate(content),
            SessionEvent::AssistantMessage { content, .. } => {
                if content.is_empty() {
                    0
                } else {
                    estimator.estimate(content)
                }
            }
            SessionEvent::ToolResult { output, .. } => estimator.estimate(&output.to_string()),
            SessionEvent::SpokenResponse { content, .. } => {
                estimator.estimate(&content.to_string())
            }
            SessionEvent::Compaction { summary, .. } => estimator.estimate(summary),
            SessionEvent::ModelChange { .. }
            | SessionEvent::Fork { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::Custom { .. } => 0,
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
    /// Configured context-window budget; the trigger is inert when unset.
    pub context_window_limit: Option<u64>,
    /// Fraction of the budget at which to fire; inert when unset.
    pub threshold_pct: Option<f64>,
    /// Assistant turns to retain when compacting.
    pub keep_recent_turns: usize,
    /// Hook registry consulted before compaction runs.
    pub hooks: Option<&'a HookRegistry>,
}

/// Decide whether auto-compaction should fire and run it if so.
///
/// The trigger fires when:
///
/// - `threshold_pct` is `Some` *and* `context_window_limit` is `Some`,
/// - `estimated_tokens > threshold_pct * context_window_limit`,
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
    let Some(pct) = args.threshold_pct else {
        return Ok(None);
    };
    if limit == 0 {
        return Ok(None);
    }
    if !pct.is_finite() {
        tracing::warn!(
            threshold_pct = pct,
            "auto-compaction threshold_pct is not a finite number; trigger disabled",
        );
        return Ok(None);
    }
    let threshold = token_count_to_f64(limit) * pct;
    if usize_token_count_to_f64(args.estimated_tokens) <= threshold {
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

    let token_estimate_freed = args
        .estimated_tokens
        .saturating_sub(f64_to_usize_token_count(threshold.max(0.0)));

    let events = args.store.events();
    let elided = &events[..plan.cut_exclusive()];
    let (summary, summary_source, summarization_usage) =
        summarize_or_fall_back(args.provider, args.model, elided, token_estimate_freed).await?;

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
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    fn assistant_tool_call(call_id: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "tool".to_owned(),
            thinking: String::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: call_id.to_owned(),
                name: "read".to_owned(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                kind: ToolCallKind::Function,
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
            context_window_limit,
            threshold_pct: Some(0.75),
            keep_recent_turns: 10,
            hooks,
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

    #[tokio::test]
    async fn non_finite_threshold_pct_disables_the_trigger() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let provider = MockProvider::new(vec![]);
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = maybe_auto_compact(AutoCompactArgs {
            state: &mut state,
            edits: Some(&mut edits),
            store: &store,
            provider: &provider,
            model: "test-model",
            estimated_tokens: 10_000,
            context_window_limit: Some(8_000),
            threshold_pct: Some(f64::NAN),
            keep_recent_turns: 10,
            hooks: None,
        })
        .await
        .expect("ok");
        assert!(n.is_none());
        assert!(!state.has_fired());
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
