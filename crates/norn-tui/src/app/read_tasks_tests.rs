//! Read supervision survives source rotation and routes delayed work before changing UI state.

use super::*;
use crate::app::state::AppState;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;
use norn::session::events::{EventBase, SessionEvent};
use norn::session::{EventStore, SessionBinding};
use std::num::NonZeroUsize;
use std::sync::Arc;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture(label: &str, agent: Uuid) -> TestResult<AppState> {
    let store = Arc::new(EventStore::new());
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    for index in 0..5 {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("{label} {index}"),
        })?;
    }
    let mut state = AppState::new(
        TerminalCaps::baseline(),
        InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        StatusBar::default(),
    );
    state
        .transcript
        .attach_history_reader(store.history_reader()?)?;
    state
        .transcript
        .config
        .set_history_demand(NonZeroUsize::new(2).ok_or("zero fixture demand")?);
    state
        .transcript
        .accept_history(&store.history_page(&state.transcript.initial_history()?)?)?;
    Ok(state)
}

fn first_body(
    state: &AppState,
) -> TestResult<(norn::session_view::ItemId, norn::session_view::BodyRef)> {
    let item = state
        .transcript
        .projection
        .items()
        .next()
        .ok_or("missing item")?;
    Ok((
        item.id.clone(),
        item.bodies.first().ok_or("missing body")?.clone(),
    ))
}

#[tokio::test]
async fn admitted_reads_survive_root_rotation_without_touching_the_new_draft_or_view() -> TestResult
{
    let id = Uuid::new_v4();
    let mut state = fixture("old", id)?;
    let (item, reference) = first_body(&state)?;
    assert!(state.transcript.load_older(&mut state.read_tasks)?);
    state
        .transcript
        .load_body(&mut state.read_tasks, &item, &reference, false)?;
    let mut replacement = fixture("new", id)?;
    std::mem::swap(&mut state.transcript, &mut replacement.transcript);
    state
        .screen
        .replace_source(state.transcript.projection.source());
    state
        .input_editor
        .paste_cells("new draft survives old completions")?;
    state.screen.feedback = Some("new view feedback".to_owned());
    state.screen.allow_body_load = false;
    state.screen.dirty = false;
    let revision = state.transcript.projection.revision();
    let viewport = state.screen.viewport.clone();
    let history = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("old history lost at rotation")?;
    crate::app::view_actions::reading::finish_history(&mut state, history)?;
    let body = state
        .read_tasks
        .bodies
        .join_next()
        .await
        .ok_or("old body lost at rotation")?;
    finish_body(&mut state, body)?;
    assert_eq!(state.transcript.projection.revision(), revision);
    assert_eq!(state.screen.viewport, viewport);
    assert_eq!(state.screen.feedback.as_deref(), Some("new view feedback"));
    assert!(!state.screen.dirty);
    assert!(!state.screen.allow_body_load);
    assert_eq!(
        state.input_editor.text(),
        "new draft survives old completions"
    );
    assert!(state.read_tasks.history.is_empty());
    assert!(state.read_tasks.bodies.is_empty());
    Ok(())
}

#[tokio::test]
async fn late_history_failure_cannot_clear_a_new_sources_search_or_latest_intent() -> TestResult {
    let id = Uuid::new_v4();
    let old = fixture("old", id)?;
    let request = old.transcript.older_history()?;
    let mut state = fixture("new", id)?;
    assert!(matches!(
        crate::app::view_actions::command("search older new", &mut state)?,
        crate::app::slash::LocalCommandOutcome::Accepted
    ));
    state.transcript.request_latest();
    state.screen.feedback = Some("new search remains selected".to_owned());
    state.screen.dirty = false;
    let (release, held) = tokio::sync::oneshot::channel::<()>();
    state.read_tasks.history.spawn(async move {
        let result = match held.await {
            Ok(()) => Err(crate::app::render::interaction(std::io::Error::other(
                "retired source read failed",
            ))),
            Err(error) => Err(crate::app::render::interaction(error)),
        };
        (request, result)
    });
    release.send(()).map_err(|()| "read barrier closed")?;
    let result = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("missing held read")?;
    crate::app::view_actions::reading::finish_history(&mut state, result)?;
    assert!(state.transcript.latest_pending());
    assert_eq!(
        state.screen.feedback.as_deref(),
        Some("new search remains selected")
    );
    assert!(!state.screen.dirty);
    let mut pinned = std::collections::HashSet::new();
    crate::app::view_actions::reading::load_requests(&mut state, &mut pinned)?;
    let result = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("new search lost after retired failure")?;
    crate::app::view_actions::reading::finish_history(&mut state, result)?;
    crate::app::view_actions::reading::load_requests(&mut state, &mut pinned)?;
    while let Some(result) = state.read_tasks.bodies.join_next().await {
        finish_body(&mut state, result)?;
    }
    crate::app::view_actions::reading::load_requests(&mut state, &mut pinned)?;
    assert_eq!(crate::app::view_actions::selected_text(&state)?, "new");
    assert!(state.transcript.load_latest(&mut state.read_tasks)?);
    let result = state
        .read_tasks
        .history
        .join_next()
        .await
        .ok_or("new read missing")?;
    crate::app::view_actions::reading::finish_history(&mut state, result)?;
    assert!(!state.transcript.latest_pending());
    Ok(())
}
