//! Explicit descendant conversation inspection; root execution and its composer remain fixed.

use std::collections::HashMap;
use std::sync::Arc;

use norn::session::action_log_tree::ActionLogTree;
use norn::session::store::SessionHistoryReader;
use norn::session_view::ViewSource;
use uuid::Uuid;

use super::conversation_view::ConversationView;
use super::render::{ConversationScreen, interaction};
use super::state::AppState;
use super::transcript::Transcript;
use crate::TuiError;

pub(in crate::app) struct ChildConversation {
    pub transcript: Transcript,
    pub screen: ConversationScreen,
    refresh_requested: bool,
}

/// Only explicitly opened children are retained. No child provider or writable store is owned here.
#[derive(Default)]
pub(in crate::app) struct AgentConversations {
    pub tree: Option<Arc<ActionLogTree>>,
    pub children: HashMap<Uuid, ChildConversation>,
    pub selected: Option<Uuid>,
    requested: Option<Uuid>,
    pub opening: tokio::task::JoinSet<Opened>,
}

pub(in crate::app) type Opened = (Uuid, ViewSource, Result<SessionHistoryReader, TuiError>);

pub(in crate::app) fn request(state: &mut AppState, target: Uuid) -> Result<(), TuiError> {
    if target == state.tab_state.root_id() {
        state.agent_conversations.requested = None;
        select(state, None);
        return Ok(());
    }
    let tree = state.agent_conversations.tree.clone().ok_or_else(|| {
        interaction(std::io::Error::other(
            "this runtime has no agent conversation directory",
        ))
    })?;
    if state.agent_conversations.requested == Some(target) {
        return Ok(());
    }
    // One pending admission prevents repeated clicks from growing a worker queue.
    if !state.agent_conversations.opening.is_empty() {
        return Err(interaction(std::io::Error::other(
            "another agent conversation is still opening; return to main or wait for its result",
        )));
    }
    let root = state.transcript.projection.source().clone();
    let caller = state.tab_state.root_id();
    state.agent_conversations.requested = Some(target);
    state.agent_conversations.opening.spawn(async move {
        let result = tokio::task::spawn_blocking(move || tree.history_reader(caller, target))
            .await
            .map_err(|source| TuiError::ViewTask {
                operation: "open agent conversation",
                source,
            })
            .and_then(|result| result.map_err(interaction));
        (target, root, result)
    });
    state.screen.conversation.feedback = Some(format!("Opening agent {target} conversation"));
    state.screen.dirty = true;
    Ok(())
}

pub(in crate::app) fn finish(
    state: &mut AppState,
    result: Result<Opened, tokio::task::JoinError>,
) -> Result<(), TuiError> {
    let (target, root, result) = result.map_err(|source| TuiError::ViewTask {
        operation: "agent conversation completion",
        source,
    })?;
    if state.agent_conversations.requested != Some(target)
        || &root != state.transcript.projection.source()
    {
        state.agent_conversations.requested = None;
        return Ok(());
    }
    state.agent_conversations.requested = None;
    let reader = match result {
        Ok(reader) => reader,
        Err(error) => {
            state.screen.conversation.feedback =
                Some(format!("Cannot open agent {target}: {error}"));
            state.screen.dirty = true;
            return Ok(());
        }
    };
    let source = reader.source().clone();
    let replace = state
        .agent_conversations
        .children
        .get(&target)
        .is_none_or(|child| child.transcript.projection.source() != &source);
    if replace {
        let mut transcript = Transcript::new(source.clone());
        transcript.config = state.transcript.config.clone();
        transcript.attach_history_reader(reader)?;
        state.agent_conversations.children.insert(
            target,
            ChildConversation {
                transcript,
                screen: ConversationScreen::new(source),
                refresh_requested: false,
            },
        );
    }
    if let Some(child) = state.agent_conversations.children.get_mut(&target) {
        child.transcript.request_latest();
        child.screen.allow_body_load = true;
    }
    state.screen.conversation.feedback = None;
    select(state, Some(target));
    Ok(())
}

