use std::collections::{HashMap, HashSet};

use crate::provider::response_item::{KnownResponseItemKind, ResponseItem};
use crate::session::context_edit::ContextEdits;
use crate::session::events::{EventBase, EventId, SessionEvent};
use crate::session::{ProviderFilteredForkBoundary, ReplayArtifacts, ResponseAudioArtifactLink};

use super::fork_context_filter_error::ContextFilterError;

/// Filter applied to parent context events before forking.
///
/// Defaults preserve everything (`include_system = true`, no recency cap,
/// no exclusion).
#[derive(Clone, Debug)]
pub struct ContextFilter {
    /// If `false`, drop ambient session events (`ModelChange`,
    /// `Compaction`, `Fork`, `Label`, `Custom`). A response-audio artifact
    /// link is retained when its assistant event survives the filter.
    pub include_system: bool,
    /// If set, keep only the last `n` events after other filters apply.
    /// Restoring a required response-audio precursor after the cut may make
    /// the returned row count exceed `n`.
    pub include_recent_n: Option<usize>,
    /// If `true`, drop local tool results and strip canonical and projected
    /// local call/output items plus their coupled hosted-program lifecycle from
    /// `AssistantMessage` events.
    pub exclude_tool_calls: bool,
}

impl Default for ContextFilter {
    fn default() -> Self {
        Self {
            include_system: true,
            include_recent_n: None,
            exclude_tool_calls: false,
        }
    }
}

