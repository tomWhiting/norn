//! Single-pass session replay: one traversal of a session history that
//! yields every derived artifact resume needs.
//!
//! Before this module existed, opening a persisted session walked the
//! full event history four to five times: the tolerant reader parsed
//! every line, the index self-heal re-summed usage, the action-log
//! rebuild walked the events twice (tool-call metadata, then tool
//! results), and the context-edit restore walked them again for
//! compaction marks. Per-step re-opens made that quadratic over a
//! workflow's life. [`ReplayArtifacts`] folds all of those derivations
//! into the single pass that materialises the events, so every resume
//! consumer reads from one value instead of re-walking the history.
//!
//! Embedders that keep a live [`EventStore`](crate::session::store::EventStore)
//! (rather than a session file) derive the same artifacts with
//! [`ReplayArtifacts::from_events`] — also a single traversal.

use std::collections::HashSet;

use crate::provider::usage::Usage;
use crate::session::events::{EventId, SessionEvent};

/// Everything a session open derives from the event history, produced
/// by exactly one traversal.
///
/// Produced by the tolerant session-file reader
/// ([`read_session_events`](super::io::read_session_events)) and by
/// [`Self::from_events`] for in-memory histories. Each consumer of a
/// resumed session reads its slice of state from here instead of
/// re-walking `events`:
///
/// * [`Self::usage`] — the index self-heal
///   ([`SessionManager::resume`](crate::session::SessionManager::resume));
/// * [`Self::superseded_event_ids`] — restoring persisted compaction
///   marks
///   ([`ContextEdits::mark_superseded`](crate::session::context_edit::ContextEdits::mark_superseded));
/// * [`Self::events`] — the action-log rebuild
///   ([`rebuild_action_log`](crate::agent::rebuild_action_log), itself
///   a single traversal of the slice);
/// * [`Self::skipped_lines`] / [`Self::format_version`] — the replay
///   summary surfaced to callers.
#[derive(Debug, Default)]
pub struct ReplayArtifacts {
    /// Every recovered event, in history order (file order for a read,
    /// slice order for [`Self::from_events`]).
    pub events: Vec<SessionEvent>,
    /// Number of non-empty lines the tolerant reader skipped:
    /// unparseable as a [`SessionEvent`] (torn write, invalid JSON,
    /// unknown variant) or carrying an [`EventId`] already seen earlier
    /// in the file. `0` for a healthy file and always `0` for
    /// [`Self::from_events`] (in-memory events have no line-level
    /// corruption to skip).
    pub skipped_lines: u64,
    /// Schema version from the file's header line; `None` for a
    /// pre-versioning (format `0`) file and for [`Self::from_events`].
    pub format_version: Option<u32>,
    /// Rollup of `AssistantMessage` usage across the history. Only the
    /// three fields the session index tracks (`input_tokens`,
    /// `output_tokens`, `cache_read_tokens`) are populated;
    /// `cache_write_tokens` and `cost_usd` stay at their defaults
    /// because the index schema does not store them.
    pub usage: Usage,
    /// Every event id replaced by a persisted
    /// [`SessionEvent::Compaction`] — the durable supersession marks the
    /// prompt view must re-apply on resume.
    pub superseded_event_ids: HashSet<EventId>,
    /// Every event id targeted by a persisted suppress
    /// [`SessionEvent::ContextMark`] — the durable suppression marks the
    /// prompt view must re-apply on resume
    /// ([`ContextEdits::mark_suppressed`](crate::session::context_edit::ContextEdits::mark_suppressed)).
    pub suppressed_event_ids: HashSet<EventId>,
    /// Every event id targeted by a persisted inject
    /// [`SessionEvent::ContextMark`] — the durable injection tags the
    /// prompt view must re-apply on resume
    /// ([`ContextEdits::mark_injected`](crate::session::context_edit::ContextEdits::mark_injected)).
    pub injected_event_ids: HashSet<EventId>,
}

impl ReplayArtifacts {
    /// Derive artifacts from an already-materialised event history in a
    /// single traversal.
    ///
    /// This is the in-memory counterpart of the session-file reader:
    /// embedders (and the agent assembly) holding an
    /// [`EventStore`](crate::session::store::EventStore) snapshot use it
    /// to restore compaction marks and rebuild the action log without
    /// walking the history once per consumer. [`Self::skipped_lines`] is
    /// `0` and [`Self::format_version`] is `None` — those describe file
    /// recovery, which does not apply here.
    #[must_use]
    pub fn from_events(events: Vec<SessionEvent>) -> Self {
        let mut artifacts = Self::default();
        for event in &events {
            artifacts.absorb(event);
        }
        artifacts.events = events;
        artifacts
    }

    /// Fold one event into every derived accumulator, then take
    /// ownership of it. The tolerant reader calls this once per
    /// recovered line — the single traversal that replaces the
    /// per-consumer re-walks.
    pub(crate) fn push(&mut self, event: SessionEvent) {
        self.absorb(&event);
        self.events.push(event);
    }

    /// Fold one event into the derived accumulators without storing it.
    fn absorb(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::AssistantMessage { usage, .. } => {
                self.usage.input_tokens =
                    self.usage.input_tokens.saturating_add(usage.input_tokens);
                self.usage.output_tokens =
                    self.usage.output_tokens.saturating_add(usage.output_tokens);
                self.usage.cache_read_tokens = self
                    .usage
                    .cache_read_tokens
                    .saturating_add(usage.cache_read_tokens);
            }
            SessionEvent::Compaction {
                replaced_event_ids, ..
            } => {
                self.superseded_event_ids
                    .extend(replaced_event_ids.iter().cloned());
            }
            SessionEvent::ContextMark {
                mark,
                target_event_id,
                ..
            } => match mark {
                crate::session::events::ContextMarkKind::Suppress => {
                    self.suppressed_event_ids.insert(target_event_id.clone());
                }
                crate::session::events::ContextMarkKind::Inject => {
                    self.injected_event_ids.insert(target_event_id.clone());
                }
            },
            // The remaining variants carry nothing any resume consumer
            // derives. Enumerated (no wildcard) so a new event variant
            // forces a decision here instead of silently contributing
            // nothing.
            SessionEvent::UserMessage { .. }
            | SessionEvent::ToolResult { .. }
            | SessionEvent::ModelChange { .. }
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::Custom { .. }
            | SessionEvent::RuleInjection { .. }
            | SessionEvent::SpokenResponse { .. } => {}
        }
    }
}
