//! Identity and display-capability regressions, including every committed event family.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use serde_json::json;
use uuid::Uuid;

use super::body::{BodyRange, BodyRef, DisplayField, DisplayText, resolve_committed_body};
use super::contract::{AcceptedModel, HistoryCursor, SessionIdentity, ViewSource};
use super::{SessionProjection, ViewError, project_committed};
use crate::model_selection::ModelRuntime;
use crate::provider::request::{ToolCallCaller, ToolCallKind};
use crate::provider::response_item::{
    ResponseItem, ResponseStreamProvenance, ResponseTranscriptItem,
};
use crate::session::events::{
    ChildBranchKind, ContextMarkKind, EventBase, EventId, EventUsage, ProviderEpochBoundaryReason,
    SessionEvent, ToolCallEvent,
};

pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(super) fn source() -> ViewSource {
    ViewSource {
        session: SessionIdentity::Persisted("test-session".to_owned()),
        agent_id: Uuid::new_v4(),
        parent_agent_id: None,
        store_generation: Uuid::new_v4(),
    }
}

pub(super) fn model() -> Result<AcceptedModel, crate::error::ConfigError> {
    let runtime = ModelRuntime::new(
        None,
        "fixture-model",
        Some(4096),
        None,
        None,
        BTreeMap::new(),
    )?;
    Ok(AcceptedModel::capture(&runtime, 3))
}

pub(super) fn assistant(
    items: Vec<ResponseTranscriptItem>,
    calls: Vec<ToolCallEvent>,
) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: items,
        content: "flat projection".to_owned(),
        thinking: "flat summary".to_owned(),
        reasoning: Vec::new(),
        tool_calls: calls,
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    }
}

pub(super) fn response(
    raw: serde_json::Value,
) -> Result<ResponseTranscriptItem, crate::provider::response_item::ResponseItemError> {
    Ok(ResponseTranscriptItem {
        item: ResponseItem::from_value(raw)?,
        provenance: ResponseStreamProvenance::default(),
    })
}

pub(super) fn call(
    id: &str,
    name: &str,
    arguments: serde_json::Value,
    kind: ToolCallKind,
) -> ToolCallEvent {
    ToolCallEvent {
        call_id: id.to_owned(),
        name: name.to_owned(),
        arguments,
        kind,
        caller: ToolCallCaller::Absent,
    }
}

pub(super) fn cursor(source: &ViewSource, ordinal: usize, event: &SessionEvent) -> HistoryCursor {
    HistoryCursor::event(source.clone(), ordinal, event.base().id.clone())
}

#[test]
fn cursor_binds_source_generation_position_and_event() -> TestResult {
    let owner = source();
    let event = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "hello".to_owned(),
    };
    let position = cursor(&owner, 2, &event);
    position.validate(&owner, 2, &event.base().id)?;
    let mut reopened = owner.clone();
    reopened.store_generation = Uuid::new_v4();
    assert!(matches!(
        position.validate(&reopened, 2, &event.base().id),
        Err(ViewError::SourceMismatch { .. })
    ));
    assert!(position.validate(&owner, 1, &event.base().id).is_err());
    assert!(position.validate(&owner, 2, &EventId::new()).is_err());
    assert!(
        HistoryCursor::empty(owner.clone())
            .validate(&owner, 0, &event.base().id)
            .is_err()
    );
    Ok(())
}

#[test]
fn display_data_cannot_emit_terminal_or_bidi_controls() {
    let raw = "ملف\u{061c}\u{200e}\u{200f}\u{202e}report.rs\u{2066}\u{2069} desc\u{1b}]52;c;payload\u{7}\n👩\u{200d}💻";
    let display = DisplayText::new(raw);
    assert!(display.as_str().contains("ملف"));
    assert!(display.as_str().contains("👩\u{200d}💻"));
    assert!(display.as_str().contains("\\u{61c}"));
    assert!(display.as_str().contains("\\u{200e}"));
    assert!(display.as_str().contains("\\u{200f}"));
    assert!(!display.as_str().contains('\u{1b}'));
    assert!(!display.as_str().contains('\u{7}'));
    assert!(display.as_str().contains('\n'));
}

