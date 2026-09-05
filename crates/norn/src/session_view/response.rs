//! Approved Responses item display parts shared by live and committed projection.

use crate::provider::request::ToolCallKind;
use crate::provider::response_item::{ResponseContentPart, ResponseItem};

use super::body::{DisplayField, summary_text};
use super::contract::{CoverageGap, HistoryRecord, ItemId, SegmentKey, ViewItem, ViewItemKind};
use super::projection::SessionProjection;

pub(super) enum ResponsePartKind<'a> {
    Text,
    Thinking,
    Refusal,
    Tool {
        call_id: &'a str,
        name: &'a str,
        kind: ToolCallKind,
    },
    Unavailable,
}

pub(super) struct ResponsePart<'a> {
    pub field: Option<DisplayField>,
    pub kind: ResponsePartKind<'a>,
    pub text: &'a str,
}

pub(super) fn response_parts(index: usize, item: &ResponseItem) -> Vec<ResponsePart<'_>> {
    match item {
        ResponseItem::Message(message) => message
            .content()
            .iter()
            .enumerate()
            .map(|(part, content)| match content {
                ResponseContentPart::OutputText { text, .. } => ResponsePart {
                    field: Some(DisplayField::ResponseText { item: index, part }),
                    kind: ResponsePartKind::Text,
                    text,
                },
                ResponseContentPart::Refusal { refusal, .. } => ResponsePart {
                    field: Some(DisplayField::ResponseRefusal { item: index, part }),
                    kind: ResponsePartKind::Refusal,
                    text: refusal,
                },
                ResponseContentPart::Opaque { part_type, .. } => ResponsePart {
                    field: None,
                    kind: ResponsePartKind::Unavailable,
                    text: part_type,
                },
            })
            .collect(),
        ResponseItem::Reasoning(reasoning) => reasoning
            .summary()
            .iter()
            .enumerate()
            .map(|(part, value)| match summary_text(value) {
                Some(text) => ResponsePart {
                    field: Some(DisplayField::ResponseSummary { item: index, part }),
                    kind: ResponsePartKind::Thinking,
                    text,
                },
                None => ResponsePart {
                    field: None,
                    kind: ResponsePartKind::Unavailable,
                    text: "Reasoning summary part unavailable",
                },
            })
            .collect(),
        ResponseItem::FunctionCall(call) => vec![ResponsePart {
            field: Some(DisplayField::ResponseArguments { item: index }),
            kind: ResponsePartKind::Tool {
                call_id: call.call_id(),
                name: call.name(),
                kind: ToolCallKind::Function,
            },
            text: call.arguments(),
        }],
        ResponseItem::CustomToolCall(call) => vec![ResponsePart {
            field: Some(DisplayField::ResponseArguments { item: index }),
            kind: ResponsePartKind::Tool {
                call_id: call.call_id(),
                name: call.name(),
                kind: ToolCallKind::Custom,
            },
            text: call.input(),
        }],
        ResponseItem::WebSearchCall(call) => vec![ResponsePart {
            field: None,
            kind: ResponsePartKind::Unavailable,
            text: call.status(),
        }],
        ResponseItem::Compaction(_) => vec![ResponsePart {
            field: None,
            kind: ResponsePartKind::Unavailable,
            text: "Provider compaction state is not a display body",
        }],
        ResponseItem::Known(_) | ResponseItem::Opaque(_) => vec![ResponsePart {
            field: None,
            kind: ResponsePartKind::Unavailable,
            text: item.item_type(),
        }],
    }
}

impl SessionProjection {
    pub(super) fn associate_response_items(
        &mut self,
        previous: &[ViewItem],
        record: &HistoryRecord,
    ) -> Result<(), super::error::ViewError> {
        let model = previous.iter().find_map(|row| row.model.as_ref()).cloned();
        for row in &record.items {
            if let Some(item) = self.items.get_mut(&row.id) {
                item.model.clone_from(&model);
            }
        }
        for old in previous {
            let ItemId::Provisional(key) = &old.id else {
                continue;
            };
            let candidates: Vec<_> = record.items.iter().filter(|new| {
                let ItemId::Committed { part, .. } = &new.id else { return false; };
                match (&old.kind, &new.kind) {
                    (ViewItemKind::Tool(old_tool), ViewItemKind::Tool(new_tool)) => old_tool.call_id.is_some() && old_tool.call_id == new_tool.call_id,
                    _ => record.parts.iter().any(|identity| identity.row == *part && matches!(&key.segment, SegmentKey::ResponsePart { item_id: Some(item_id), part, .. } if item_id == &identity.item_id && part == &identity.part)),
                }
            }).map(|row| row.id.clone()).collect();
            if let [id] = candidates.as_slice() {
                self.link_alias(old.id.clone(), id.clone())?;
                if let Some(mut row) = self.items.get(id).cloned() {
                    if let (ViewItemKind::Tool(old_tool), ViewItemKind::Tool(new_tool)) =
                        (&old.kind, &mut row.kind)
                    {
                        new_tool
                            .invocation_attempt
                            .clone_from(&old_tool.invocation_attempt);
                        if new_tool.result.is_none() {
                            new_tool.result.clone_from(&old_tool.result);
                            new_tool.result_event.clone_from(&old_tool.result_event);
                            new_tool.result_parent.clone_from(&old_tool.result_parent);
                            new_tool.result_state = old_tool.result_state;
                            new_tool.duration_ms = old_tool.duration_ms;
                            new_tool.committed = old_tool.committed;
                            if old_tool.result.is_some() {
                                new_tool.state = old_tool.state;
                            }
                        }
                        row.bodies = new_tool
                            .arguments
                            .iter()
                            .chain(new_tool.result.iter())
                            .cloned()
                            .collect();
                    }
                    self.items.insert(row)?;
                }
            } else {
                self.coverage
                    .gaps
                    .insert(CoverageGap::IncompleteAssociation);
            }
        }
        Ok(())
    }
}
