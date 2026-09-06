//! Tool evidence joined by source and exact call identity, never tool name or text.

use crate::provider::request::ToolCallKind;
use crate::session::events::EventId;
use crate::tool::ENVELOPE_DESCRIPTION_KEY;

use super::body::{BodyOrigin, BodyRef, BodyRepresentation, DisplayText};
use super::contract::{
    AttemptKey, CoverageGap, HistoryPosition, ItemId, SegmentKey, ViewItem, ViewItemKind,
};
use super::error::ViewError;
use super::projection::SessionProjection;

/// Tool lifecycle inferred only from explicit invocation/result evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    /// Partial invocation arguments are still arriving.
    Assembling,
    /// A complete call was received, with no result yet observed.
    Running,
    /// An execution result was received without an error field.
    Completed,
    /// Execution returned an explicit error.
    Failed,
    /// An explicit permission denial prevented execution.
    Blocked,
    /// Local execution cancellation interrupted pending work.
    Cancelled,
    /// Missing or ambiguous invocation/result coverage prevents exact joining.
    Incomplete,
}

/// Exact tool facts retained when the compact row's full body is collapsed.
#[derive(Clone, Debug)]
pub struct ToolView {
    /// Actual provider call ID; absent while only the stream item ID is known.
    pub call_id: Option<String>,
    /// Actual streaming alias, never substituted for `call_id`.
    pub stream_item_id: Option<String>,
    /// Original tool name, absent if the stream has not supplied it.
    pub name: Option<DisplayText>,
    /// Original nonblank envelope intent, explicitly absent when not supplied or invalid.
    pub description: Option<DisplayText>,
    /// Why complete arguments could not yield a description, kept separate.
    pub description_error: Option<DisplayText>,
    /// Function or freeform call kind, absent for an orphan result.
    pub kind: Option<ToolCallKind>,
    /// Exact argument body; no fabricated empty arguments for orphan results.
    pub arguments: Option<BodyRef>,
    /// Recorded inline output or authoritative spool capability.
    pub result: Option<BodyRef>,
    /// Accepted invocation event, narrowing reused call IDs when possible.
    pub invocation_event: Option<EventId>,
    /// Proven local attempt that emitted this invocation, when observed live.
    pub invocation_attempt: Option<AttemptKey>,
    /// Accepted result event when observed.
    pub result_event: Option<EventId>,
    /// Actual result parent link, usable only when it names the invocation.
    pub result_parent: Option<EventId>,
    /// Observed lifecycle state.
    pub state: ToolState,
    /// Observed result outcome even when invocation coverage is incomplete.
    pub result_state: Option<ToolState>,
    /// Actual reported execution duration.
    pub duration_ms: Option<u64>,
    /// Explicit mutation commitment, independent of diagnostics/error status.
    pub committed: Option<bool>,
}

impl ToolView {
    pub(super) fn orphan(call_id: &str, name: &str) -> Self {
        Self {
            call_id: Some(call_id.to_owned()),
            stream_item_id: None,
            name: Some(DisplayText::new(name)),
            description: None,
            description_error: None,
            kind: None,
            arguments: None,
            result: None,
            invocation_event: None,
            invocation_attempt: None,
            result_event: None,
            result_parent: None,
            state: ToolState::Incomplete,
            result_state: None,
            duration_ms: None,
            committed: None,
        }
    }
}

pub(super) fn description(
    arguments: &str,
    kind: ToolCallKind,
) -> (Option<DisplayText>, Option<DisplayText>) {
    if kind == ToolCallKind::Custom {
        return (None, None);
    }
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) => description_value(&value, kind),
        Err(error) => (
            None,
            Some(DisplayText::new(&format!(
                "cannot read {ENVELOPE_DESCRIPTION_KEY}: {error}"
            ))),
        ),
    }
}

pub(super) fn description_value(
    arguments: &serde_json::Value,
    kind: ToolCallKind,
) -> (Option<DisplayText>, Option<DisplayText>) {
    if kind == ToolCallKind::Custom {
        return (None, None);
    }
    let Some(arguments) = arguments.as_object() else {
        return (
            None,
            Some(DisplayText::new(&format!(
                "function tool arguments must be an object to read {ENVELOPE_DESCRIPTION_KEY}"
            ))),
        );
    };
    match arguments.get(ENVELOPE_DESCRIPTION_KEY) {
        None => (None, None),
        Some(serde_json::Value::String(text)) if !text.trim().is_empty() => {
            (Some(DisplayText::new(text)), None)
        }
        Some(serde_json::Value::String(_)) => (
            None,
            Some(DisplayText::new(&format!(
                "{ENVELOPE_DESCRIPTION_KEY} is empty"
            ))),
        ),
        Some(_) => (
            None,
            Some(DisplayText::new(&format!(
                "{ENVELOPE_DESCRIPTION_KEY} must be a string"
            ))),
        ),
    }
}