#[test]
fn ranges_preserve_original_utf8_offsets_and_refuse_zero_progress() -> TestResult {
    let text = "a🦀é\n";
    let demand = NonZeroUsize::new(5).ok_or("fixture demand is zero")?;
    let (first, next) = BodyRange {
        offset: 0,
        max_bytes: demand,
    }
    .slice(text)?;
    assert_eq!(first, "a🦀");
    assert_eq!(next, 5);
    let (second, end) = BodyRange {
        offset: next,
        max_bytes: demand,
    }
    .slice(text)?;
    assert_eq!(second, "é\n");
    assert_eq!(end, text.len());
    assert!(matches!(
        BodyRange {
            offset: 2,
            max_bytes: demand
        }
        .slice(text),
        Err(ViewError::InvalidRange { .. })
    ));
    let one = NonZeroUsize::new(1).ok_or("fixture demand is zero")?;
    assert!(matches!(
        BodyRange {
            offset: 1,
            max_bytes: one
        }
        .slice(text),
        Err(ViewError::RangeTooSmall { .. })
    ));
    Ok(())
}

#[test]
fn custom_lifecycle_requires_matching_phase_and_redacts_malformed_values() -> TestResult {
    let delivered = crate::provider::agent_event::AgentMessageLifecycle::Delivered {
        message_id: Uuid::new_v4(),
        from_id: Uuid::new_v4(),
        from: "sender".to_owned(),
        to_id: Uuid::new_v4(),
        seq: None,
        delivered_at: chrono::Utc::now(),
    };
    let mismatched = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "agent_message.sent".to_owned(),
        data: serde_json::to_value(delivered)?,
    };
    assert!(matches!(
        resolve_committed_body(&mismatched, &DisplayField::CustomLifecycle),
        Err(ViewError::LifecycleMismatch { .. })
    ));
    let malformed = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "agent_message.sent".to_owned(),
        data: json!({"phase":"opaque-private-marker"}),
    };
    let error = resolve_committed_body(&malformed, &DisplayField::CustomLifecycle)
        .err()
        .ok_or("malformed lifecycle was accepted")?;
    assert!(!format!("{error} {error:?}").contains("opaque-private-marker"));
    Ok(())
}

#[test]
fn every_response_item_variant_has_an_explicit_approved_projection() -> TestResult {
    let raw_items = vec![
        json!({"type":"message","id":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"text","annotations":[],"logprobs":[]}]}),
        json!({"type":"reasoning","id":"reasoning","summary":[{"type":"summary_text","text":"summary"}],"encrypted_content":"private-reasoning"}),
        json!({"type":"function_call","id":"function","call_id":"function-call","name":"read","arguments":"{\"path\":\"file\"}"}),
        json!({"type":"custom_tool_call","id":"custom","call_id":"custom-call","name":"patch","input":"freeform input"}),
        json!({"type":"web_search_call","id":"search","status":"completed","action":{"private":"private-search"}}),
        json!({"type":"compaction","id":"compaction","encrypted_content":"private-compaction"}),
        json!({"type":"image_generation_call","id":"image","result":"private-image"}),
        json!({"type":"unknown_future","id":"future","state":"private-future"}),
    ];
    let owner = source();
    let mut seen = std::collections::BTreeSet::new();
    for (ordinal, raw) in raw_items.into_iter().enumerate() {
        let item = response(raw)?;
        let name = match &item.item {
            ResponseItem::Message(_) => "message",
            ResponseItem::Reasoning(_) => "reasoning",
            ResponseItem::FunctionCall(_) => "function",
            ResponseItem::CustomToolCall(_) => "custom",
            ResponseItem::WebSearchCall(_) => "search",
            ResponseItem::Compaction(_) => "compaction",
            ResponseItem::Known(_) => "known",
            ResponseItem::Opaque(_) => "opaque",
        };
        assert!(seen.insert(name));
        let event = assistant(vec![item], Vec::new());
        let record = project_committed(&cursor(&owner, ordinal, &event), &event)?;
        for row in record.items() {
            for body in &row.bodies {
                let super::BodyOrigin::Committed { field, .. } = body.origin() else {
                    return Err("historical body was not committed".into());
                };
                assert!(!resolve_committed_body(&event, field)?.contains("private-"));
            }
        }
    }
    assert_eq!(seen.len(), 8);
    Ok(())
}

