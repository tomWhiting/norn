//! Validation against authoritative completed output items.

use std::collections::BTreeSet;

use serde_json::Value;

use super::{HostedPhase, ItemStringKind};
use crate::provider::response_item::ResponseItem;

use super::super::{ResponseItemIdentity, ResponseReconciler, ResponseReconciliationError};

impl ResponseReconciler {
    pub(in super::super) fn reconcile_item_channel_authority(
        &mut self,
        identity: &ResponseItemIdentity,
        item: &ResponseItem,
    ) -> Result<(), ResponseReconciliationError> {
        validate_required_hosted_fields(item)?;
        self.reconcile_parts(identity, item)?;
        self.reconcile_summary_parts(identity, item)?;
        self.reconcile_annotations(identity, item)?;
        self.reconcile_hosted_strings(identity, item)?;
        self.validate_hosted_lifecycle(identity, item)
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

fn validate_required_hosted_fields(item: &ResponseItem) -> Result<(), ResponseReconciliationError> {
    match item.item_type() {
        "file_search_call" => {
            require_array(item, "queries")?;
            require_status(
                item,
                &[
                    "in_progress",
                    "searching",
                    "completed",
                    "incomplete",
                    "failed",
                ],
            )?;
        }
        "web_search_call" => {
            require_object(item, "action")?;
            require_status(item, &["in_progress", "searching", "completed", "failed"])?;
        }
        "image_generation_call" => {
            require_present(item, &["result"])?;
            require_nullable_string(item, "result")?;
            require_status(item, &["in_progress", "completed", "generating", "failed"])?;
        }
        "mcp_call" => {
            require_strings(item, &["arguments", "name", "server_label"])?;
            require_optional_nullable_string(item, "output")?;
            require_optional_nullable_string(item, "error")?;
            require_optional_status(
                item,
                &[
                    "in_progress",
                    "completed",
                    "incomplete",
                    "calling",
                    "failed",
                ],
            )?;
        }
        "mcp_list_tools" => {
            require_strings(item, &["server_label"])?;
            if item.raw().get("tools").and_then(Value::as_array).is_none() {
                return Err(ResponseReconciliationError::MissingAuthoritativeItemField {
                    item_type: "mcp_list_tools",
                    field: "tools",
                });
            }
            require_optional_nullable_string(item, "error")?;
        }
        "code_interpreter_call" => {
            require_strings(item, &["container_id"])?;
            require_present(item, &["code", "outputs"])?;
            require_nullable_string(item, "code")?;
            require_nullable_array(item, "outputs")?;
            require_status(
                item,
                &[
                    "in_progress",
                    "completed",
                    "incomplete",
                    "interpreting",
                    "failed",
                ],
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn require_strings(
    item: &ResponseItem,
    fields: &[&'static str],
) -> Result<(), ResponseReconciliationError> {
    for field in fields {
        if item.raw().get(*field).and_then(Value::as_str).is_none() {
            return Err(ResponseReconciliationError::MissingAuthoritativeItemField {
                item_type: hosted_item_type(item.item_type()),
                field,
            });
        }
    }
    Ok(())
}

fn require_present(
    item: &ResponseItem,
    fields: &[&'static str],
) -> Result<(), ResponseReconciliationError> {
    for field in fields {
        if item.raw().get(*field).is_none() {
            return Err(ResponseReconciliationError::MissingAuthoritativeItemField {
                item_type: hosted_item_type(item.item_type()),
                field,
            });
        }
    }
    Ok(())
}

fn require_array(
    item: &ResponseItem,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    if item.raw().get(field).and_then(Value::as_array).is_none() {
        return Err(missing_field(item, field));
    }
    Ok(())
}

fn require_object(
    item: &ResponseItem,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    if item.raw().get(field).and_then(Value::as_object).is_none() {
        return Err(missing_field(item, field));
    }
    Ok(())
}

fn require_nullable_string(
    item: &ResponseItem,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    match item.raw().get(field) {
        Some(value) if value.is_null() || value.is_string() => Ok(()),
        Some(_) | None => Err(missing_field(item, field)),
    }
}

fn require_optional_nullable_string(
    item: &ResponseItem,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    match item.raw().get(field) {
        Some(value) if !value.is_null() && !value.is_string() => Err(missing_field(item, field)),
        Some(_) | None => Ok(()),
    }
}

fn require_nullable_array(
    item: &ResponseItem,
    field: &'static str,
) -> Result<(), ResponseReconciliationError> {
    match item.raw().get(field) {
        Some(value) if value.is_null() || value.is_array() => Ok(()),
        Some(_) | None => Err(missing_field(item, field)),
    }
}

fn require_status(
    item: &ResponseItem,
    allowed: &[&str],
) -> Result<(), ResponseReconciliationError> {
    let Some(status) = item.raw().get("status").and_then(Value::as_str) else {
        return Err(missing_field(item, "status"));
    };
    if allowed.contains(&status) {
        Ok(())
    } else {
        Err(missing_field(item, "status"))
    }
}

fn require_optional_status(
    item: &ResponseItem,
    allowed: &[&str],
) -> Result<(), ResponseReconciliationError> {
    match item.raw().get("status") {
        None => Ok(()),
        Some(value)
            if value
                .as_str()
                .is_some_and(|status| allowed.contains(&status)) =>
        {
            Ok(())
        }
        Some(_) => Err(missing_field(item, "status")),
    }
}

fn missing_field(item: &ResponseItem, field: &'static str) -> ResponseReconciliationError {
    ResponseReconciliationError::MissingAuthoritativeItemField {
        item_type: hosted_item_type(item.item_type()),
        field,
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

fn hosted_item_type(value: &str) -> &'static str {
    match value {
        "file_search_call" => "file_search_call",
        "web_search_call" => "web_search_call",
        "image_generation_call" => "image_generation_call",
        "mcp_call" => "mcp_call",
        "mcp_list_tools" => "mcp_list_tools",
        "code_interpreter_call" => "code_interpreter_call",
        _ => "hosted output item",
    }
}
