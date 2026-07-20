//! Resume-time repair of a transcript killed mid-turn.
//!
//! A hard process kill (`SIGKILL`, or the norn session killed mid-turn)
//! can leave a durable `AssistantMessage` carrying tool calls with no
//! matching local result: the assistant turn is persisted the moment the
//! model requests the tools, and each result is persisted only after that
//! tool executes, so a process that dies in between leaves one or more tool
//! calls with no recorded output. On the next `--resume-if-exists`,
//! [`events_to_messages`](crate::session::conversion::events_to_messages)
//! projects each dangling call into a `function_call` input item with no
//! `function_call_output`, and the `OpenAI` Responses API rejects the
//! request with HTTP 400 "No tool output found for function call call_…"
//! on every retry — deterministically, because the durable append-only log
//! never changes on its own.
//!
//! [`repair_dangling_tool_calls`] heals the log at open time by appending a
//! synthetic `ToolResult` for every tool call that has neither a legacy
//! `ToolResult` nor a canonical function/custom call-output item,
//! so the reopened transcript is well-formed before the first provider
//! request is assembled. It is:
//!
//! - **Deterministic** — synthesizes a fixed error output, in append order.
//! - **Idempotent** — a log where every call already has a result is a
//!   no-op, so a healthy session file is left byte-for-byte unchanged and
//!   re-running the repair appends nothing.
//! - **Persistent** — every synthetic result is appended through the
//!   store's write-through sink, so the on-disk session file is healthy
//!   afterwards, not just the in-memory view.
//!
//! The repair *synthesizes* the missing result rather than dropping the
//! dangling call: norn's event log is append-only and audit-grade, so
//! dropping would mean rewriting history and would also discard the
//! assistant turn's reasoning/encrypted-content replay items. The
//! synthesized result flows through both request-assembly paths (manual
//! replay and provider-threaded) and stays queryable in the resumed action
//! log ([`rebuild_action_log`](crate::agent::resume::rebuild_action_log)
//! records it as an `Error` outcome).

use crate::error::SessionError;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;

/// Output recorded for a tool call whose result never persisted before the
/// session was killed.
///
/// The `error` object keeps the call classified as
/// [`Outcome::Error`](crate::session::Outcome) on action-log rebuild (the
/// presence of the `error` key is the failure signal), and the wording is
/// deliberately distinct from the in-process cancel path's "execution
/// cancelled before completion" so an operator reading a transcript or the
/// action log can tell a resume-repair result apart from a live mid-turn
/// cancellation.
const INTERRUPTED_OUTPUT: &str =
    "interrupted — output unavailable; re-run the call if still needed";

/// Whether `event` is the local-only result synthesized by resume repair.
///
/// This distinction is also a response-thread safety boundary: the result is
/// absent from the provider state named by the preceding response ID, so the
/// first resumed request must replay the healed transcript in full.
pub(crate) fn is_interrupted_tool_result(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ToolResult { output, .. }
            if output.get("error").and_then(serde_json::Value::as_str)
                == Some(INTERRUPTED_OUTPUT)
    )
}