pub(super) fn result_facts(tool: &mut ToolView, output: &serde_json::Value, duration_ms: u64) {
    tool.duration_ms = Some(duration_ms);
    tool.committed = output.get("committed").and_then(serde_json::Value::as_bool);
    tool.state = if output.get("error").is_some_and(|error| !error.is_null()) {
        ToolState::Failed
    } else {
        ToolState::Completed
    };
    if output
        .pointer("/error/kind")
        .and_then(serde_json::Value::as_str)
        == Some("permission_denied")
    {
        tool.state = ToolState::Blocked;
    }
    tool.result_state = Some(tool.state);
    if tool.arguments.is_none() {
        tool.state = ToolState::Incomplete;
    }
}

impl SessionProjection {
    pub(super) fn live_tool_delta(
        &mut self,
        item_id: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        arguments: &str,
        kind: ToolCallKind,
    ) -> Result<(), ViewError> {
        let key = self.key(SegmentKey::ToolItem(item_id.to_owned()))?;
        let direct = ItemId::Provisional(key.clone());
        let existing = if self.items.get(&direct).is_some() {
            Some(direct)
        } else {
            call_id.and_then(|call| self.items.live_call_id(&key.attempt, call))
        };
        let body_key = existing
            .as_ref()
            .and_then(|id| self.items.get(id))
            .and_then(|row| match &row.kind {
                ViewItemKind::Tool(tool) => tool.arguments.as_ref(),
                _ => None,
            })
            .and_then(|body| match body.origin() {
                BodyOrigin::Provisional { key, .. } => Some(key.clone()),
                BodyOrigin::Committed { .. } | BodyOrigin::Local { .. } => None,
            })
            .unwrap_or_else(|| key.clone());
        let body = self.store_body(body_key, arguments, true, BodyRepresentation::Text)?;
        let mut row = existing
            .as_ref()
            .and_then(|id| self.items.get(id))
            .cloned()
            .unwrap_or_else(|| {
                let mut tool = ToolView::orphan("", "");
                tool.call_id = None;
                tool.name = None;
                ViewItem {
                    id: ItemId::Provisional(key.clone()),
                    label: DisplayText::new("Tool name unavailable"),
                    kind: ViewItemKind::Tool(Box::new(tool)),
                    bodies: Vec::new(),
                    model: self
                        .execution
                        .as_ref()
                        .map(|execution| execution.model.clone()),
                }
            });
        if let ViewItemKind::Tool(tool) = &mut row.kind {
            tool.stream_item_id = Some(item_id.to_owned());
            if let Some(call) = call_id {
                tool.call_id = Some(call.to_owned());
            }
            if let Some(name) = name {
                tool.name = Some(DisplayText::new(name));
                row.label = DisplayText::new(name);
            }
            tool.kind = Some(kind);
            tool.invocation_attempt = Some(key.attempt);
            tool.arguments = Some(body.clone());
            tool.state = ToolState::Assembling;
            row.bodies = vec![body];
        }
        self.items.insert(row)?;
        if let Some(execution) = &mut self.execution {
            execution.last_segment = None;
        }
        Ok(())
    }

    pub(super) fn live_tool_complete(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        kind: ToolCallKind,
    ) -> Result<(), ViewError> {
        let key = self.key(SegmentKey::ToolCall(call_id.to_owned()))?;
        let body = self.store_body(key.clone(), arguments, false, BodyRepresentation::Text)?;
        let existing = self.items.live_call_id(&key.attempt, call_id);
        let mut tool = existing
            .as_ref()
            .and_then(|id| self.items.get(id))
            .and_then(|row| match &row.kind {
                ViewItemKind::Tool(tool) => Some(tool.as_ref().clone()),
                _ => None,
            })
            .unwrap_or_else(|| ToolView::orphan(call_id, name));
        tool.name = Some(DisplayText::new(name));
        tool.kind = Some(kind);
        tool.invocation_attempt = Some(key.attempt.clone());
        (tool.description, tool.description_error) = description(arguments, kind);
        tool.arguments = Some(body);
        if tool.result.is_none() {
            tool.state = ToolState::Running;
        }
        let bodies = tool
            .arguments
            .iter()
            .chain(tool.result.iter())
            .cloned()
            .collect();
        let row = ViewItem {
            id: ItemId::Provisional(key),
            kind: ViewItemKind::Tool(Box::new(tool)),
            label: DisplayText::new(name),
            bodies,
            model: self
                .execution
                .as_ref()
                .map(|execution| execution.model.clone()),
        };
        if let Some(previous) = existing {
            self.link_alias(previous.clone(), row.id.clone())?;
            self.items.replace(&previous, row)?;
        } else {
            self.items.insert(row)?;
        }
        if let Some(execution) = &mut self.execution {
            execution.last_segment = None;
        }
        Ok(())
    }

