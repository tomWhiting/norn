//! Public values produced by the two-phase context-compaction workflow.

use crate::session::events::EventId;

/// Result of a successful context auto-compaction.
#[derive(Debug)]
pub struct AutoCompactionOutcome {
    /// ID of the appended compaction event.
    pub compaction_id: EventId,
    /// Previously visible events newly superseded by this compaction.
    pub newly_superseded: Vec<EventId>,
}

/// A computed auto-compaction cut that has not yet been committed.
///
/// The two-phase value lets a provider-backed caller summarize the events
/// about to be elided before the durable compaction record is appended.
#[derive(Debug)]
pub struct CompactionPlan {
    pub(super) cut_exclusive: usize,
    pub(super) replaced_ids: Vec<EventId>,
    pub(super) newly_superseded: Vec<EventId>,
}

impl CompactionPlan {
    /// Exclusive end of the replaced span in store insertion order.
    #[must_use]
    pub const fn cut_exclusive(&self) -> usize {
        self.cut_exclusive
    }

    /// IDs of every event the compaction will supersede.
    #[must_use]
    pub fn replaced_ids(&self) -> &[EventId] {
        &self.replaced_ids
    }

    /// Events still visible in the prompt view when the plan was computed.
    #[must_use]
    pub fn newly_superseded(&self) -> &[EventId] {
        &self.newly_superseded
    }
}