#[test]
fn authoritative_allowlist_excludes_flat_opaque_and_raw_reasoning_bodies() -> TestResult {
    let owner = source();
    let event = assistant(
        vec![
            response(
                json!({"type":"message","id":"msg","role":"assistant","status":"completed","content":[{"type":"output_text","text":"actual","annotations":[],"logprobs":[]},{"type":"refusal","refusal":"declined"}]}),
            )?,
            response(
                json!({"type":"reasoning","id":"reason","summary":[{"type":"summary_text","text":"approved summary"}],"content":[{"type":"reasoning_text","text":"raw-secret"}],"encrypted_content":"encrypted-secret"}),
            )?,
            response(json!({"type":"future_private","id":"opaque","payload":"opaque-secret"}))?,
        ],
        Vec::new(),
    );
    let position = cursor(&owner, 0, &event);
    assert!(BodyRef::committed(position.clone(), &event, DisplayField::AssistantContent).is_err());
    assert_eq!(
        resolve_committed_body(&event, &DisplayField::ResponseText { item: 0, part: 0 })?,
        "actual"
    );
    assert_eq!(
        resolve_committed_body(&event, &DisplayField::ResponseRefusal { item: 0, part: 1 })?,
        "declined"
    );
    assert_eq!(
        resolve_committed_body(&event, &DisplayField::ResponseSummary { item: 1, part: 0 })?,
        "approved summary"
    );
    assert!(
        resolve_committed_body(&event, &DisplayField::ResponseText { item: 2, part: 0 }).is_err()
    );
    let compact = project_committed(&position, &event)?;
    let debug = format!("{compact:?}");
    for secret in [
        "raw-secret",
        "encrypted-secret",
        "opaque-secret",
        "flat projection",
    ] {
        assert!(!debug.contains(secret));
    }
    let custom = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "provider.state.provenance".to_owned(),
        data: json!({"token":"private"}),
    };
    assert!(
        BodyRef::committed(
            cursor(&owner, 1, &custom),
            &custom,
            DisplayField::CustomLifecycle
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn all_committed_variants_have_explicit_compact_dispositions() -> TestResult {
    let owner = source();
    let events = vec![
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "input".to_owned(),
        },
        assistant(Vec::new(), Vec::new()),
        SessionEvent::SpokenResponse {
            base: EventBase::new(None),
            content: json!({"text":"spoken"}),
        },
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "orphan".to_owned(),
            tool_name: "read".to_owned(),
            output: json!({"text":"result"}),
            spool_ref: None,
            duration_ms: 8,
        },
        SessionEvent::ModelChange {
            base: EventBase::new(None),
            old_model: "old".to_owned(),
            new_model: "new".to_owned(),
        },
        SessionEvent::ProviderEpochBoundary {
            base: EventBase::new(None),
            reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
        },
        SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "summary".to_owned(),
            replaced_event_ids: Vec::new(),
        },
        SessionEvent::ChildBranch {
            base: EventBase::new(None),
            parent_session_id: None,
            child_session_id: None,
            path_address: "/root/child".to_owned(),
            parent_event_anchor: None,
            kind: ChildBranchKind::Spawn,
        },
        SessionEvent::ForkComplete {
            base: EventBase::new(None),
            forked_session_id: None,
            result_summary: json!({"done":true}),
            usage: EventUsage::default(),
            duration_ms: 3,
        },
        SessionEvent::Label {
            base: EventBase::new(None),
            label: "label".to_owned(),
            description: Some("description".to_owned()),
        },
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: "unknown".to_owned(),
            data: json!({"private":"secret"}),
        },
        SessionEvent::ContextMark {
            base: EventBase::new(None),
            mark: ContextMarkKind::Suppress,
            target_event_id: EventId::new(),
        },
        SessionEvent::RuleInjection {
            base: EventBase::new(None),
            rule_id: "rule".to_owned(),
            origin: None,
            delivery: crate::rules::types::DeliveryMode::ContextInjection,
            timing: crate::rules::types::TriggerTiming::Before,
            content: "rule input".to_owned(),
        },
    ];
    let mut dispositions = std::collections::BTreeSet::new();
    let mut view = SessionProjection::new(owner.clone());
    for (ordinal, event) in events.iter().enumerate() {
        let disposition = match event {
            SessionEvent::UserMessage { .. } => "input",
            SessionEvent::AssistantMessage { .. } => "assistant",
            SessionEvent::SpokenResponse { .. } => "spoken",
            SessionEvent::ToolResult { .. } => "tool",
            SessionEvent::ModelChange { .. } => "model",
            SessionEvent::ProviderEpochBoundary { .. } => "epoch",
            SessionEvent::Compaction { .. } => "compaction",
            SessionEvent::ChildBranch { .. } => "branch",
            SessionEvent::ForkComplete { .. } => "fork",
            SessionEvent::Label { .. } => "label",
            SessionEvent::Custom { .. } => "custom",
            SessionEvent::ContextMark { .. } => "mark",
            SessionEvent::RuleInjection { .. } => "rule",
        };
        dispositions.insert(disposition);
        let record = project_committed(&cursor(&owner, ordinal, event), event)?;
        assert!(!record.items().is_empty());
        view.apply_history_record(&record)?;
    }
    assert_eq!(dispositions.len(), events.len());
    assert!(view.items().all(|item| item.model.is_none()));
    Ok(())
}

