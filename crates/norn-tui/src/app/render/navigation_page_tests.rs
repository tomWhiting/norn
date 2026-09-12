//! Deferred scrolling follows real asynchronous pages and explicit interaction barriers.

use super::*;
use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use termina::event::{KeyCode, KeyEvent, Modifiers};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture() -> TestResult<(AppState, Arc<EventStore>)> {
    let store = Arc::new(EventStore::new());
    for number in 0..65 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("history message {number}"),
        })?;
    }
    let source = store.bind_view_source(
        &SessionBinding::ephemeral_root(),
        uuid::Uuid::new_v4(),
        None,
    )?;
    let mut state = AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.input_editor.paste_cells("draft to keep")?;
    state
        .transcript
        .accept_history(&store.history_page(&state.transcript.initial_history()?)?)?;
    super::super::prepare(&mut state, 80, 14)?;
    Ok((state, store))
}

fn scroll_to_boundary(state: &mut AppState) -> TestResult {
    queue(state, true, 10_000)?;
    super::super::prepare(state, 80, 14)?;
    assert!(state.screen.request_older);
    assert!(
        state
            .screen
            .navigation
            .as_ref()
            .is_some_and(|plan| plan.waiting.is_some())
    );
    Ok(())
}

async fn next_page(
    state: &mut AppState,
    store: &Arc<EventStore>,
) -> TestResult<crate::app::view_actions::reading::HistoryResult> {
    super::super::load_visible(state, store)?;
    assert_eq!(state.transcript.history_tasks.len(), 1);
    super::super::load_visible(state, store)?;
    assert_eq!(
        state.transcript.history_tasks.len(),
        1,
        "requests must be coalesced"
    );
    state
        .transcript
        .history_tasks
        .join_next()
        .await
        .ok_or_else(|| "expected history job".into())
}

#[tokio::test]
async fn one_scroll_crosses_three_pages_without_repeated_motion_or_losing_draft() -> TestResult {
    let (mut state, store) = fixture()?;
    scroll_to_boundary(&mut state)?;
    for _ in 0..3 {
        let anchor = state.screen.viewport.anchor().cloned();
        let remaining = state
            .screen
            .navigation
            .as_ref()
            .ok_or("missing deferred motion")?
            .motions[0]
            .rows;
        for _ in 0..3 {
            super::super::prepare(&mut state, 80, 14)?;
        }
        assert_eq!(state.screen.viewport.anchor(), anchor.as_ref());
        assert_eq!(
            state
                .screen
                .navigation
                .as_ref()
                .ok_or("lost waiting motion")?
                .motions[0]
                .rows,
            remaining
        );
        assert!(
            state.transcript.history_tasks.is_empty(),
            "paint cannot start a read"
        );
        let result = next_page(&mut state, &store).await?;
        crate::app::view_actions::reading::finish_history(&mut state, result)?;
        super::super::prepare(&mut state, 80, 14)?;
        assert_ne!(state.screen.viewport.anchor(), anchor.as_ref());
        assert_eq!(state.input_editor.text(), "draft to keep");
    }
    assert!(!state.transcript.has_older);
    assert!(state.screen.navigation.is_none());
    assert!(!state.screen.request_older);
    let first = state
        .transcript
        .projection
        .items()
        .next()
        .ok_or("no first history item")?;
    assert_eq!(
        state.screen.viewport.anchor().map(|anchor| &anchor.item),
        Some(&first.id)
    );
    super::super::load_visible(&mut state, &store)?;
    assert!(state.transcript.history_tasks.is_empty());
    Ok(())
}

#[tokio::test]
async fn reverse_scroll_retires_remainder_before_a_late_page() -> TestResult {
    let (mut state, store) = fixture()?;
    scroll_to_boundary(&mut state)?;
    let result = next_page(&mut state, &store).await?;
    queue(&mut state, false, 1)?;
    super::super::prepare(&mut state, 80, 14)?;
    let anchor = state.screen.viewport.anchor().cloned();
    assert!(state.screen.navigation.is_none());
    crate::app::view_actions::reading::finish_history(&mut state, result)?;
    super::super::prepare(&mut state, 80, 14)?;
    assert_eq!(state.screen.viewport.anchor(), anchor.as_ref());
    assert!(!state.screen.request_older);
    Ok(())
}

#[test]
fn reversal_in_one_input_batch_does_not_wait_for_older_history() -> TestResult {
    let (mut state, _) = fixture()?;
    queue(&mut state, true, 10_000)?;
    queue(&mut state, false, 1)?;
    super::super::prepare(&mut state, 80, 14)?;
    assert!(state.screen.navigation.is_none());
    assert!(!state.screen.request_older);
    Ok(())
}