fn select(state: &mut AppState, selected: Option<Uuid>) {
    state.agent_conversations.selected = selected;
    // Pixel hit authority is revoked, but the actual terminal frame baseline is preserved.
    super::display_selection::revoke_pointer_mapping(&mut state.screen);
    state.screen.display_selection = None;
    state.screen.dragging_selection = false;
    state.screen.latest_hit = None;
    state.screen.prepared_latest = None;
    state.screen.dirty = true;
}

pub(in crate::app) fn selected(state: &AppState) -> Option<&ChildConversation> {
    state
        .agent_conversations
        .selected
        .and_then(|id| state.agent_conversations.children.get(&id))
}

pub(in crate::app) fn selected_mut(state: &mut AppState) -> Option<&mut ChildConversation> {
    state
        .agent_conversations
        .selected
        .and_then(|id| state.agent_conversations.children.get_mut(&id))
}

pub(in crate::app) fn paint(
    state: &mut AppState,
    frame: &mut crate::render::frame::Frame,
    area: crate::render::layout::Rect,
) -> Result<bool, TuiError> {
    let layout = state.screen.layout;
    let toggles = state.display_toggles;
    let Some(child) = selected_mut(state) else {
        return Ok(false);
    };
    child.screen.visible.clear();
    child.screen.hit_rows.clear();
    child.screen.demands.clear();
    let mut view =
        ConversationView::new(&mut child.transcript, &mut child.screen, layout, toggles)?;
    super::render::transcript::paint(&mut view, frame, area, None)?;
    Ok(true)
}

pub(in crate::app) fn load(state: &mut AppState) -> Result<bool, TuiError> {
    let Some(id) = state.agent_conversations.selected else {
        return Ok(false);
    };
    let child = state
        .agent_conversations
        .children
        .get_mut(&id)
        .ok_or_else(|| {
            interaction(std::io::Error::other(format!(
                "selected agent {id} has no opened conversation"
            )))
        })?;
    if child.refresh_requested
        && child.screen.viewport.follows_tail()
        && !child.transcript.latest_pending()
    {
        child.refresh_requested = false;
        child.transcript.request_latest();
    }
    child.transcript.load_latest(&mut state.read_tasks)?;
    if child.screen.request_older
        && (!child.transcript.has_older || child.transcript.load_older(&mut state.read_tasks)?)
    {
        child.screen.request_older = false;
    }
    if std::mem::take(&mut child.screen.request_more) {
        let item = child
            .screen
            .viewport
            .selected()
            .or_else(|| child.screen.visible.first().map(|anchor| &anchor.item));
        if let Some(item) = item
            .and_then(|id| child.transcript.projection.item(id))
            .cloned()
        {
            for body in item.bodies {
                child
                    .transcript
                    .load_body(&mut state.read_tasks, &item.id, &body, true)?;
            }
        }
    }
    if !child.screen.allow_body_load {
        return Ok(true);
    }
    child.screen.allow_body_load = false;
    let demands = std::mem::take(&mut child.screen.demands);
    let pinned = demands.iter().map(|(_, body)| body.clone()).collect();
    for (item, body) in demands {
        child
            .transcript
            .load_body(&mut state.read_tasks, &item, &body, false)?;
    }
    child.transcript.retain_bodies(&pinned);
    child.screen.retain_display(&pinned);
    Ok(true)
}

pub(in crate::app) fn scroll(
    state: &mut AppState,
    backwards: bool,
    rows: usize,
) -> Result<bool, TuiError> {
    let layout = state.screen.layout;
    let toggles = state.display_toggles;
    let Some(child) = selected_mut(state) else {
        return Ok(false);
    };
    let mut view =
        ConversationView::new(&mut child.transcript, &mut child.screen, layout, toggles)?;
    super::render::navigation::queue_view(&mut view, backwards, rows)?;
    child.screen.allow_body_load = true;
    state.screen.dirty = true;
    Ok(true)
}