/// Append a synthetic [`SessionEvent::ToolResult`] for every tool call in
/// `store` that has no recorded output, healing a transcript killed
/// mid-turn so a resumed session assembles a well-formed provider request.
///
/// A tool call is *dangling* when it remains in the effective durable prompt
/// view and no later family-compatible canonical output or legacy
/// `ToolResult` consumes it — exactly the condition the Responses API rejects
/// with "no tool output found for function call". Compacted or suppressed
/// calls are not repaired back into visibility. Every effective dangling call
/// receives one synthetic result carrying [`INTERRUPTED_OUTPUT`], appended in
/// the calls' original order after all existing events (so each synthetic
/// output still follows its originating call on the wire).
///
/// Returns the `call_id`s repaired, in append order — empty when the log
/// was already well-formed. The caller logs the single honest summary line
/// (it holds the session id); this function performs no logging so it can
/// be composed without duplicating that line.
///
/// # Errors
///
/// Propagates [`SessionError`] from the underlying
/// [`EventStore::append`] — a resume that cannot heal the log is surfaced,
/// not silently left to 400 on the first request.
pub fn repair_dangling_tool_calls(store: &EventStore) -> Result<Vec<String>, SessionError> {
    let events = store.events();

    let dangling = crate::session::unresolved_effective_local_tool_calls(&events);

    let mut repaired = Vec::with_capacity(dangling.len());
    for call in dangling {
        let call_id = call.call_id;
        // `last_event_id` is re-read per append so the parent chain links
        // correctly across the whole synthesized batch.
        let event = SessionEvent::ToolResult {
            base: EventBase::new(store.last_event_id()),
            tool_call_id: call_id.clone(),
            tool_name: call.name,
            output: serde_json::json!({ "error": INTERRUPTED_OUTPUT }),
            spool_ref: None,
            duration_ms: 0,
        };
        store.append(event)?;
        repaired.push(call_id);
    }

    Ok(repaired)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::provider::openai::request::build_payload;
    use crate::provider::request::{
        Message, MessageRole, ProviderRequest, ToolCallKind as ReqToolCallKind,
    };
    use crate::session::conversion::events_to_messages;
    use crate::session::events::{EventUsage, ToolCallEvent};
    use crate::session::manager::{CreateSessionOptions, SessionManager};
    use crate::session::persistence::resolved_session_file_path;
    use crate::session::store::DurabilityPolicy;

    fn assistant_with_calls(calls: &[(&str, &str)]) -> SessionEvent {
        SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: String::new(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: calls
                .iter()
                .map(|(call_id, name)| ToolCallEvent {
                    call_id: (*call_id).to_owned(),
                    name: (*name).to_owned(),
                    arguments: serde_json::json!({"path": "/tmp/x"}),
                    kind: ReqToolCallKind::Function,
                    caller: crate::provider::request::ToolCallCaller::Absent,
                })
                .collect(),
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_owned(),
            response_id: Some("resp_dangling".to_owned()),
        }
    }

    fn tool_result(call_id: &str, name: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: call_id.to_owned(),
            tool_name: name.to_owned(),
            output: serde_json::json!({"content": "real output"}),
            spool_ref: None,
            duration_ms: 3,
        }
    }

    fn user(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn interrupted_result_count(events: &[SessionEvent], call_id: &str) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SessionEvent::ToolResult { tool_call_id, output, .. }
                        if tool_call_id == call_id
                            && output.get("error").and_then(serde_json::Value::as_str)
                                == Some(INTERRUPTED_OUTPUT)
                )
            })
            .count()
    }

    /// A session killed mid-turn (a persisted assistant turn with a tool
    /// call whose result never landed) is healed on repair: a synthetic
    /// interrupted result is appended AND persisted, so reopening the
    /// session file replays a well-formed transcript.
    #[test]
    fn repair_synthesizes_and_persists_missing_result() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(dir.path());
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/repo".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        let id = opened.entry.id.clone();
        for event in [
            user("do it"),
            assistant_with_calls(&[("call_a", "read"), ("call_b", "bash")]),
            tool_result("call_a", "read"),
        ] {
            opened.store.append(event).unwrap();
        }

        let repaired = repair_dangling_tool_calls(&opened.store).unwrap();
        assert_eq!(
            repaired,
            vec!["call_b".to_owned()],
            "only the dangling call"
        );
        assert_eq!(
            interrupted_result_count(&opened.store.events(), "call_b"),
            1,
            "one synthetic interrupted result for the dangling call",
        );
        // The already-answered call keeps its real result untouched.
        assert_eq!(
            interrupted_result_count(&opened.store.events(), "call_a"),
            0
        );
        drop(opened);

        // Reopening from disk sees the healed transcript: the synthetic
        // result was persisted through the sink, not just held in memory.
        let reopened = SessionManager::new(dir.path())
            .resume(&id, DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(
            interrupted_result_count(&reopened.store.events(), "call_b"),
            1,
            "the synthetic result survived the round-trip to disk",
        );
    }

    /// Re-running the repair over an already-healed log appends nothing and
    /// returns an empty set — it is idempotent.
    #[test]
    fn repair_is_idempotent() {
        let store = EventStore::new();
        for event in [user("go"), assistant_with_calls(&[("call_x", "read")])] {
            store.append(event).unwrap();
        }

        let first = repair_dangling_tool_calls(&store).unwrap();
        assert_eq!(first, vec!["call_x".to_owned()]);
        let after_first = store.events().len();

        let second = repair_dangling_tool_calls(&store).unwrap();
        assert!(second.is_empty(), "second pass finds nothing to repair");
        assert_eq!(
            store.events().len(),
            after_first,
            "idempotent repair appends no further events",
        );
    }

    /// A healthy session file — every tool call already answered — is left
    /// byte-for-byte unchanged: the repair appends nothing to disk.
    #[test]
    fn healthy_transcript_untouched_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(dir.path());
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/repo".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        for event in [
            user("what dir?"),
            assistant_with_calls(&[("call_ok", "bash")]),
            tool_result("call_ok", "bash"),
            SessionEvent::AssistantMessage {
                response_items: Vec::new(),
                base: EventBase::new(None),
                content: "/home/user".to_owned(),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_owned(),
                response_id: None,
            },
        ] {
            opened.store.append(event).unwrap();
        }

        let file = resolved_session_file_path(dir.path(), &opened.entry);
        let before = std::fs::read(&file).unwrap();
        let event_count = opened.store.events().len();

        let repaired = repair_dangling_tool_calls(&opened.store).unwrap();
        assert!(repaired.is_empty(), "healthy transcript needs no repair");
        assert_eq!(
            opened.store.events().len(),
            event_count,
            "no event appended to a healthy log",
        );

        let after = std::fs::read(&file).unwrap();
        assert_eq!(
            before, after,
            "healthy session file is byte-for-byte unchanged"
        );
    }

    /// An assistant turn with three calls, only two answered, repairs
    /// exactly the one missing result — leaving the answered pair alone.
    #[test]
    fn repair_covers_only_the_missing_call_in_a_partial_set() {
        let store = EventStore::new();
        for event in [
            user("triple"),
            assistant_with_calls(&[("c1", "read"), ("c2", "bash"), ("c3", "edit")]),
            tool_result("c1", "read"),
            tool_result("c3", "edit"),
        ] {
            store.append(event).unwrap();
        }

        let repaired = repair_dangling_tool_calls(&store).unwrap();
        assert_eq!(repaired, vec!["c2".to_owned()]);
        let events = store.events();
        assert_eq!(interrupted_result_count(&events, "c2"), 1);
        assert_eq!(interrupted_result_count(&events, "c1"), 0);
        assert_eq!(interrupted_result_count(&events, "c3"), 0);
    }

    /// An empty log and a log whose final assistant turn made no tool calls
    /// both repair to no-ops — there is nothing dangling to heal.
    #[test]
    fn repair_is_noop_without_dangling_calls() {
        let empty = EventStore::new();
        assert!(repair_dangling_tool_calls(&empty).unwrap().is_empty());

        let plain = EventStore::new();
        plain.append(user("hi")).unwrap();
        plain
            .append(SessionEvent::AssistantMessage {
                response_items: Vec::new(),
                base: EventBase::new(None),
                content: "hello".to_owned(),
                thinking: String::new(),
                reasoning: Vec::new(),
                tool_calls: Vec::new(),
                usage: EventUsage::default(),
                stop_reason: "end_turn".to_owned(),
                response_id: None,
            })
            .unwrap();
        assert!(repair_dangling_tool_calls(&plain).unwrap().is_empty());
    }

    /// After repair, the request assembled from the healed transcript
    /// carries a `function_call_output` for every `function_call` — the
    /// exact wire condition whose absence produced the Responses API 400.
    /// Builds the payload locally; no real provider is contacted.
    #[test]
    fn assembled_request_has_no_dangling_function_call() {
        let store = EventStore::new();
        for event in [
            user("assemble"),
            assistant_with_calls(&[("call_live", "read"), ("call_killed", "bash")]),
            tool_result("call_live", "read"),
        ] {
            store.append(event).unwrap();
        }
        repair_dangling_tool_calls(&store).unwrap();

        // Assemble exactly as the loop does: a System prefix plus the
        // replayed history projected to provider messages.
        let mut messages = vec![Message {
            response_items: Vec::new(),
            role: MessageRole::System,
            content: Some("you are helpful".to_owned()),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_call_kind: None,
            tool_call_caller: crate::provider::request::ToolCallCaller::Absent,
        }];
        messages.extend(events_to_messages(&store.events()));

        let request = ProviderRequest {
            messages,
            tools: Vec::new(),
            model: "test-model".to_owned(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            config: None,
            cache_key: None,
            previous_response_id: None,
            store: false,
            context_management: None,
        };
        let payload = build_payload(&request, "codex_subscription").expect("build_payload");
        let input = payload["input"].as_array().expect("input array");

        let call_ids: HashSet<&str> = input
            .iter()
            .filter(|item| item["type"] == "function_call")
            .filter_map(|item| item["call_id"].as_str())
            .collect();
        let output_ids: HashSet<&str> = input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .filter_map(|item| item["call_id"].as_str())
            .collect();

        assert!(
            call_ids.is_subset(&output_ids),
            "every function_call must have a function_call_output; \
             calls={call_ids:?} outputs={output_ids:?}",
        );
        assert!(
            output_ids.contains("call_killed"),
            "the previously-dangling call now carries a synthetic output",
        );
        // And that synthetic output is the interrupted marker, not a fake
        // success.
        let killed_output = input
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call_killed")
            .and_then(|item| item["output"].as_str())
            .expect("killed call output present");
        assert!(
            killed_output.contains("interrupted"),
            "synthetic output carries the interrupted marker: {killed_output}",
        );
    }
}

#[cfg(test)]
mod effective_view_tests;
