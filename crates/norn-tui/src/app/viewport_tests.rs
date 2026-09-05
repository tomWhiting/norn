//! Logical navigation regressions using real semantic projection updates, without a terminal.

use std::num::NonZeroUsize;
use std::sync::Arc;

use norn::provider::agent_event::{AgentEvent, AgentEventKind};
use norn::provider::events::ProviderEvent;
use norn::provider::request::{ToolCallCaller, ToolCallKind};
use norn::session::branch::SessionBinding;
use norn::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use norn::session::store::{EventStore, HistoryAnchor, HistoryDirection, HistoryRead};
use norn::session_view::{
    AcceptedModel, BodyRepresentation, DisplayText, HistoryRecord, SessionIdentity, ViewItemKind,
};
use serde_json::json;
use uuid::Uuid;

use super::super::focus::{Focus, FocusAvailability, FocusDirection, FocusError, FocusState};
use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn required<T>(value: Option<T>) -> TestResult<T> {
    value.ok_or_else(|| std::io::Error::other("required viewport fixture value is absent").into())
}

fn source() -> ViewSource {
    ViewSource {
        session: SessionIdentity::Ephemeral(Uuid::new_v4()),
        agent_id: Uuid::new_v4(),
        parent_agent_id: None,
        store_generation: Uuid::new_v4(),
    }
}

fn model() -> AcceptedModel {
    AcceptedModel {
        model: DisplayText::new("viewport-fixture"),
        backend: None,
        context_window: 4096,
        effort: None,
        tier: None,
        configuration_revision: 1,
    }
}

fn live(view: &mut SessionProjection, event: ProviderEvent) -> TestResult {
    view.apply_live(&AgentEvent {
        agent_id: view.source().agent_id,
        agent_role: Arc::from("viewport-fixture"),
        event: AgentEventKind::Provider(event),
    })?;
    Ok(())
}

fn body_anchor(view: &SessionProjection, item: &ItemId, offset: usize) -> TestResult<ViewAnchor> {
    Ok(ViewAnchor {
        item: item.clone(),
        position: AnchorPosition::Body {
            reference: required(required(view.item(item))?.bodies.first())?.clone(),
            original_offset: offset,
        },
    })
}

fn accepted_record(
    store: &EventStore,
    owner: &ViewSource,
    tool: bool,
) -> TestResult<HistoryRecord> {
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: if tool { "" } else { "same text" }.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: if tool {
            vec![ToolCallEvent {
                call_id: "actual-call".to_owned(),
                name: "same-name".to_owned(),
                arguments: json!("argument"),
                kind: ToolCallKind::Custom,
                caller: ToolCallCaller::Absent,
            }]
        } else {
            Vec::new()
        },
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    })?;
    let page = store.history_page(&HistoryRead {
        source: owner.clone(),
        anchor: HistoryAnchor::Start,
        direction: HistoryDirection::After,
        max_events: required(NonZeroUsize::new(1))?,
    })?;
    required(page.records.into_iter().next())
}

#[test]
fn user_navigation_pins_and_activity_preserves_exact_logical_state() -> TestResult {
    let mut view = SessionProjection::new(source());
    let id = view.record_local_body(
        ViewItemKind::Text,
        "older output",
        "abé🙂\nunchanged",
        BodyRepresentation::Text,
    )?;
    let anchor = body_anchor(&view, &id, 2)?;
    let mut viewport = Viewport::new(view.source().clone(), true);
    viewport.scroll_to(anchor.clone(), &view)?;
    viewport.select(id.clone(), &view)?;
    assert!(!viewport.follows_tail());
    let prior = viewport.clone();
    view.record_notice(ViewItemKind::Notice, "new activity")?;
    assert_eq!(
        viewport.reconcile(&view)?,
        ViewportReconciliation {
            anchor: Some(AnchorState::Current),
            selected: Some(ItemState::Current),
        }
    );
    assert_eq!(viewport, prior);
    assert_eq!(viewport.anchor(), Some(&anchor));
    assert_eq!(viewport.selected(), Some(&id));
    viewport.follow_tail();
    assert!(viewport.follows_tail());
    assert!(viewport.anchor().is_none() && viewport.selected().is_none());
    viewport.pin();
    assert!(!viewport.follows_tail());
    Ok(())
}

