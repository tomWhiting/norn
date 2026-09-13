//! Real tree/source selection, pending-read ownership and main draft preservation.

use super::*;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;
use norn::session::action_log::ActionLog;
use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn fixture() -> TestResult<(AppState, Uuid, Arc<EventStore>)> {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let tree = Arc::new(ActionLogTree::new(root));
    let root_store = Arc::new(EventStore::new());
    let source = root_store.bind_view_source(&SessionBinding::ephemeral_root(), root, None)?;
    tree.register(root, None, Arc::new(ActionLog::new(root_store)));
    let child_store = Arc::new(EventStore::new());
    child_store.bind_view_source(&SessionBinding::ephemeral_root(), child, Some(root))?;
    tree.register(
        child,
        Some(root),
        Arc::new(ActionLog::new(Arc::clone(&child_store))),
    );
    child_store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "actual child conversation".to_owned(),
    })?;
    let mut state = AppState::new(
        TerminalCaps::baseline(),
        InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        StatusBar::default(),
    );
    state.agent_conversations.tree = Some(tree);
    state.input_editor.paste_cells("main draft retained")?;
    Ok((state, child, child_store))
}
async fn open(state: &mut AppState, target: Uuid) -> TestResult {
    request(state, target)?;
    let result = state
        .agent_conversations
        .opening
        .join_next()
        .await
        .ok_or("missing open job")?;
    finish(state, result)?;
    Ok(())
}
#[tokio::test]
async fn selected_child_reads_actual_history_without_changing_main_or_draft() -> TestResult {
    let (mut state, child, store) = fixture()?;
    let root_source = state.transcript.projection.source().clone();
    let viewport = state.screen.conversation.viewport.clone();
    open(&mut state, child).await?;
    assert_eq!(state.agent_conversations.selected, Some(child));
    assert!(load(&mut state)?);
    let page = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing initial history")?;
    crate::app::view_actions::reading::finish_history(&mut state, page)?;
    assert_eq!(
        selected(&state)
            .ok_or("no child")?
            .transcript
            .observed_events,
        1
    );
    assert_eq!(state.transcript.projection.source(), &root_source);
    assert_eq!(state.screen.conversation.viewport, viewport);
    assert_eq!(state.input_editor.text(), "main draft retained");
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "child later message".to_owned(),
    })?;
    changed(&mut state, child);
    load(&mut state)?;
    let page = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing tail history")?;
    crate::app::view_actions::reading::finish_history(&mut state, page)?;
    assert_eq!(
        selected(&state)
            .ok_or("no child")?
            .transcript
            .observed_events,
        2
    );
    let root = state.tab_state.root_id();
    request(&mut state, root)?;
    assert!(selected(&state).is_none());
    assert_eq!(state.input_editor.text(), "main draft retained");
    Ok(())
}
#[tokio::test]
async fn return_to_main_cancels_selection_without_abandoning_the_open_job() -> TestResult {
    let (mut state, child, _) = fixture()?;
    request(&mut state, child)?;
    assert_eq!(state.agent_conversations.opening.len(), 1);
    request(&mut state, child)?;
    assert_eq!(state.agent_conversations.opening.len(), 1);
    let root = state.tab_state.root_id();
    request(&mut state, root)?;
    let result = state
        .agent_conversations
        .opening
        .join_next()
        .await
        .ok_or("lost task")?;
    finish(&mut state, result)?;
    assert!(selected(&state).is_none());
    assert!(state.agent_conversations.children.is_empty());
    Ok(())
}
#[tokio::test]
async fn unknown_agent_keeps_current_selection_and_reports_the_named_failure() -> TestResult {
    let (mut state, child, _) = fixture()?;
    open(&mut state, child).await?;
    let missing = Uuid::new_v4();
    open(&mut state, missing).await?;
    assert_eq!(state.agent_conversations.selected, Some(child));
    assert!(
        state
            .screen
            .conversation
            .feedback
            .as_deref()
            .ok_or("missing failure")?
            .contains(&missing.to_string())
    );
    Ok(())
}

