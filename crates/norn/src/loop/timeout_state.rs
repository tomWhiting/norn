//! Shared state retained when a provider call is hard-cut.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::session::ResponseAudioArtifactRef;

/// Output accumulated by an in-flight provider call but not yet attached to a
/// durable assistant turn.
#[derive(Clone, Debug, Default)]
pub struct InFlightPartial {
    /// Assistant text deltas accumulated so far, in stream order.
    pub text: String,
    /// Thinking/reasoning-summary deltas accumulated so far, in stream order.
    pub thinking: String,
    /// Refusal content accumulated so far. `Some("")` remains distinct from
    /// absence.
    pub refusal: Option<String>,
    /// Current unsealed or sealed response-audio sidecar for this attempt.
    pub response_audio: Option<ResponseAudioArtifactRef>,
}

impl InFlightPartial {
    /// Whether the call produced no recoverable content before the cut.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.thinking.is_empty()
            && self.refusal.is_none()
            && self.response_audio.is_none()
    }
}

/// Mutable handle shared by the runner and its outer timeout wrapper.
#[derive(Debug, Default)]
pub struct TimeoutState {
    /// Iterations completed so far in this step.
    pub iterations: usize,
    /// Most recent non-empty assistant text observed by the runner.
    pub last_assistant_text: Option<String>,
    /// Token usage accumulated so far in this step.
    pub usage: crate::provider::usage::Usage,
    /// Current provider attempt's output, retained through hard cuts and the
    /// post-LLM/pre-append window.
    pub in_flight_partial: Option<InFlightPartial>,
}

/// Convenience alias for `Arc<Mutex<TimeoutState>>`.
pub type SharedTimeoutState = Arc<Mutex<TimeoutState>>;

/// Construct a fresh shared timeout-state handle.
#[must_use]
pub fn shared_timeout_state() -> SharedTimeoutState {
    Arc::new(Mutex::new(TimeoutState::default()))
}
