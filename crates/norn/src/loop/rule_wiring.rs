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
use crate::integration::hooks::HookRegistry;
use crate::r#loop::helpers::append_and_notify;
use crate::r#loop::loop_context::LoopContext;
use crate::provider::request::{Message, MessageRole};
use crate::rules::types::{PathOperation, RuleInjection, RuntimeEvent, TriggerTiming};
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

/// Collapse an injection batch to at most one injection per rule id,
/// keeping the first occurrence.
///
/// A single tool batch may run several calls concurrently, each observing
/// the same pre-batch presence snapshot (the presence set is only rebuilt
/// between prompt-construction passes, never mid-batch). A broad rule that
/// matches two of those calls would otherwise fire twice for one context
/// window — duplicate audit events, doubled token cost, and a doubled
/// system section on re-materialization. The rule's guidance is identical
/// regardless of which matching call triggered it, so the first firing is
/// authoritative.
pub(super) fn dedup_injections_by_rule(injections: Vec<RuleInjection>) -> Vec<RuleInjection> {
    let mut seen = std::collections::HashSet::new();
    injections
        .into_iter()
        .filter(|injection| seen.insert(injection.rule_id.clone()))
        .collect()
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
/// Every fired rule — regardless of delivery mode — is persisted as a
/// [`SessionEvent::RuleInjection`] event. That event is the single source
/// of truth the rest of the lifecycle reads: the prompt view tags it
/// [`ContentTag::Rule`](crate::agent_loop::context::ContentTag::Rule) so the
/// engine's presence set tracks it, and it survives resume/compaction as an
/// immutable audit record of which rule fired and how.
///
/// - [`RuleDeliveryMode::SystemContextAppend`] additionally appends the raw
///   content to [`LoopContext::system_sections`] for the current iteration.
///   On every subsequent prompt-construction pass the content is
///   re-materialized from the persisted event (see
///   [`LoopContext::materialize_system_context_rules`]), so it persists for
///   the remainder of the session yet is dropped the moment the event is
///   compacted out — at which point the rule re-fires on its next trigger.
/// - [`RuleDeliveryMode::ContextInjection`] / [`RuleDeliveryMode::MessageDelivery`]
///   additionally push the delivery-prefixed content into the in-flight
///   `messages` so the current provider call sees it. On resume the same
///   prefixed message is reconstructed from the persisted event via
///   [`crate::session::conversion`], so nothing is pushed twice.
pub(super) async fn apply_rule_injections(
    loop_context: &mut LoopContext,
    injections: Vec<RuleInjection>,
    messages: &mut Vec<Message>,
    store: &EventStore,
) -> Result<(), SessionError> {
    for injection in injections {
        let rule_id = injection.rule_id.to_string();
        let live_message = injection
            .delivery
            .format_conversation_content(&rule_id, &injection.content);

        append_and_notify(
            store,
            SessionEvent::RuleInjection {
                base: EventBase::new(store.last_event_id()),
                rule_id,
                delivery: injection.delivery.clone(),
                timing: injection.timing.clone(),
                content: injection.content.clone(),
            },
            loop_context.hooks.as_deref(),
        )
        .await?;

        match live_message {
            None => loop_context.append_system_section(injection.content),
            Some(formatted) => messages.push(Message {
                role: MessageRole::User,
                content: Some(formatted),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_call_kind: None,
            }),
        }
    }
    Ok(())
}

/// Persist the [`SessionEvent::RuleInjection`] audit event for every fired
/// Before-timing injection that no `build_request` ever consumed, without
/// the live-delivery side effects (the message push / system-section append
/// that [`apply_rule_injections`] also performs).
///
/// Those live-delivery effects are meaningless on a step-exit path — the
/// `messages` vec and the current iteration's `system_sections` are both
/// discarded when the step ends, and the delivered content re-enters the
/// next step reconstructed from this very event (see
/// [`crate::session::conversion`] and
/// [`LoopContext::materialize_system_context_rules`](crate::r#loop::loop_context::LoopContext::materialize_system_context_rules)).
/// What must survive is the audit event, whose presence keeps "fired" and
/// "in context" coherent identically to After-timing.
///
/// This is the single persist path shared by the normal step-exit drain
/// ([`StepMachine::run`](crate::r#loop::runner)) and the step-timeout drop
/// path ([`run_agent_step_common`](crate::r#loop::runner)); both must leave
/// an identical record so a fired firing is never discarded without one.
///
/// # Errors
///
/// Propagates the first event-store append failure. Callers log it and
/// never let it rewrite an already-decided step result.
pub(super) async fn persist_before_injection_audit(
    store: &EventStore,
    hooks: Option<&HookRegistry>,
    injections: &[RuleInjection],
) -> Result<(), SessionError> {
    for injection in injections {
        append_and_notify(
            store,
            SessionEvent::RuleInjection {
                base: EventBase::new(store.last_event_id()),
                rule_id: injection.rule_id.to_string(),
                delivery: injection.delivery.clone(),
                timing: injection.timing.clone(),
                content: injection.content.clone(),
            },
            hooks,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::rules::types::{DeliveryMode, RuleId};

    fn injection(id: &str) -> RuleInjection {
        RuleInjection {
            rule_id: RuleId::from(id),
            delivery: DeliveryMode::ContextInjection,
            timing: TriggerTiming::Before,
            content: id.to_owned(),
        }
    }

    #[test]
    fn dedup_keeps_first_occurrence_per_rule() {
        let deduped = dedup_injections_by_rule(vec![
            injection("a"),
            injection("b"),
            injection("a"),
            injection("a"),
        ]);
        let ids: Vec<_> = deduped
            .iter()
            .map(|i| i.rule_id.as_str().to_owned())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn dedup_empty_is_empty() {
        assert!(dedup_injections_by_rule(Vec::new()).is_empty());
    }

    #[test]
    fn build_runtime_events_read_emits_path_and_tool() {
        let events = build_runtime_events("read", r#"{"path":"src/lib.rs"}"#);
        assert!(matches!(
            events.first(),
            Some(RuntimeEvent::PathChanged {
                operation: PathOperation::Read,
                ..
            })
        ));
        assert!(matches!(
            events.last(),
            Some(RuntimeEvent::ToolInvoked { .. })
        ));
    }
}