#[tokio::test]
async fn explicit_barriers_prevent_late_page_motion() -> TestResult {
    for barrier in ["latest", "resize", "select", "expand", "pane", "source"] {
        let (mut state, store) = fixture()?;
        scroll_to_boundary(&mut state)?;
        let result = next_page(&mut state, &store).await?;
        let (columns, rows) = if barrier == "resize" {
            (70, 16)
        } else {
            (80, 14)
        };
        match barrier {
            "latest" => crate::app::view_actions::latest::follow_latest(&mut state),
            "resize" => {
                super::super::sync_input_area(&mut state, columns, rows)?;
            }
            "source" => {
                let other = EventStore::new();
                let source = other.bind_view_source(
                    &SessionBinding::ephemeral_root(),
                    uuid::Uuid::new_v4(),
                    None,
                )?;
                state.screen.replace_source(&source);
                state.transcript = crate::app::transcript::Transcript::new(source);
            }
            "pane" => {
                crate::app::view_actions::command("pane diff", &mut state)?;
            }
            _ => {
                state.screen.focus.focus(
                    crate::app::focus::Focus::Conversation,
                    state.screen.availability(),
                )?;
                assert!(crate::app::view_actions::key(
                    KeyEvent::new(
                        if barrier == "select" {
                            KeyCode::Up
                        } else {
                            KeyCode::Enter
                        },
                        Modifiers::NONE
                    ),
                    &mut state
                ));
                assert!(state.screen.viewport.selected().is_some());
            }
        }
        super::super::prepare(&mut state, columns, rows)?;
        let anchor = state.screen.viewport.anchor().cloned();
        assert!(state.screen.navigation.is_none(), "barrier {barrier}");
        crate::app::view_actions::reading::finish_history(&mut state, result)?;
        super::super::prepare(&mut state, columns, rows)?;
        assert_eq!(
            state.screen.viewport.anchor(),
            anchor.as_ref(),
            "barrier {barrier}"
        );
        assert_eq!(state.input_editor.text(), "draft to keep");
    }
    Ok(())
}

#[tokio::test]
async fn failed_page_retires_motion_and_does_not_retry_automatically() -> TestResult {
    let (mut state, store) = fixture()?;
    scroll_to_boundary(&mut state)?;
    let (request, _) = next_page(&mut state, &store).await??;
    crate::app::view_actions::reading::finish_history(
        &mut state,
        Ok((
            request,
            Err(TuiError::InvalidViewDemand {
                name: "fixture history read",
                value: 0,
            }),
        )),
    )?;
    super::super::prepare(&mut state, 80, 14)?;
    super::super::load_visible(&mut state, &store)?;
    assert!(state.screen.navigation.is_none());
    assert!(!state.screen.request_older);
    assert!(state.transcript.history_tasks.is_empty());
    assert!(
        state
            .transcript
            .projection
            .items()
            .any(|item| matches!(item.kind, ViewItemKind::Unavailable))
    );
    Ok(())
}

#[test]
fn additional_backward_input_accumulates_while_the_page_is_pending() -> TestResult {
    let (mut state, _) = fixture()?;
    scroll_to_boundary(&mut state)?;
    let remaining = state
        .screen
        .navigation
        .as_ref()
        .ok_or("no initial motion")?
        .motions[0]
        .rows;
    queue(&mut state, true, 7)?;
    super::super::prepare(&mut state, 80, 14)?;
    let plan = state
        .screen
        .navigation
        .as_ref()
        .ok_or("lost accumulated motion")?;
    assert!(plan.waiting.is_some());
    assert_eq!(plan.motions.len(), 1);
    assert_eq!(plan.motions[0].rows, remaining + 7);
    Ok(())
}

#[tokio::test]
async fn nonprogressing_page_retires_motion_without_an_automatic_read_loop() -> TestResult {
    let (mut state, store) = fixture()?;
    scroll_to_boundary(&mut state)?;
    let (request, page) = next_page(&mut state, &store).await??;
    let mut page = page?;
    page.records.clear();
    crate::app::view_actions::reading::finish_history(&mut state, Ok((request, Ok(page))))?;
    super::super::prepare(&mut state, 80, 14)?;
    super::super::load_visible(&mut state, &store)?;
    assert!(state.screen.navigation.is_none());
    assert!(!state.screen.request_older);
    assert!(state.transcript.history_tasks.is_empty());
    Ok(())
}

#[test]
fn pointer_motion_without_a_button_does_not_cancel_waiting_scroll() -> TestResult {
    let (mut state, _) = fixture()?;
    scroll_to_boundary(&mut state)?;
    let anchor = state.screen.viewport.anchor().cloned();
    assert!(!crate::app::view_actions::mouse(
        termina::event::MouseEvent {
            kind: termina::event::MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: Modifiers::NONE,
        },
        &mut state
    ));
    assert!(
        state
            .screen
            .navigation
            .as_ref()
            .is_some_and(|plan| plan.waiting.is_some())
    );
    assert_eq!(state.screen.viewport.anchor(), anchor.as_ref());
    Ok(())
}
