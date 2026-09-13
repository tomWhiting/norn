//! Producer-owned child-run identity and timeline boundaries, independent of delivery time.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::session::events::EventId;
use crate::session::store::EventStore;
use crate::session_view::{SessionIdentity, ViewSource};

/// What started this controller invocation; not a fabricated task-service ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildRunTrigger {
    /// The task supplied when the child was launched.
    InitialTask,
    /// A later invocation consuming the persistent child's pending messages.
    FollowupMessages,
}

impl ChildRunTrigger {
    /// Stable wire label for this invocation's trigger.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InitialTask => "initial_task",
            Self::FollowupMessages => "followup_messages",
        }
    }
}

/// Original execution facts retained even when a result is queued or replayed later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildResultOrigin {
    /// Unique producer-owned controller invocation, not a provider response ID.
    pub run_id: Uuid,
    /// Actual source bound before launch; its store generation is process-local.
    pub source: ViewSource,
    /// Initial task or subsequent queued-message execution.
    pub trigger: ChildRunTrigger,
    /// Controller invocation start, before entering the agent step.
    pub started_at: DateTime<Utc>,
    /// Step return, before stop hooks, mailbox cleanup or result-channel waiting.
    pub completed_at: DateTime<Utc>,
    /// Timeline frontier before execution; absence means the timeline was empty.
    pub start_after_event: Option<EventId>,
    /// Timeline frontier at step return; concurrent audits can also fall in this span.
    pub end_at_event: Option<EventId>,
}

impl ChildResultOrigin {
    /// Structured transport metadata. Wall time never establishes supersession.
    #[must_use]
    pub fn metadata(&self) -> Value {
        let session = match &self.source.session {
            SessionIdentity::Persisted(id) => json!({"kind":"persisted", "id":id}),
            SessionIdentity::Ephemeral(id) => json!({"kind":"ephemeral", "id":id}),
        };
        json!({
            "run_id": self.run_id,
            "source": {"session":session, "agent_id":self.source.agent_id,
                "parent_agent_id":self.source.parent_agent_id,
                "store_generation":self.source.store_generation},
            "trigger":self.trigger.as_str(),
            "started_at":self.started_at,
            "completed_at":self.completed_at,
            "start_after_event":self.start_after_event,
            "end_at_event":self.end_at_event,
        })
    }
}

/// One controller-owned start record, consumed exactly once at step completion.
pub(crate) struct ChildRun {
    run_id: Uuid,
    source: ViewSource,
    trigger: ChildRunTrigger,
    started_at: DateTime<Utc>,
    start_after_event: Option<EventId>,
}

impl ChildRun {
    pub(crate) fn begin(store: &EventStore, source: &ViewSource, trigger: ChildRunTrigger) -> Self {
        Self {
            run_id: Uuid::new_v4(),
            source: source.clone(),
            trigger,
            started_at: Utc::now(),
            start_after_event: store.last_event_id(),
        }
    }

    pub(crate) fn finish(self, store: &EventStore) -> ChildResultOrigin {
        ChildResultOrigin {
            run_id: self.run_id,
            source: self.source,
            trigger: self.trigger,
            started_at: self.started_at,
            completed_at: Utc::now(),
            start_after_event: self.start_after_event,
            end_at_event: store.last_event_id(),
        }
    }
}

#[cfg(test)]
#[path = "result_origin_tests.rs"]
mod tests;