    pub(super) fn live_tool_result(
        &mut self,
        call_id: &str,
        name: &str,
        output: &serde_json::Value,
        duration_ms: u64,
    ) -> Result<(), ViewError> {
        let execution = self
            .execution
            .as_ref()
            .ok_or(ViewError::NoExecution)?
            .attempt
            .execution;
        let candidates = self.items.pending_call_ids(execution, call_id);
        let serialized =
            serde_json::to_string(output).map_err(|source| ViewError::LiveBodyMalformed {
                referent: call_id.to_owned(),
                source,
            })?;
        if let [id] = candidates.as_slice() {
            let attempt = self
                .items
                .get(id)
                .and_then(|row| match &row.kind {
                    ViewItemKind::Tool(tool) => tool.invocation_attempt.clone(),
                    _ => None,
                })
                .ok_or(ViewError::AttemptMismatch)?;
            let key = super::contract::ProvisionalKey {
                source: self.source.clone(),
                attempt,
                segment: SegmentKey::ToolResult(call_id.to_owned()),
            };
            let body = self.store_body(key, &serialized, false, BodyRepresentation::Json)?;
            if let Some(mut row) = self.items.get(id).cloned()
                && let ViewItemKind::Tool(tool) = &mut row.kind
            {
                tool.result = Some(body.clone());
                result_facts(tool, output, duration_ms);
                row.bodies = tool.arguments.iter().cloned().chain([body]).collect();
                self.items.insert(row)?;
            }
        } else {
            let mut tool = ToolView::orphan(call_id, name);
            result_facts(&mut tool, output, duration_ms);
            let ordinal = self.local_ordinal;
            self.local_body(
                ViewItemKind::Tool(Box::new(tool)),
                name,
                &serialized,
                BodyRepresentation::Json,
            )?;
            let id = ItemId::Local {
                source: self.source.clone(),
                ordinal,
            };
            if let Some(row) = self.items.get_mut(&id)
                && let ViewItemKind::Tool(tool) = &mut row.kind
            {
                tool.result = row.bodies.first().cloned();
            }
            self.coverage
                .gaps
                .insert(CoverageGap::IncompleteAssociation);
        }
        Ok(())
    }

    pub(super) fn interrupt_tools(&mut self, execution: uuid::Uuid) {
        for id in self.items.execution_ids(execution) {
            if let Some(row) = self.items.get_mut(&id)
                && let ViewItemKind::Tool(tool) = &mut row.kind
                && matches!(tool.state, ToolState::Running | ToolState::Assembling)
            {
                tool.state = ToolState::Cancelled;
            }
        }
    }

    pub(super) fn join_committed_result(&mut self, orphan: &ItemId) -> Result<(), ViewError> {
        let Some(row) = self.items.get(orphan) else {
            return Ok(());
        };
        let ViewItemKind::Tool(result) = &row.kind else {
            return Err(ViewError::AttemptMismatch);
        };
        let result = result.as_ref().clone();
        let call_id = result
            .call_id
            .as_deref()
            .ok_or(ViewError::AttemptMismatch)?;
        let exact = result
            .result_parent
            .as_ref()
            .map_or_else(Vec::new, |parent| {
                self.items.invocation_ids(parent, call_id)
            });
        let selected = if let [id] = exact.as_slice() {
            precedes(id, orphan).then(|| id.clone())
        } else if exact.is_empty() {
            let parent_loaded = result
                .result_parent
                .as_ref()
                .is_none_or(|parent| self.events.contains_key(parent));
            let covered = matches!(orphan, ItemId::Committed { cursor, .. } if matches!(cursor.position(), HistoryPosition::Event { ordinal, .. } if *ordinal < self.complete_prefix));
            if parent_loaded && covered {
                match self
                    .items
                    .preceding_invocation_ids(call_id, orphan)
                    .as_slice()
                {
                    [id] => Some(id.clone()),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(id) = selected {
            let mut target = self
                .items
                .get(&id)
                .cloned()
                .ok_or(ViewError::AttemptMismatch)?;
            if let ViewItemKind::Tool(tool) = &mut target.kind {
                if tool.result_event.is_some() && tool.result_event != result.result_event {
                    self.coverage
                        .gaps
                        .insert(CoverageGap::IncompleteAssociation);
                    return Ok(());
                }
                tool.result = result.result;
                tool.result_event = result.result_event;
                tool.result_parent = result.result_parent;
                tool.result_state = result.result_state;
                tool.duration_ms = result.duration_ms;
                tool.committed = result.committed;
                tool.state = result.result_state.unwrap_or(ToolState::Incomplete);
                target.bodies = tool
                    .arguments
                    .iter()
                    .chain(tool.result.iter())
                    .cloned()
                    .collect();
            }
            self.items.insert(target)?;
            self.link_alias(orphan.clone(), id)?;
            self.items.remove(orphan);
        }
        Ok(())
    }
}

fn precedes(invocation: &ItemId, result: &ItemId) -> bool {
    match (invocation, result) {
        (
            ItemId::Committed {
                cursor: invocation, ..
            },
            ItemId::Committed { cursor: result, .. },
        ) => {
            matches!((invocation.position(), result.position()), (HistoryPosition::Event { ordinal: left, .. }, HistoryPosition::Event { ordinal: right, .. }) if left < right)
        }
        _ => false,
    }
}
