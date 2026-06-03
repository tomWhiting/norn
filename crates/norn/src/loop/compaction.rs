//! Auto-compaction trigger and shared timeout state (N-023 R4 + R2).
//!
//! `maybe_auto_compact` ties the client-side token estimate from
//! [`crate::r#loop::tokens`] to the [`ContextEdits::auto_compact_keeping_recent_turns`]
//! helper added in this brief. It fires once per `run_agent_step` call —
//! the runner threads a [`CompactionState`] through the loop body so a
//! second over-threshold iteration is a no-op.
//!
//! `TimeoutState` is the shared mutable handle the runner exposes to the
//! `tokio::time::timeout` wrapper; on cancellation the outer scope reads
//! the most recent assistant text and iteration count to populate
//! [`crate::r#loop::runner::AgentStepResult::TimedOut`].

use std::sync::Arc;

use parking_lot::Mutex;

use crate::error::SessionError;
use crate::integration::hooks::{HookOutcome, HookRegistry};
use crate::session::context_edit::ContextEdits;
use crate::session::store::EventStore;

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
/// Returns the count of events suppressed if compaction ran, otherwise
/// `Ok(0)`.
///
/// # Errors
///
/// Propagates any [`SessionError`] from
/// [`ContextEdits::auto_compact_keeping_recent_turns`].
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
pub async fn maybe_auto_compact(
    state: &mut CompactionState,
    edits: Option<&mut ContextEdits>,
    store: &EventStore,
    estimated_tokens: usize,
    context_window_limit: Option<u64>,
    threshold_pct: Option<f64>,
    keep_recent_turns: usize,
    hooks: Option<&HookRegistry>,
) -> Result<usize, SessionError> {
    if state.fired {
        return Ok(0);
    }
    let Some(limit) = context_window_limit else {
        return Ok(0);
    };
    let Some(pct) = threshold_pct else {
        return Ok(0);
    };
    if limit == 0 {
        return Ok(0);
    }
    let threshold = (limit as f64) * pct;
    if (estimated_tokens as f64) <= threshold {
        return Ok(0);
    }
    let Some(edits) = edits else { return Ok(0) };

    // NH-006 R6 / C58: CompactionHook fires before the compaction
    // event is appended. The event count passed to the hook is the
    // current number of events in the store. A Block returns Ok(0)
    // without compacting — this is not an error, the operator
    // explicitly chose to skip this trigger.
    if let Some(hooks) = hooks
        && let HookOutcome::Block { reason } = hooks.run_pre_compaction(store.len()).await
    {
        tracing::info!(
            reason = %reason,
            "compaction skipped by CompactionHook block",
        );
        return Ok(0);
    }

    let token_estimate_freed = estimated_tokens.saturating_sub(threshold.max(0.0) as usize);
    let result =
        edits.auto_compact_keeping_recent_turns(store, keep_recent_turns, token_estimate_freed)?;
    if result.is_some() {
        state.fired = true;
        Ok(token_estimate_freed)
    } else {
        Ok(0)
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
    use crate::session::events::{EventBase, EventUsage, SessionEvent};

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

    #[tokio::test]
    async fn does_nothing_when_not_configured() {
        let store = EventStore::new();
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = maybe_auto_compact(
            &mut state,
            Some(&mut edits),
            &store,
            10_000,
            None,
            Some(0.75),
            10,
            None,
        )
        .await
        .expect("ok");
        assert_eq!(n, 0);
        assert!(!state.has_fired());
    }

    #[tokio::test]
    async fn fires_once_then_short_circuits() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();

        let first = maybe_auto_compact(
            &mut state,
            Some(&mut edits),
            &store,
            10_000,
            Some(8_000),
            Some(0.75),
            10,
            None,
        )
        .await
        .expect("first compaction");
        assert!(first > 0);
        assert!(state.has_fired());

        let second = maybe_auto_compact(
            &mut state,
            Some(&mut edits),
            &store,
            10_000,
            Some(8_000),
            Some(0.75),
            10,
            None,
        )
        .await
        .expect("second invocation");
        assert_eq!(second, 0, "expected no second compaction in same step");
    }

    #[tokio::test]
    async fn below_threshold_does_not_fire() {
        let store = EventStore::new();
        for i in 0..30 {
            store.append(assistant(&format!("t{i}"))).expect("append");
        }
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();
        let n = maybe_auto_compact(
            &mut state,
            Some(&mut edits),
            &store,
            100,
            Some(8_000),
            Some(0.75),
            10,
            None,
        )
        .await
        .expect("ok");
        assert_eq!(n, 0);
        assert!(!state.has_fired());
    }

    // NH-006 R6 / C58: a CompactionHook returning Block must prevent
    // compaction from running — `maybe_auto_compact` returns Ok(0) and
    // the [`CompactionState`] never flips to fired.
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
        let mut state = CompactionState::new();
        let mut edits = ContextEdits::new();

        let mut registry = HookRegistry::new();
        registry.register(Hook::Compaction(Box::new(BlockAll)));

        let n = maybe_auto_compact(
            &mut state,
            Some(&mut edits),
            &store,
            10_000,
            Some(8_000),
            Some(0.75),
            10,
            Some(&registry),
        )
        .await
        .expect("ok");
        assert_eq!(n, 0, "Block must suppress compaction");
        assert!(
            !state.has_fired(),
            "Block must not mark CompactionState as fired",
        );
    }

    #[test]
    fn timeout_state_captures_latest() {
        let handle = shared_timeout_state();
        {
            let mut guard = handle.lock();
            guard.iterations = 3;
            guard.last_assistant_text = Some("partial".to_string());
        }
        let snapshot = handle.lock();
        assert_eq!(snapshot.iterations, 3);
        assert_eq!(snapshot.last_assistant_text.as_deref(), Some("partial"));
    }
}
