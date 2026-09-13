//! Actual source-bound conversation rendering stays independent from the main runtime and draft.

use std::sync::Arc;

use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use uuid::Uuid;

use super::*;
use crate::app::render::{ScreenState, navigation, transcript};
use crate::app::selection::Selection;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::render::frame::Frame;
use crate::render::layout::{Layout, Rect, UpperLayout, UpperPane};
use crate::terminal::caps::TerminalCaps;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn loaded(text: &str, parent: Option<Uuid>) -> TestResult<(Transcript, ScreenState)> {
    let store = Arc::new(EventStore::new());
    store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), parent)?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: text.to_owned(),
    })?;
    let reader = store.history_reader()?;
    let mut transcript = Transcript::new(reader.source().clone());
    let page = reader.history_page(&transcript.initial_history()?)?;
    assert!(transcript.accept_history(&page)?);
    for record in &page.records {
        for item in record.items() {
            for reference in &item.bodies {
                let demand = transcript
                    .demand_body(&item.id, reference, false)?
                    .ok_or("expected approved body demand")?;
                let body = reader.read_body(&demand.read)?;
                assert!(transcript.accept_body(&demand, body.into())?);
            }
        }
    }
    Ok((transcript, ScreenState::new(reader.source().clone())))
}

fn paint(view: &mut ConversationView<'_>) -> TestResult<Frame> {
    let area = Rect {
        column: 0,
        row: 0,
        width: 40,
        height: 4,
    };
    let layout = Layout::Ready {
        upper: UpperLayout::Single {
            pane: UpperPane::Conversation,
            area,
        },
        composer: Rect {
            row: 4,
            height: 3,
            ..area
        },
    };
    view.layout = layout;
    view.screen.visible.clear();
    view.screen.hit_rows.clear();
    view.screen.demands.clear();
    let mut frame = Frame {
        layout,
        rows: Vec::new(),
        composer: None,
        cursor: None,
    };
    transcript::paint(view, &mut frame, area, None)?;
    Ok(frame)
}

fn displayed(frame: &Frame) -> TestResult<String> {
    let mut text = String::new();
    for row in &frame.rows {
        text.push_str(
            row.text
                .styled
                .text()
                .get(row.geometry.bytes())
                .ok_or("painted fixture row must be within its text")?,
        );
        text.push('\n');
    }
    Ok(text)
}

#[test]
fn conversation_view_renders_and_scrolls_child_without_mutating_root_or_draft() -> TestResult {
    let (root_transcript, root_screen) = loaded("Root narrative", None)?;
    let mut root = AppState::new(
        TerminalCaps::baseline(),
        InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        root_transcript.projection.source().clone(),
        StatusBar::default(),
    );
    root.transcript = root_transcript;
    root.screen = root_screen;
    root.input_editor.paste_cells("next main draft: é🙂")?;
    let draft = root.input_editor.snapshot()?;
    let root_frame = paint(&mut ConversationView::root(&mut root)?)?;
    let root_viewport = root.screen.conversation.viewport.clone();
    let root_revision = root.transcript.projection.revision();
    let (mut child, mut child_screen) = loaded(
        "Child first\nChild second\nChild third\nChild fourth\nChild fifth\nChild last",
        Some(root.tab_state.root_id()),
    )?;
    let mut view = ConversationView::new(
        &mut child,
        &mut child_screen.conversation,
        child_screen.layout,
        root.display_toggles,
    )?;
    let tail = paint(&mut view)?;
    assert!(displayed(&tail)?.contains("Child last"));
    assert!(!displayed(&tail)?.contains("Root narrative"));
    navigation::queue_view(&mut view, true, 2)?;
    let earlier = paint(&mut view)?;
    assert_ne!(displayed(&earlier)?, displayed(&tail)?);
    assert!(!view.screen.viewport.follows_tail());
    assert!(root.read_tasks.history.is_empty());
    assert!(root.read_tasks.bodies.is_empty());
    assert_eq!(root.screen.conversation.viewport, root_viewport);
    assert_eq!(root.transcript.projection.revision(), root_revision);
    root.input_editor.validate_snapshot(&draft)?;
    let root_again = paint(&mut ConversationView::root(&mut root)?)?;
    assert_eq!(displayed(&root_again)?, displayed(&root_frame)?);
    Ok(())
}

#[test]
fn conversation_view_refuses_mismatched_sources_without_changing_either_owner() -> TestResult {
    let (mut first, mut first_screen) = loaded("first private message", None)?;
    let (second, mut second_screen) = loaded("second private message", None)?;
    let viewport = second_screen.conversation.viewport.clone();
    let revision = first.projection.revision();
    assert!(
        ConversationView::new(
            &mut first,
            &mut second_screen.conversation,
            second_screen.layout,
            DisplayToggles::new()
        )
        .is_err()
    );
    assert_eq!(second_screen.conversation.viewport, viewport);
    assert_eq!(first.projection.revision(), revision);
    let frame = paint(&mut ConversationView::new(
        &mut first,
        &mut first_screen.conversation,
        first_screen.layout,
        DisplayToggles::new(),
    )?)?;
    assert!(displayed(&frame)?.contains("first private message"));
    assert!(!displayed(&frame)?.contains("second private message"));
    assert_ne!(first.projection.source(), second.projection.source());
    Ok(())
}

#[test]
fn conversation_view_selection_reads_only_its_exact_body_owner() -> TestResult {
    let (root, root_screen) = loaded("root private text", None)?;
    let (child, mut child_screen) = loaded("child selected text", None)?;
    let item = child
        .projection
        .items()
        .next()
        .ok_or("child item missing")?;
    let reference = item.bodies.first().ok_or("child body missing")?;
    child_screen.conversation.selection = Some(Selection::from_original(
        child.projection.source(),
        original_for(&child, &child_screen.conversation, &item.id, reference)?,
        0.."child selected text".len(),
    )?);
    child_screen.conversation.selection_item = Some(item.id.clone());
    assert_eq!(
        selected_text(&child, &child_screen.conversation)?,
        "child selected text"
    );
    assert!(selected_text(&root, &child_screen.conversation).is_err());
    assert!(original_for(&root, &root_screen.conversation, &item.id, reference).is_err());
    Ok(())
}
