//! Exhaustive committed-event reduction; authoritative body references without eager reads.

use crate::provider::reasoning::ReasoningSummaryPart;
use crate::provider::request::ToolCallKind;
use crate::session::PROVIDER_STATE_PROVENANCE_EVENT_TYPE;
use crate::session::events::{ContextMarkKind, ProviderEpochBoundaryReason, SessionEvent};

use super::body::{BodyRef, DisplayField, DisplayText, known_lifecycle};
use super::contract::{
    CommittedPartIdentity, HistoryCursor, HistoryPosition, HistoryRecord, ItemId, ViewItem,
    ViewItemKind,
};
use super::error::ViewError;
use super::projection::SessionProjection;
use super::response::{ResponsePartKind, response_parts};
use super::tools::{ToolState, ToolView, description, result_facts};

/// Project a selected store record after its actual cursor has been validated.
/// The returned record owns only compact metadata and lazy display capabilities.
pub(crate) fn project_committed(
    cursor: &HistoryCursor,
    event: &SessionEvent,
) -> Result<HistoryRecord, ViewError> {
    let HistoryPosition::Event { ordinal, .. } = cursor.position() else {
        return Err(ViewError::CursorMismatch {
            event_id: event.base().id.clone(),
        });
    };
    cursor.validate(cursor.source(), *ordinal, &event.base().id)?;
    let items = SessionProjection::committed_rows(cursor, event)?;
    let parts = items
        .iter()
        .enumerate()
        .filter_map(|(row, item)| {
            let SessionEvent::AssistantMessage { response_items, .. } = event else {
                return None;
            };
            let super::body::BodyOrigin::Committed { field, .. } = item.bodies.first()?.origin()
            else {
                return None;
            };
            let (index, part) = match field {
                DisplayField::ResponseText { item, part }
                | DisplayField::ResponseRefusal { item, part }
                | DisplayField::ResponseSummary { item, part } => (*item, *part),
                _ => return None,
            };
            Some(CommittedPartIdentity {
                row,
                item_id: response_items.get(index)?.item.id()?.to_owned(),
                part,
            })
        })
        .collect();
    Ok(HistoryRecord {
        cursor: cursor.clone(),
        items,
        assistant: matches!(event, SessionEvent::AssistantMessage { .. }),
        parts,
    })
}

struct CommittedBuilder<'a> {
    cursor: &'a HistoryCursor,
    event: &'a SessionEvent,
    items: Vec<ViewItem>,
}

impl CommittedBuilder<'_> {
    fn push(
        &mut self,
        kind: ViewItemKind,
        label: &str,
        field: Option<DisplayField>,
    ) -> Result<(), ViewError> {
        let bodies = field
            .map(|field| BodyRef::committed(self.cursor.clone(), self.event, field))
            .transpose()?
            .into_iter()
            .collect();
        self.items.push(ViewItem {
            id: ItemId::Committed {
                cursor: self.cursor.clone(),
                part: self.items.len(),
            },
            kind,
            label: DisplayText::new(label),
            bodies,
            model: None,
        });
        Ok(())
    }

    fn tool(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        kind: ToolCallKind,
        field: DisplayField,
    ) -> Result<(), ViewError> {
        let body = BodyRef::committed(self.cursor.clone(), self.event, field)?;
        let mut tool = ToolView::orphan(call_id, name);
        tool.kind = Some(kind);
        tool.arguments = Some(body.clone());
        tool.invocation_event = Some(self.event.base().id.clone());
        tool.state = ToolState::Running;
        (tool.description, tool.description_error) = description(arguments, kind);
        self.push(ViewItemKind::Tool(Box::new(tool)), name, None)?;
        if let Some(row) = self.items.last_mut() {
            row.bodies.push(body);
        }
        Ok(())
    }
}