/// A committed child event requests a coalesced tail read; no provider event is forged.
pub(in crate::app) fn changed(state: &mut AppState, agent: Uuid) {
    if state.agent_conversations.selected != Some(agent) {
        return;
    }
    if let Some(child) = state.agent_conversations.children.get_mut(&agent) {
        // A read captures a finite frontier. A later event must survive that
        // read's completion even when no further producer event will arrive.
        child.refresh_requested = true;
        child.screen.allow_body_load = true;
    }
}

pub(in crate::app) fn history_result(
    state: &mut AppState,
    result: super::read_tasks::HistoryResult,
) -> Result<Option<super::read_tasks::HistoryResult>, TuiError> {
    let source = match &result {
        Ok((request, _)) => &request.source,
        Err(_) => return Ok(Some(result)),
    };
    if source == state.transcript.projection.source() {
        return Ok(Some(result));
    }
    if let Some(child) = state.agent_conversations.children.get_mut(&source.agent_id)
        && child.transcript.projection.source() == source
    {
        child.transcript.finish_history(result)?;
        child.screen.allow_body_load = true;
        state.screen.dirty = true;
    }
    Ok(None)
}

pub(in crate::app) fn body_result(
    state: &mut AppState,
    result: super::read_tasks::BodyResult,
) -> Result<Option<super::read_tasks::BodyResult>, TuiError> {
    let Ok((source, _, _)) = &result else {
        return Ok(Some(result));
    };
    if source == state.transcript.projection.source() {
        return Ok(Some(result));
    }
    if let Some(child) = state.agent_conversations.children.get_mut(&source.agent_id)
        && child.transcript.projection.source() == source
    {
        child.transcript.finish_body(result)?;
        child.screen.allow_body_load = true;
        state.screen.dirty = true;
    }
    Ok(None)
}

/// Inspection controls never act on the hidden root conversation.
pub(in crate::app) fn command(state: &mut AppState, text: &str) -> Result<bool, TuiError> {
    if selected(state).is_none() {
        return Ok(false);
    }
    let text = text.trim();
    if matches!(text, "up" | "down") {
        return scroll(state, text == "up", 1);
    }
    if text.starts_with("pane ")
        || text.starts_with("focus ")
        || text.starts_with("split ")
        || text.starts_with("composer ")
        || text.starts_with("keys")
        || text.starts_with("preferences")
        || text.starts_with("clipboard ")
        || matches!(text, "status" | "help")
    {
        return Ok(false);
    }
    let child = selected_mut(state)
        .ok_or_else(|| interaction(std::io::Error::other("selected conversation disappeared")))?;
    match text {
        "follow" => {
            child.screen.viewport.follow_tail();
            child.refresh_requested = false;
            child.transcript.request_latest();
        }
        "pin" => {
            child.screen.viewport.pin();
            child.transcript.cancel_latest();
        }
        "older" => {
            child.transcript.cancel_latest();
            child.screen.viewport.pin();
            child.screen.request_older = true;
        }
        "more" => child.screen.request_more = true,
        "toggle" | "expand" | "collapse" => {
            let item = child
                .screen
                .viewport
                .selected()
                .cloned()
                .or_else(|| {
                    child
                        .screen
                        .visible
                        .first()
                        .map(|anchor| anchor.item.clone())
                })
                .ok_or_else(|| {
                    interaction(std::io::Error::other("no visible child item to expand"))
                })?;
            let old = child
                .screen
                .tool_overrides
                .get(&item)
                .copied()
                .unwrap_or(child.transcript.config.expanded_tools);
            child.screen.tool_overrides.insert(
                item,
                match text {
                    "expand" => true,
                    "collapse" => false,
                    _ => !old,
                },
            );
        }
        "compact" | "detailed" => {
            child.transcript.config.expanded_tools = text == "detailed";
        }
        _ => {
            return Err(interaction(std::io::Error::other(
                "This inspection control is not connected yet. Use /view agent main to return to the main conversation.",
            )));
        }
    }
    child.screen.allow_body_load = true;
    state.screen.dirty = true;
    Ok(true)
}

#[cfg(test)]
#[path = "agent_conversations_tests.rs"]
mod tests;
