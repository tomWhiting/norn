use crate::provider::response_item::{KnownResponseItemKind, ResponseItem};
use crate::session::events::SessionEvent;

/// Filter applied to parent context events before forking.
///
/// Defaults preserve everything (`include_system = true`, no recency cap,
/// no exclusion).
#[derive(Clone, Debug)]
pub struct ContextFilter {
    /// If `false`, drop ambient session events (`ModelChange`,
    /// `Compaction`, `Fork`, `Label`, `Custom`).
    pub include_system: bool,
    /// If set, keep only the last `n` events after other filters apply.
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
    /// Apply the filter to `events` and return a fresh `Vec` of the
    /// retained or rewritten events.
    #[must_use]
    pub fn apply(&self, events: &[SessionEvent]) -> Vec<SessionEvent> {
        let mut filtered: Vec<SessionEvent> = events
            .iter()
            .filter_map(|event| self.transform(event))
            .collect();

        if let Some(limit) = self.include_recent_n
            && filtered.len() > limit
        {
            let cut = filtered.len() - limit;
            filtered.drain(..cut);
            filtered = crate::session::without_orphan_local_tool_outputs(filtered);
        }
        filtered
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
            } if self.exclude_tool_calls => Some(SessionEvent::AssistantMessage {
                response_items: response_items
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
                    .collect(),
                base: base.clone(),
                content: content.clone(),
                thinking: thinking.clone(),
                reasoning: reasoning.clone(),
                tool_calls: Vec::new(),
                usage: usage.clone(),
                stop_reason: stop_reason.clone(),
                response_id: response_id.clone(),
            }),
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