#[test]
fn changed_body_revision_remains_named_stale_without_offset_or_capability_substitution()
-> TestResult {
    let mut view = SessionProjection::new(source());
    view.begin_execution(Uuid::new_v4(), model())?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "abé".to_owned(),
        },
    )?;
    let id = required(view.items().next())?.id.clone();
    let anchor = body_anchor(&view, &id, 2)?;
    let mut viewport = Viewport::new(view.source().clone(), true);
    viewport.scroll_to(anchor.clone(), &view)?;
    viewport.select(id.clone(), &view)?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: " replacement".to_owned(),
        },
    )?;
    assert_eq!(
        viewport.reconcile(&view)?.anchor,
        Some(AnchorState::BodyStale)
    );
    assert_eq!(viewport.anchor(), Some(&anchor));
    assert_eq!(viewport.selected(), Some(&id));
    assert!(!viewport.follows_tail());
    let pinned = viewport.clone();
    assert!(matches!(
        viewport.scroll_to(anchor, &view),
        Err(ViewportError::BodyNotCurrent { .. })
    ));
    assert_eq!(viewport, pinned);
    Ok(())
}

#[test]
fn proven_tool_alias_chain_moves_item_identity_but_never_rebinds_an_old_body() -> TestResult {
    let store = EventStore::new();
    let owner = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    let mut view = SessionProjection::new(owner.clone());
    let attempt = view.begin_execution(Uuid::new_v4(), model())?;
    live(
        &mut view,
        ProviderEvent::ToolCallDelta {
            item_id: "actual-item".to_owned(),
            call_id: Some("actual-call".to_owned()),
            name: Some("same-name".to_owned()),
            arguments_delta: "a".to_owned(),
            kind: ToolCallKind::Custom,
        },
    )?;
    let early = required(view.items().next())?.id.clone();
    let body = body_anchor(&view, &early, 1)?;
    let mut body_viewport = Viewport::new(owner.clone(), true);
    body_viewport.scroll_to(body.clone(), &view)?;
    body_viewport.select(early.clone(), &view)?;
    let mut header = Viewport::new(owner.clone(), false);
    header.scroll_to(
        ViewAnchor {
            item: early.clone(),
            position: AnchorPosition::Header,
        },
        &view,
    )?;
    live(
        &mut view,
        ProviderEvent::ToolCallComplete {
            call_id: "actual-call".to_owned(),
            name: "same-name".to_owned(),
            arguments: "argument".to_owned(),
            kind: ToolCallKind::Custom,
        },
    )?;
    view.reconcile_history_record(&attempt, &accepted_record(&store, &owner, true)?)?;
    let committed = required(view.alias(&early))?.clone();
    assert!(matches!(committed, ItemId::Committed { .. }));
    assert_eq!(header.reconcile(&view)?.anchor, Some(AnchorState::Current));
    assert_eq!(required(header.anchor())?.item, committed);
    let status = body_viewport.reconcile(&view)?;
    assert_eq!(status.anchor, Some(AnchorState::BodyStale));
    assert_eq!(status.selected, Some(ItemState::Current));
    assert_eq!(body_viewport.selected(), Some(&committed));
    assert_eq!(required(body_viewport.anchor())?.position, body.position);
    Ok(())
}

#[test]
fn identical_text_does_not_rebind_an_unproven_retired_item() -> TestResult {
    let store = EventStore::new();
    let owner = store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
    let mut view = SessionProjection::new(owner.clone());
    let attempt = view.begin_execution(Uuid::new_v4(), model())?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "same text".to_owned(),
        },
    )?;
    let early = required(view.items().next())?.id.clone();
    let mut viewport = Viewport::new(owner.clone(), false);
    viewport.scroll_to(body_anchor(&view, &early, 4)?, &view)?;
    viewport.select(early.clone(), &view)?;
    let prior = viewport.clone();
    view.reconcile_history_record(&attempt, &accepted_record(&store, &owner, false)?)?;
    assert!(view.alias(&early).is_none());
    assert_eq!(
        viewport.reconcile(&view)?,
        ViewportReconciliation {
            anchor: Some(AnchorState::ItemUnavailable),
            selected: Some(ItemState::Unavailable),
        }
    );
    assert_eq!(viewport, prior);
    Ok(())
}

