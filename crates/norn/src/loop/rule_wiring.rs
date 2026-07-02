//! Rules-engine wiring helpers extracted from `helpers.rs` to stay within
//! the 500-line production-code limit.
//!
//! Provides:
//! - [`build_runtime_events`] — derive
//!   [`RuntimeEvent`](crate::rules::types::RuntimeEvent) values from a
//!   completed tool call's name and arguments.
//! - [`partition_injections_by_timing`] — split an injection batch into
//!   Before- and After-timing buckets.
//! - [`apply_rule_injections`] — apply an injection batch to the running
//!   prompt, system sections, and event store.

use serde_json::Value;

use crate::error::SessionError;
use crate::r#loop::helpers::append_and_notify;
use crate::r#loop::loop_context::LoopContext;
use crate::provider::request::{Message, MessageRole};
use crate::rules::types::{
    DeliveryMode as RuleDeliveryMode, PathOperation, RuleInjection, RuntimeEvent, TriggerTiming,
};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Build the runtime events the rules engine should observe for a single
/// completed tool call.
///
/// `read` produces a `PathChanged{Read}` event; `write`, `edit`, and
/// `apply_patch` produce `PathChanged{Write}`; `bash` produces a
/// `BashCommandRun`. Every tool also produces a generic `ToolInvoked` event
/// so rules can match by tool name. The more-specific event is emitted
/// first so engines that prioritise the first match see it.
pub(super) fn build_runtime_events(tool_name: &str, arguments_json: &str) -> Vec<RuntimeEvent> {
    let parsed = serde_json::from_str::<Value>(arguments_json).ok();
    let mut events = Vec::new();

    match tool_name {
        "read" => {
            if let Some(path) = parsed.as_ref().and_then(|v| extract_str(v, "path")) {
                events.push(RuntimeEvent::PathChanged {
                    path,
                    operation: PathOperation::Read,
                });
            }
        }
        "write" | "edit" | "apply_patch" => {
            if let Some(path) = parsed.as_ref().and_then(|v| extract_str(v, "path")) {
                events.push(RuntimeEvent::PathChanged {
                    path,
                    operation: PathOperation::Write,
                });
            }
        }
        "bash" => {
            if let Some(command) = parsed.as_ref().and_then(|v| extract_str(v, "command")) {
                events.push(RuntimeEvent::BashCommandRun { command });
            }
        }
        _ => {}
    }

    events.push(RuntimeEvent::ToolInvoked {
        tool_name: tool_name.to_string(),
        arguments: parsed,
    });

    events
}

fn extract_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

/// Partition rule injections by timing. Before-timing injections are
/// buffered and applied at the top of the next iteration so their content
/// reaches the provider on the *next* call; After-timing injections are
/// applied immediately after the current tool batch.
pub(super) fn partition_injections_by_timing(
    injections: Vec<RuleInjection>,
) -> (Vec<RuleInjection>, Vec<RuleInjection>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for inj in injections {
        match inj.timing {
            TriggerTiming::Before => before.push(inj),
            TriggerTiming::After => after.push(inj),
        }
    }
    (before, after)
}

/// Apply a batch of [`RuleInjection`] results to the running prompt and the
/// session store.
///
/// - [`RuleDeliveryMode::SystemContextAppend`] appends to
///   [`LoopContext::system_sections`]. The base system message in `messages`
///   is rebuilt from `loop_context.system_instruction()` at the top of the
///   next iteration.
/// - [`RuleDeliveryMode::ContextInjection`] becomes a `[Context: id] body`
///   user-role message — recorded as a `SessionEvent::UserMessage` for
///   audit and pushed into `messages` so the next provider call sees it.
/// - [`RuleDeliveryMode::MessageDelivery`] becomes a `[Rule: id] body`
///   user-role message under the same dual-append pattern.
pub(super) async fn apply_rule_injections(
    loop_context: &mut LoopContext,
    injections: Vec<RuleInjection>,
    messages: &mut Vec<Message>,
    store: &EventStore,
) -> Result<(), SessionError> {
    for injection in injections {
        match injection.delivery {
            RuleDeliveryMode::SystemContextAppend => {
                loop_context.append_system_section(injection.content);
            }
            RuleDeliveryMode::ContextInjection => {
                let formatted = format!("[Context: {}] {}", injection.rule_id, injection.content);
                push_rule_user_message(loop_context, messages, store, formatted).await?;
            }
            RuleDeliveryMode::MessageDelivery => {
                let formatted = format!("[Rule: {}] {}", injection.rule_id, injection.content);
                push_rule_user_message(loop_context, messages, store, formatted).await?;
            }
        }
    }
    Ok(())
}

async fn push_rule_user_message(
    loop_context: &LoopContext,
    messages: &mut Vec<Message>,
    store: &EventStore,
    formatted: String,
) -> Result<(), SessionError> {
    append_and_notify(
        store,
        SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: formatted.clone(),
        },
        loop_context.hooks.as_deref(),
    )
    .await?;
    messages.push(Message {
        role: MessageRole::User,
        content: Some(formatted),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_call_kind: None,
    });
    Ok(())
}
