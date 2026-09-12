//! Failed semantic compaction retains its cause and measured spend without changing context.

use super::{ErrorClass, NornError};
use crate::provider::events::StopReason;
use crate::provider::usage::Usage;

/// A failed summary request; no compaction record or context marks were committed.
#[derive(Debug, thiserror::Error)]
#[error(
    "compaction summary failed for model {model}: {reason}; context is unchanged; resolve the cause and retry, or explicitly request mechanical compaction"
)]
pub struct CompactionFailure {
    /// The actual model used for the summary request.
    pub model: String,
    /// Provider failure or an unusable completed response.
    #[source]
    pub reason: CompactionFailureReason,
    /// Known spend, including a completed response rejected as unusable.
    pub usage: Option<Usage>,
}

/// The semantic-summary failure preserves the original provider error when available.
#[derive(Debug, thiserror::Error)]
pub enum CompactionFailureReason {
    /// The summary provider failed after the configured retry policy.
    #[error("provider request failed: {0}")]
    Provider(#[source] Box<NornError>),
    /// The provider returned an empty or incomplete summary.
    #[error("unusable summary (stop reason {stop_reason:?}, {text_chars} text characters)")]
    Unusable {
        /// The exact provider stop reason.
        stop_reason: StopReason,
        /// Text length, without retaining an unusable summary as context.
        text_chars: usize,
    },
}

impl CompactionFailureReason {
    /// Retain provider retry classification; rejected completed output is terminal.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Provider(error) => error.class(),
            Self::Unusable { .. } => ErrorClass::Terminal,
        }
    }
}