#[test]
fn failed_admission_is_atomic_and_only_actual_source_replacement_resets_state() -> TestResult {
    let mut view = SessionProjection::new(source());
    let id = view.record_notice(ViewItemKind::Notice, "selected")?;
    let mut viewport = Viewport::new(view.source().clone(), true);
    viewport.select(id.clone(), &view)?;
    viewport.scroll_to(
        ViewAnchor {
            item: id,
            position: AnchorPosition::Header,
        },
        &view,
    )?;
    let prior = viewport.clone();
    let mut foreign = SessionProjection::new(source());
    let foreign_id = foreign.record_notice(ViewItemKind::Notice, "selected")?;
    assert!(viewport.select(foreign_id.clone(), &foreign).is_err());
    assert!(viewport.select(foreign_id, &view).is_err());
    assert!(viewport.reconcile(&foreign).is_err());
    let missing = ItemId::Local {
        source: view.source().clone(),
        ordinal: 900,
    };
    assert!(matches!(
        viewport.select(missing, &view),
        Err(ViewportError::ItemUnavailable { .. })
    ));
    assert_eq!(viewport, prior);
    assert!(!viewport.replace_source(view.source().clone()));
    assert_eq!(viewport, prior);
    let mut reopened = view.source().clone();
    reopened.store_generation = Uuid::new_v4();
    assert!(viewport.replace_source(reopened.clone()));
    assert_eq!(viewport.source(), &reopened);
    assert!(viewport.anchor().is_none() && viewport.selected().is_none());
    assert!(!viewport.follows_tail());
    Ok(())
}

fn full_focus() -> FocusAvailability {
    FocusAvailability {
        composer: true,
        conversation: true,
        changes: true,
        divider: true,
    }
}

#[test]
fn focus_cycles_forward_and_backward_and_skips_closed_panes() -> TestResult {
    let mut state = FocusState::new();
    for expected in [
        Focus::Conversation,
        Focus::Changes,
        Focus::Divider,
        Focus::Composer,
    ] {
        assert_eq!(
            state.cycle(FocusDirection::Forward, full_focus())?,
            expected
        );
    }
    for expected in [
        Focus::Divider,
        Focus::Changes,
        Focus::Conversation,
        Focus::Composer,
    ] {
        assert_eq!(
            state.cycle(FocusDirection::Backward, full_focus())?,
            expected
        );
    }
    let closed = FocusAvailability {
        changes: false,
        divider: false,
        ..full_focus()
    };
    assert_eq!(
        state.cycle(FocusDirection::Forward, closed)?,
        Focus::Conversation
    );
    assert_eq!(
        state.cycle(FocusDirection::Forward, closed)?,
        Focus::Composer
    );
    assert_eq!(
        state.cycle(FocusDirection::Backward, closed)?,
        Focus::Conversation
    );
    let prior = state;
    assert_eq!(
        state.focus(Focus::Changes, closed),
        Err(FocusError::Unavailable {
            target: Focus::Changes
        })
    );
    assert_eq!(state, prior);
    Ok(())
}

#[test]
fn hidden_geometry_restores_requested_focus_and_zero_geometry_has_no_invented_target() -> TestResult
{
    let mut state = FocusState::new();
    state.focus(Focus::Changes, full_focus())?;
    let narrow = FocusAvailability {
        changes: false,
        divider: false,
        ..full_focus()
    };
    assert_eq!(state.visible(narrow)?, Focus::Conversation);
    assert_eq!(state.requested(), Focus::Changes);
    assert_eq!(state.visible(full_focus())?, Focus::Changes);
    state.focus(Focus::Divider, full_focus())?;
    assert_eq!(state.visible(narrow)?, Focus::Conversation);
    assert_eq!(state.visible(full_focus())?, Focus::Divider);
    let zero = FocusAvailability {
        composer: false,
        conversation: false,
        changes: false,
        divider: false,
    };
    let prior = state;
    assert_eq!(state.visible(zero), Err(FocusError::NoVisiblePane));
    assert_eq!(
        state.cycle(FocusDirection::Forward, zero),
        Err(FocusError::NoVisiblePane)
    );
    assert_eq!(state, prior);
    assert_eq!(
        state.cycle(FocusDirection::Forward, narrow)?,
        Focus::Composer
    );
    assert_eq!(state.visible(full_focus())?, Focus::Composer);
    Ok(())
}