#[test]
fn routine_provider_bookkeeping_is_typed_metadata_without_losing_record_identity() -> TestResult {
    let owner = source();
    let events = [
        SessionEvent::ProviderEpochBoundary {
            base: EventBase::new(None),
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
        },
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: crate::session::PROVIDER_STATE_PROVENANCE_EVENT_TYPE.to_owned(),
            data: json!({"private": "not a display capability"}),
        },
    ];
    for (ordinal, event) in events.iter().enumerate() {
        let position = cursor(&owner, ordinal, event);
        let record = project_committed(&position, event)?;
        assert_eq!(record.cursor(), &position);
        let [item] = record.items() else {
            return Err("expected retained metadata item".into());
        };
        assert!(matches!(item.kind, super::ViewItemKind::Metadata));
        assert!(!item.label.as_str().is_empty());
        assert!(item.bodies.is_empty());
        assert!(!format!("{item:?}").contains("not a display capability"));
    }
    let unknown = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "unexpected.event".to_owned(),
        data: json!({}),
    };
    let record = project_committed(&cursor(&owner, 2, &unknown), &unknown)?;
    assert!(matches!(
        record.items()[0].kind,
        super::ViewItemKind::Unavailable
    ));
    let input = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "hello".to_owned(),
    };
    let record = project_committed(&cursor(&owner, 3, &input), &input)?;
    assert!(matches!(record.items()[0].kind, super::ViewItemKind::Input));
    assert_eq!(record.items()[0].label.as_str(), "Input");
    assert_eq!(record.items()[0].bodies.len(), 1);
    Ok(())
}