impl SessionProjection {
    pub(super) fn committed_rows(
        cursor: &HistoryCursor,
        event: &SessionEvent,
    ) -> Result<Vec<ViewItem>, ViewError> {
        let mut rows = CommittedBuilder {
            cursor,
            event,
            items: Vec::new(),
        };
        match event {
            SessionEvent::UserMessage { .. } => rows.push(
                ViewItemKind::Input,
                "Input",
                Some(DisplayField::UserContent),
            )?,
            SessionEvent::AssistantMessage {
                response_items,
                content,
                thinking,
                reasoning,
                tool_calls,
                ..
            } => {
                if response_items.is_empty() {
                    if thinking.is_empty() {
                        for (item, reasoning) in reasoning.iter().enumerate() {
                            for (part, summary) in reasoning.summary.iter().enumerate() {
                                let ReasoningSummaryPart::SummaryText { text } = summary;
                                if !text.is_empty() {
                                    rows.push(
                                        ViewItemKind::Thinking,
                                        "Thinking summary",
                                        Some(DisplayField::ReasoningSummary { item, part }),
                                    )?;
                                }
                            }
                        }
                    } else {
                        rows.push(
                            ViewItemKind::Thinking,
                            "Thinking summary",
                            Some(DisplayField::AssistantThinking),
                        )?;
                    }
                    if !content.is_empty() {
                        rows.push(
                            ViewItemKind::Text,
                            "Assistant",
                            Some(DisplayField::AssistantContent),
                        )?;
                    }
                    for (call, tool) in tool_calls.iter().enumerate() {
                        let body = BodyRef::committed(
                            cursor.clone(),
                            event,
                            DisplayField::LegacyArguments { call },
                        )?;
                        let mut view = ToolView::orphan(&tool.call_id, &tool.name);
                        view.kind = Some(tool.kind);
                        view.arguments = Some(body.clone());
                        view.invocation_event = Some(event.base().id.clone());
                        view.state = ToolState::Running;
                        view.description = match tool.kind {
                            ToolCallKind::Function => tool
                                .arguments
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .map(DisplayText::new),
                            ToolCallKind::Custom => None,
                        };
                        rows.push(ViewItemKind::Tool(Box::new(view)), &tool.name, None)?;
                        if let Some(row) = rows.items.last_mut() {
                            row.bodies.push(body);
                        }
                    }
                } else {
                    for (index, item) in response_items.iter().enumerate() {
                        for part in response_parts(index, &item.item) {
                            match part.kind {
                                ResponsePartKind::Text => {
                                    rows.push(ViewItemKind::Text, "Assistant", part.field)?;
                                }
                                ResponsePartKind::Thinking => rows.push(
                                    ViewItemKind::Thinking,
                                    "Thinking summary",
                                    part.field,
                                )?,
                                ResponsePartKind::Refusal => {
                                    rows.push(ViewItemKind::Refusal, "Model refusal", part.field)?;
                                }
                                ResponsePartKind::Tool {
                                    call_id,
                                    name,
                                    kind,
                                } => rows.tool(
                                    call_id,
                                    name,
                                    part.text,
                                    kind,
                                    DisplayField::ResponseArguments { item: index },
                                )?,
                                ResponsePartKind::Unavailable => {
                                    rows.push(ViewItemKind::Unavailable, part.text, None)?;
                                }
                            }
                        }
                    }
                }
                if rows.items.is_empty() {
                    rows.push(
                        ViewItemKind::Notice,
                        "Assistant response without display content",
                        None,
                    )?;
                }
            }
            SessionEvent::SpokenResponse { .. } => rows.push(
                ViewItemKind::Structured,
                "Spoken response",
                Some(DisplayField::SpokenContent),
            )?,
            SessionEvent::ToolResult {
                tool_call_id,
                tool_name,
                output,
                spool_ref,
                duration_ms,
                ..
            } => {
                let field = if spool_ref.is_some() {
                    DisplayField::ToolOutputSpool
                } else {
                    DisplayField::ToolOutputInline
                };
                let body = BodyRef::committed(cursor.clone(), event, field)?;
                let mut tool = ToolView::orphan(tool_call_id, tool_name);
                tool.result = Some(body.clone());
                tool.result_event = Some(event.base().id.clone());
                tool.result_parent.clone_from(&event.base().parent_id);
                result_facts(&mut tool, output, *duration_ms);
                rows.push(ViewItemKind::Tool(Box::new(tool)), tool_name, None)?;
                if let Some(row) = rows.items.last_mut() {
                    row.bodies.push(body);
                }
            }
            SessionEvent::ModelChange {
                old_model,
                new_model,
                ..
            } => rows.push(
                ViewItemKind::ModelChange {
                    old: DisplayText::new(old_model),
                    new: DisplayText::new(new_model),
                },
                "Model changed",
                None,
            )?,
            SessionEvent::ProviderEpochBoundary { reason, .. } => {
                let label = match reason {
                    ProviderEpochBoundaryReason::MigratedLegacy => {
                        "Provider epoch: migrated legacy session"
                    }
                    ProviderEpochBoundaryReason::ProviderIdentityAdoption => {
                        "Provider epoch: identity adoption"
                    }
                    ProviderEpochBoundaryReason::ResponseStatePublication
                    | ProviderEpochBoundaryReason::ResponseStatePublicationV1(_) => {
                        "Provider epoch: response state publication"
                    }
                    ProviderEpochBoundaryReason::FilteredFork => "Provider epoch: filtered fork",
                };
                rows.push(ViewItemKind::Metadata, label, None)?;
            }
            SessionEvent::Compaction {
                replaced_event_ids, ..
            } => rows.push(
                ViewItemKind::Context,
                &format!(
                    "Compaction summary for {} recorded events",
                    replaced_event_ids.len()
                ),
                Some(DisplayField::CompactionSummary),
            )?,
            SessionEvent::ChildBranch {
                parent_session_id,
                child_session_id,
                path_address,
                parent_event_anchor,
                kind,
                ..
            } => rows.push(
                ViewItemKind::Child,
                &format!(
                    "{} {}: parent session {}, child session {}, parent anchor {}",
                    kind.as_str(),
                    path_address,
                    parent_session_id.as_deref().unwrap_or("ephemeral"),
                    child_session_id.as_deref().unwrap_or("ephemeral"),
                    parent_event_anchor
                        .as_ref()
                        .map_or("empty", |id| id.as_str())
                ),
                None,
            )?,
            SessionEvent::ForkComplete {
                forked_session_id,
                duration_ms,
                ..
            } => rows.push(
                ViewItemKind::Child,
                &format!(
                    "Child result: {} ({duration_ms} ms)",
                    forked_session_id.as_deref().unwrap_or("ephemeral")
                ),
                Some(DisplayField::ForkResult),
            )?,
            SessionEvent::Label {
                label, description, ..
            } => rows.push(
                ViewItemKind::Notice,
                label,
                description.as_ref().map(|_| DisplayField::LabelDescription),
            )?,
            SessionEvent::Custom { event_type, .. } => rows.push(
                if event_type == PROVIDER_STATE_PROVENANCE_EVENT_TYPE {
                    ViewItemKind::Metadata
                } else if known_lifecycle(event_type) {
                    ViewItemKind::Notice
                } else {
                    ViewItemKind::Unavailable
                },
                event_type,
                known_lifecycle(event_type).then_some(DisplayField::CustomLifecycle),
            )?,
            SessionEvent::ContextMark {
                mark,
                target_event_id,
                ..
            } => {
                let operation = match mark {
                    ContextMarkKind::Suppress => "suppressed",
                    ContextMarkKind::Inject => "injected",
                };
                rows.push(
                    ViewItemKind::Context,
                    &format!("Context mark: {operation} {target_event_id}"),
                    None,
                )?;
            }
            SessionEvent::RuleInjection { rule_id, .. } => rows.push(
                ViewItemKind::Context,
                &format!("Recorded rule input: {rule_id}"),
                Some(DisplayField::RuleContent),
            )?,
        }
        Ok(rows.items)
    }
}
