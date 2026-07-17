//! Ownership validation for Responses Programmatic Tool Calling.

use std::collections::HashSet;

use crate::error::{NornError, ProviderError};
use crate::r#loop::assembly::AssembledResponse;
use crate::provider::request::{Message, ToolCallCaller};
use crate::provider::response_item::{KnownResponseItemKind, ResponseItem, ResponseTranscriptItem};

/// Reject a program-issued local call unless its caller names an active
/// program in retained history or earlier in the current response.
pub(super) fn validate_programmatic_callers(
    messages: &[Message],
    response: &AssembledResponse,
) -> Result<(), NornError> {
    let mut active_programs = HashSet::new();
    for message in messages {
        apply_program_lifecycle(&message.response_items, &mut active_programs, false)?;
    }
    apply_program_lifecycle(&response.response_items, &mut active_programs, true)
}

fn apply_program_lifecycle(
    items: &[ResponseTranscriptItem],
    active_programs: &mut HashSet<String>,
    validate_callers: bool,
) -> Result<(), NornError> {
    for transcript in items {
        match &transcript.item {
            ResponseItem::FunctionCall(_) | ResponseItem::CustomToolCall(_) if validate_callers => {
                validate_caller(transcript, active_programs)?;
            }
            ResponseItem::Known(item) if item.kind() == KnownResponseItemKind::Program => {
                if let Some(call_id) = transcript
                    .item
                    .raw()
                    .get("call_id")
                    .and_then(|v| v.as_str())
                {
                    active_programs.insert(call_id.to_owned());
                }
            }
            ResponseItem::Known(item) if item.kind() == KnownResponseItemKind::ProgramOutput => {
                if let Some(call_id) = transcript
                    .item
                    .raw()
                    .get("call_id")
                    .and_then(|v| v.as_str())
                {
                    active_programs.remove(call_id);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_caller(
    transcript: &ResponseTranscriptItem,
    active_programs: &HashSet<String>,
) -> Result<(), NornError> {
    let caller = ToolCallCaller::from_item(transcript.item.raw());
    let Some(value) = caller.value() else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let Some(object) = value.as_object() else {
        return Err(protocol_error());
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("program") => {
            let linked = object
                .get("caller_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|caller_id| active_programs.contains(caller_id));
            if linked {
                Ok(())
            } else {
                Err(protocol_error())
            }
        }
        Some("direct") => Ok(()),
        _ => Err(protocol_error()),
    }
}

fn protocol_error() -> NornError {
    NornError::Provider(ProviderError::ResponseProtocolViolation {
        source: crate::provider::openai::response_reconciler::ResponseReconciliationError::UnmooredProgramCaller,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::events::StopReason;
    use crate::provider::request::MessageRole;
    use crate::provider::response_item::{ResponseItem, ResponseStreamProvenance};
    use crate::provider::usage::Usage;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn current_and_prior_active_programs_authorize_callers() -> TestResult {
        let current = response(vec![
            item(serde_json::json!({
                "type": "program",
                "id": "prog_current",
                "call_id": "program_current",
                "code": "text('ok')",
                "fingerprint": "opaque"
            }))?,
            function_call("call_current", "program_current")?,
        ]);
        validate_programmatic_callers(&[], &current)?;

        let history = vec![assistant(vec![item(serde_json::json!({
            "type": "program",
            "id": "prog_prior",
            "call_id": "program_prior",
            "code": "text('ok')",
            "fingerprint": "opaque"
        }))?])];
        let continuation = response(vec![function_call("call_prior", "program_prior")?]);
        validate_programmatic_callers(&history, &continuation)?;
        Ok(())
    }

    #[test]
    fn unmoored_or_completed_program_caller_is_rejected_without_id_disclosure() -> TestResult {
        let unmoored_response = response(vec![function_call("call_bad", "sentinel_unmoored")?]);
        let unmoored = validate_programmatic_callers(&[], &unmoored_response);
        let Err(unmoored) = unmoored else {
            return Err("unmoored program caller was accepted".into());
        };
        assert!(!unmoored.to_string().contains("sentinel_unmoored"));

        let history = vec![assistant(vec![
            item(serde_json::json!({
                "type": "program",
                "id": "prog_closed",
                "call_id": "program_closed",
                "code": "text('done')",
                "fingerprint": "opaque"
            }))?,
            item(serde_json::json!({
                "type": "program_output",
                "id": "prog_out_closed",
                "call_id": "program_closed",
                "result": "done",
                "status": "completed"
            }))?,
        ])];
        let after_close = response(vec![function_call("call_after", "program_closed")?]);
        assert!(validate_programmatic_callers(&history, &after_close).is_err());
        Ok(())
    }

    #[test]
    fn current_turn_cannot_pre_activate_a_later_program_item() -> TestResult {
        let current = response(vec![
            function_call("call_early", "program_late")?,
            item(serde_json::json!({
                "type": "program",
                "id": "prog_late",
                "call_id": "program_late",
                "code": "text('late')",
                "fingerprint": "opaque"
            }))?,
        ]);
        assert!(validate_programmatic_callers(&[], &current).is_err());
        Ok(())
    }

    fn function_call(
        call_id: &str,
        caller_id: &str,
    ) -> Result<ResponseTranscriptItem, crate::provider::ResponseItemError> {
        item(serde_json::json!({
            "type": "function_call",
            "id": format!("fc_{call_id}"),
            "call_id": call_id,
            "name": "lookup",
            "arguments": "{}",
            "caller": {"type": "program", "caller_id": caller_id}
        }))
    }

    fn item(
        raw: serde_json::Value,
    ) -> Result<ResponseTranscriptItem, crate::provider::ResponseItemError> {
        ResponseItem::from_value(raw).map(|item| ResponseTranscriptItem {
            item,
            provenance: ResponseStreamProvenance::default(),
        })
    }

    fn assistant(response_items: Vec<ResponseTranscriptItem>) -> Message {
        Message {
            response_items,
            role: MessageRole::Assistant,
            content: None,
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: ToolCallCaller::Absent,
        }
    }

    fn response(response_items: Vec<ResponseTranscriptItem>) -> AssembledResponse {
        AssembledResponse {
            response_items,
            refusal: None,
            text: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: None,
            response_audio: None,
        }
    }
}
