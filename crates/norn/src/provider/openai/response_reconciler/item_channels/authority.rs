//! Validation against authoritative completed output items.

use std::collections::BTreeSet;

use serde_json::Value;

use super::{HostedPhase, ItemStringKind};
use crate::provider::response_item::{ResponseItem, ResponseTranscriptItem};

use super::super::{ResponseItemIdentity, ResponseReconciler, ResponseReconciliationError};

mod advanced_tool_schema;
mod client_call_schema;
mod container_tool_schema;
mod core_schema;
mod hosted_schema;
mod known_schema;
mod schema;
mod tool_filter_schema;
mod tool_schema;

impl ResponseReconciler {
    pub(in super::super) fn reconcile_item_channel_authority(
        &mut self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        self.reconcile_parts(identity, item)?;
        self.reconcile_summary_parts(identity, item)?;
        self.reconcile_annotations(identity, item)?;
        self.reconcile_hosted_strings(identity, item)?;
        self.validate_hosted_lifecycle(identity, item)
    }

    pub(in super::super) fn validate_authoritative_item_schema(
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        schema::validate_authoritative_item(item)
    }

    pub(in super::super) fn validate_authoritative_item_schemas(
        items: &[ResponseTranscriptItem],
    ) -> Result<(), ResponseReconciliationError> {
        items
            .iter()
            .try_for_each(|item| schema::validate_authoritative_item(&item.item))
    }

    #[cfg(test)]
    pub(in super::super) fn has_authoritative_item_validator(item_type: &str) -> bool {
        schema::has_authoritative_validator(item_type)
    }

    pub(in super::super) fn validate_terminal_item_channels(
        &self,
        terminal_identities: &BTreeSet<ResponseItemIdentity>,
    ) -> Result<(), ResponseReconciliationError> {
        if self
            .item_channels
            .touched
            .iter()
            .any(|identity| !terminal_identities.contains(identity))
        {
            return Err(ResponseReconciliationError::ItemScopedStateAbsentFromTerminal);
        }
        Ok(())
    }

    fn reconcile_parts(
        &self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        for ((part_identity, content_index), state) in &self.item_channels.parts {
            if part_identity != identity {
                continue;
            }
            let final_part = content_part(item, *content_index)?;
            if state.done.as_ref().is_some_and(|done| done != final_part) {
                return Err(ResponseReconciliationError::ItemScopedCompletionConflict);
            }
        }
        Ok(())
    }

    fn reconcile_annotations(
        &self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        for ((annotation_identity, content_index, annotation_index), annotation) in
            &self.item_channels.annotations
        {
            if annotation_identity != identity {
                continue;
            }
            let final_part = content_part(item, *content_index)?;
            let annotations = final_part
                .get("annotations")
                .and_then(Value::as_array)
                .ok_or(ResponseReconciliationError::MissingAuthoritativeItemField {
                    item_type: "message",
                    field: "content.annotations",
                })?;
            let index = usize::try_from(*annotation_index).map_err(|error| {
                ResponseReconciliationError::ContentIndexOverflow {
                    reason: error.to_string(),
                }
            })?;
            if annotations.get(index) != Some(annotation) {
                return Err(ResponseReconciliationError::ItemScopedCompletionConflict);
            }
        }
        Ok(())
    }

    fn reconcile_summary_parts(
        &self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        for ((part_identity, summary_index), state) in &self.item_channels.summary_parts {
            if part_identity != identity {
                continue;
            }
            let final_part = reasoning_summary_part(item, *summary_index)?;
            if state.done.as_ref().is_some_and(|done| done != final_part) {
                return Err(ResponseReconciliationError::ItemScopedCompletionConflict);
            }
        }
        Ok(())
    }

    fn reconcile_hosted_strings(
        &mut self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        for kind in [
            ItemStringKind::McpArguments,
            ItemStringKind::CodeInterpreterCode,
        ] {
            let Some(state) = self
                .item_channels
                .strings
                .get_mut(&(identity.clone(), kind))
            else {
                continue;
            };
            let field = authoritative_string_field(kind);
            let authoritative = item.raw().get(field).and_then(Value::as_str).ok_or(
                ResponseReconciliationError::MissingAuthoritativeItemField {
                    item_type: item_type_for_string(kind),
                    field,
                },
            )?;
            if state
                .done
                .as_deref()
                .is_some_and(|done| done != authoritative)
            {
                return Err(ResponseReconciliationError::ItemScopedCompletionConflict);
            }
            authoritative.clone_into(&mut state.preview);
        }
        Ok(())
    }

    fn validate_hosted_lifecycle(
        &self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        for ((lifecycle_identity, _), phase) in &self.item_channels.lifecycles {
            if lifecycle_identity != identity {
                continue;
            }
            if let Some(status) = item.raw().get("status").and_then(Value::as_str)
                && !lifecycle_status_is_consistent(*phase, status)
            {
                return Err(ResponseReconciliationError::ItemScopedCompletionConflict);
            }
        }
        Ok(())
    }
}

fn lifecycle_status_is_consistent(phase: HostedPhase, status: &str) -> bool {
    match phase {
        HostedPhase::Completed => status == "completed",
        HostedPhase::Failed => status == "failed",
        HostedPhase::InProgress => true,
        HostedPhase::Active => status != "in_progress",
    }
}

fn content_part(
    item: &ResponseItem,
    content_index: u64,
) -> Result<&Value, ResponseReconciliationError> {
    let content = item.raw().get("content").and_then(Value::as_array).ok_or(
        ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "message or reasoning",
            field: "content",
        },
    )?;
    let index = usize::try_from(content_index).map_err(|error| {
        ResponseReconciliationError::ContentIndexOverflow {
            reason: error.to_string(),
        }
    })?;
    content
        .get(index)
        .ok_or(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "message or reasoning",
            field: "content index",
        })
}

fn reasoning_summary_part(
    item: &ResponseItem,
    summary_index: u64,
) -> Result<&Value, ResponseReconciliationError> {
    let summary = item.raw().get("summary").and_then(Value::as_array).ok_or(
        ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "reasoning",
            field: "summary",
        },
    )?;
    let index = usize::try_from(summary_index).map_err(|error| {
        ResponseReconciliationError::ContentIndexOverflow {
            reason: error.to_string(),
        }
    })?;
    summary
        .get(index)
        .ok_or(ResponseReconciliationError::MissingAuthoritativeItemField {
            item_type: "reasoning",
            field: "summary index",
        })
}

const fn authoritative_string_field(kind: ItemStringKind) -> &'static str {
    match kind {
        ItemStringKind::McpArguments => "arguments",
        ItemStringKind::CodeInterpreterCode => "code",
    }
}

const fn item_type_for_string(kind: ItemStringKind) -> &'static str {
    match kind {
        ItemStringKind::McpArguments => "mcp_call",
        ItemStringKind::CodeInterpreterCode => "code_interpreter_call",
    }
}