impl ContextFilter {
    /// Whether this filter preserves the parent's append-only audit history
    /// exactly.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.include_system && self.include_recent_n.is_none() && !self.exclude_tool_calls
    }

    /// Apply the filter to `events` and return a fresh `Vec` of the
    /// retained or rewritten events.
    ///
    /// The identity filter is deliberately a byte-for-byte audit copy. A
    /// non-identity filter instead starts from the effective persisted prompt
    /// view: durable suppression and compaction supersession marks are applied
    /// before any requested filtering, so removed rows cannot reappear in a
    /// child. The filtered seed ends with a fresh provider epoch boundary;
    /// provider response IDs retained for audit can therefore never become the
    /// child's continuation anchor.
    ///
    /// # Errors
    ///
    /// Returns [`ContextFilterError::ResponseAudio`] when a non-identity
    /// filter encounters a malformed reserved response-audio artifact link.
    pub fn apply(&self, events: &[SessionEvent]) -> Result<Vec<SessionEvent>, ContextFilterError> {
        if self.is_identity() {
            return Ok(events.to_vec());
        }

        let artifacts = ReplayArtifacts::from_events(events.to_vec());
        let mut edits = ContextEdits::new();
        edits.mark_superseded(artifacts.superseded_event_ids.iter().cloned());
        edits.mark_suppressed(artifacts.suppressed_event_ids.iter().cloned());
        edits.mark_injected(artifacts.injected_event_ids.iter().cloned());
        let mut effective = Vec::with_capacity(artifacts.events.len());
        crate::r#loop::context::for_each_visible_event(&artifacts.events, &edits, |event, _tag| {
            effective.push(event.clone());
        });
        effective = crate::session::atomic_local_tool_projection(&artifacts.events, effective);
        effective = remove_split_response_audio_pairs(&artifacts.events, effective)?;

        let mut filtered: Vec<SessionEvent> = effective
            .iter()
            .filter_map(|event| self.transform(event))
            .collect();

        if let Some(limit) = self.include_recent_n
            && filtered.len() > limit
        {
            let cut = filtered.len() - limit;
            filtered.drain(..cut);
        }
        filtered = crate::session::atomic_local_tool_projection(&effective, filtered);
        let mut filtered = preserve_response_audio_pairs(&effective, filtered)?;
        let parent_id = filtered.last().map(|event| event.base().id.clone());
        filtered.push(ProviderFilteredForkBoundary::into_event(EventBase::new(
            parent_id,
        )));
        Ok(filtered)
    }

    fn transform(&self, event: &SessionEvent) -> Option<SessionEvent> {
        match event {
            SessionEvent::ToolResult { .. } if self.exclude_tool_calls => None,
            SessionEvent::AssistantMessage {
                base,
                response_items,
                content,
                thinking,
                reasoning,
                usage,
                stop_reason,
                response_id,
                ..
            } if self.exclude_tool_calls => {
                let had_canonical_items = !response_items.is_empty();
                let response_items = response_items
                    .iter()
                    .filter(|item| {
                        !matches!(
                            &item.item,
                            ResponseItem::FunctionCall(_) | ResponseItem::CustomToolCall(_)
                        ) && !matches!(
                            &item.item,
                            ResponseItem::Known(known)
                                if matches!(
                                    known.kind(),
                                    KnownResponseItemKind::FunctionCallOutput
                                        | KnownResponseItemKind::CustomToolCallOutput
                                        | KnownResponseItemKind::Program
                                        | KnownResponseItemKind::ProgramOutput
                                )
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                // An empty response_items vector selects the legacy flat
                // projections. Drop a canonical event emptied by this filter
                // rather than reactivating compatibility-only content.
                if had_canonical_items && response_items.is_empty() {
                    None
                } else {
                    Some(SessionEvent::AssistantMessage {
                        response_items,
                        base: base.clone(),
                        content: content.clone(),
                        thinking: thinking.clone(),
                        reasoning: reasoning.clone(),
                        tool_calls: Vec::new(),
                        usage: usage.clone(),
                        stop_reason: stop_reason.clone(),
                        response_id: response_id.clone(),
                    })
                }
            }
            SessionEvent::ModelChange { .. }
            | SessionEvent::Compaction { .. }
            | SessionEvent::ChildBranch { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::Custom { .. }
                if !self.include_system =>
            {
                None
            }
            other => Some(other.clone()),
        }
    }
}

fn remove_split_response_audio_pairs(
    source: &[SessionEvent],
    mut effective: Vec<SessionEvent>,
) -> Result<Vec<SessionEvent>, ContextFilterError> {
    let source_assistants = source
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { base, .. } => Some(base.id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let effective_ids = effective
        .iter()
        .map(|event| event.base().id.clone())
        .collect::<HashSet<_>>();
    let mut split_halves = HashSet::new();
    for event in source {
        let Some(link) = ResponseAudioArtifactLink::from_event(event)? else {
            continue;
        };
        let assistant_id = link.assistant_event_id();
        if !source_assistants.contains(assistant_id) {
            continue;
        }
        let link_id = &event.base().id;
        let link_visible = effective_ids.contains(link_id);
        let assistant_visible = effective_ids.contains(assistant_id);
        if link_visible != assistant_visible {
            split_halves.insert(link_id.clone());
            split_halves.insert(assistant_id.clone());
        }
    }
    effective.retain(|event| !split_halves.contains(&event.base().id));
    Ok(effective)
}

fn preserve_response_audio_pairs(
    source: &[SessionEvent],
    filtered: Vec<SessionEvent>,
) -> Result<Vec<SessionEvent>, ContextFilterError> {
    // Format-2 stores the link as a separate Custom row. Rebuild in source
    // order so generic system/recency filters cannot split that association.
    let source_assistants = source
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { base, .. } => Some(base.id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let retained_assistants = filtered
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { base, .. } => Some(base.id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut required_links = HashSet::new();
    let mut excluded_links = HashSet::new();
    for event in source {
        let Some(link) = ResponseAudioArtifactLink::from_event(event)? else {
            continue;
        };
        if !source_assistants.contains(link.assistant_event_id()) {
            continue;
        }
        if retained_assistants.contains(link.assistant_event_id()) {
            required_links.insert(event.base().id.clone());
        } else {
            excluded_links.insert(event.base().id.clone());
        }
    }

    let mut retained = filtered
        .into_iter()
        .map(|event| (event.base().id.clone(), event))
        .collect::<HashMap<EventId, SessionEvent>>();
    let mut paired = Vec::with_capacity(retained.len().saturating_add(required_links.len()));
    for event in source {
        let id = &event.base().id;
        if excluded_links.contains(id) {
            retained.remove(id);
            continue;
        }
        if let Some(event) = retained.remove(id) {
            paired.push(event);
        } else if required_links.contains(id) {
            paired.push(event.clone());
        }
    }
    Ok(paired)
}