#[tokio::test]
async fn pane_click_opens_the_child_and_can_return_to_main_without_touching_draft() -> TestResult {
    use crate::app::agent_pane::AgentHit;
    use crate::render::frame::Frame;
    use crate::render::layout::{Layout, Rect, UpperLayout};
    use termina::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
    let (mut state, child, _) = fixture()?;
    let root = state.tab_state.root_id();
    let pane = Rect {
        column: 40,
        row: 0,
        width: 40,
        height: 10,
    };
    let layout = Layout::Ready {
        upper: UpperLayout::Split {
            conversation: Rect {
                column: 0,
                row: 0,
                width: 39,
                height: 10,
            },
            changes: pane,
            divider: Rect {
                column: 39,
                row: 0,
                width: 1,
                height: 10,
            },
        },
        composer: Rect {
            column: 0,
            row: 10,
            width: 80,
            height: 5,
        },
    };
    state.screen.layout = layout;
    state.screen.changes_open = true;
    state.screen.auxiliary = crate::app::render::AuxiliaryPane::Agents;
    for target in [child, root] {
        state.screen.agent_pane.prepared = vec![AgentHit {
            id: target,
            area: Rect { height: 1, ..pane },
        }];
        let frame = Arc::new(Frame {
            layout,
            rows: Vec::new(),
            composer: None,
            cursor: None,
        });
        crate::app::view_actions::latest::finish_publication(&mut state.screen, frame, Ok(()))?;
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            assert!(crate::app::view_actions::mouse(
                MouseEvent {
                    kind,
                    column: pane.column,
                    row: pane.row,
                    modifiers: Modifiers::NONE,
                },
                &mut state
            ));
        }
        if target == child {
            let result = state
                .agent_conversations
                .opening
                .join_next()
                .await
                .ok_or("click did not open child")?;
            finish(&mut state, result)?;
            assert_eq!(state.agent_conversations.selected, Some(child));
        } else {
            assert_eq!(state.agent_conversations.selected, None);
        }
        assert_eq!(state.input_editor.text(), "main draft retained");
        assert_eq!(state.transcript.projection.source().agent_id, root);
    }
    Ok(())
}

#[tokio::test]
async fn change_during_captured_history_read_is_not_lost_at_completion() -> TestResult {
    let (mut state, child, store) = fixture()?;
    open(&mut state, child).await?;
    load(&mut state)?;
    let captured = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing captured read")?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "arrived after captured frontier".to_owned(),
    })?;
    // Several notifications coalesce while the first captured page is in flight.
    changed(&mut state, child);
    changed(&mut state, child);
    load(&mut state)?;
    assert!(state.read_tasks.history.is_empty());
    crate::app::view_actions::reading::finish_history(&mut state, captured)?;
    assert_eq!(
        selected(&state)
            .ok_or("no selected child")?
            .transcript
            .observed_events,
        1
    );
    load(&mut state)?;
    assert_eq!(
        state.read_tasks.history.len(),
        1,
        "later demand was dropped"
    );
    let tail = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing retained tail read")?;
    crate::app::view_actions::reading::finish_history(&mut state, tail)?;
    assert_eq!(
        selected(&state)
            .ok_or("no selected child")?
            .transcript
            .observed_events,
        2
    );
    load(&mut state)?;
    assert!(
        state.read_tasks.history.is_empty(),
        "refresh became polling"
    );
    assert_eq!(state.input_editor.text(), "main draft retained");
    Ok(())
}

#[tokio::test]
async fn pin_holds_later_changes_until_explicit_follow() -> TestResult {
    let (mut state, child, store) = fixture()?;
    open(&mut state, child).await?;
    load(&mut state)?;
    let first = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing first read")?;
    crate::app::view_actions::reading::finish_history(&mut state, first)?;
    command(&mut state, "pin")?;
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "while pinned".to_owned(),
    })?;
    changed(&mut state, child);
    load(&mut state)?;
    assert!(state.read_tasks.history.is_empty());
    command(&mut state, "follow")?;
    load(&mut state)?;
    assert_eq!(state.read_tasks.history.len(), 1);
    let tail = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing follow read")?;
    crate::app::view_actions::reading::finish_history(&mut state, tail)?;
    assert_eq!(
        selected(&state)
            .ok_or("no child")?
            .transcript
            .observed_events,
        2
    );
    load(&mut state)?;
    assert!(state.read_tasks.history.is_empty());
    Ok(())
}

#[tokio::test]
async fn invalid_or_unconnected_view_commands_are_rejected_without_exiting() -> TestResult {
    let (mut state, child, _) = fixture()?;
    open(&mut state, child).await?;
    assert!(state.screen.conversation.feedback.is_none());
    for command in ["agent invalid-uuid", "not-a-control"] {
        let outcome = crate::app::view_actions::command(command, &mut state)?;
        assert!(matches!(
            outcome,
            crate::app::slash::LocalCommandOutcome::Rejected
        ));
        assert_eq!(state.agent_conversations.selected, Some(child));
        assert_eq!(state.input_editor.text(), "main draft retained");
    }
    Ok(())
}
