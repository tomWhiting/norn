//! Disk-resumed compacted history remains browseable without changing prompt or timeline bytes.

use super::*;
use norn::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use norn::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, SessionBinding, SessionManager,
};
use norn::session_view::ViewItemKind;
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const OLD_RESULT: &str = "old-result-界-retained";
const DESCRIPTION: &str = "Read the original record";
const DRAFT: &str = "draft before browsing\nsecond line stays";

fn seed(store: &EventStore) -> TestResult<Vec<norn::session::events::EventId>> {
    let mut replaced = Vec::new();
    let call = SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: "Earlier work".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "old-read".to_owned(),
            name: "read".to_owned(),
            arguments: json!({"path":"/fixture/never-opened.txt", "tool_use_description":DESCRIPTION}),
            kind: norn::provider::request::ToolCallKind::Function,
            caller: norn::provider::request::ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    };
    let parent = call.base().id.clone();
    replaced.push(store.append(call)?);
    replaced.push(store.append(SessionEvent::ToolResult {
        base: EventBase::new(Some(parent)),
        tool_call_id: "old-read".to_owned(),
        tool_name: "read".to_owned(),
        output: json!({"content":OLD_RESULT}),
        spool_ref: None,
        duration_ms: 7,
    })?);
    // 61 total events put the call at the end of the final one-record
    // older page and its result in the preceding page, exercising late pairing.
    for number in 0..55 {
        replaced.push(store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("historical turn {number}"),
        })?);
    }
    let first_summary = store.append(SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "Earlier work summarized; original records remain available.".to_owned(),
        replaced_event_ids: replaced.clone(),
    })?;
    let bridge = store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "work between compactions".to_owned(),
    })?;
    store.append(SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "A second summary retains references to earlier work.".to_owned(),
        replaced_event_ids: vec![first_summary.clone(), bridge.clone()],
    })?;
    replaced.extend([first_summary, bridge]);
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "newest post-compaction turn".to_owned(),
    })?;
    store.checkpoint()?;
    Ok(replaced)
}

fn text(frame: &Frame) -> String {
    frame
        .rows
        .iter()
        .map(|row| row.text.styled.text()[row.geometry.bytes()].to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

// Advance only actual pending tasks. The finite fixture bound detects an accidental
// demand loop; there is no sleep or simulated body injection.
async fn settle(state: &mut AppState, store: &Arc<EventStore>) -> TestResult<Frame> {
    for _ in 0..100 {
        let frame = prepare(state, 100, 30)?;
        load_visible(state, store)?;
        if state.transcript.history_tasks.is_empty() && state.transcript.body_tasks.is_empty() {
            return Ok(frame);
        }
        while let Some(result) = state.transcript.history_tasks.join_next().await {
            crate::app::view_actions::reading::finish_history(state, result)?;
        }
        while let Some(result) = state.transcript.body_tasks.join_next().await {
            state.transcript.finish_body(result)?;
            state.screen.allow_body_load = true;
            state.screen.dirty = true;
        }
    }
    Err("compacted-history fixture did not settle after 100 task rounds".into())
}

#[tokio::test]
async fn disk_resume_browses_superseded_tools_and_returns_to_latest_without_writes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().to_path_buf();
    let prepared = tokio::task::spawn_blocking(move || -> TestResult<_> {
        let manager = SessionManager::new(&root);
        let session = manager.create(
            CreateSessionOptions {
                model: "fixture-model".to_owned(),
                working_dir: root.to_string_lossy().into_owned(),
                name: Some("compacted history fixture".to_owned()),
            },
            DurabilityPolicy::Flush,
        )?;
        let replaced = seed(&session.store)?;
        let id = session.entry.id.clone();
        let path = root.join(format!("{id}.jsonl"));
        let before = std::fs::read(&path)?;
        let ids = session.store.with_events(|events| {
            events
                .iter()
                .map(|event| event.base().id.clone())
                .collect::<Vec<_>>()
        });
        drop(session);
        let resumed = manager.resume(&id, DurabilityPolicy::Flush)?;
        assert_eq!(resumed.replay.replayed_events, 61);
        let artifacts = norn::session::ReplayArtifacts::from_events(resumed.store.events());
        assert!(
            replaced
                .iter()
                .all(|id| artifacts.superseded_event_ids.contains(id))
        );
        assert_eq!(
            artifacts
                .events
                .iter()
                .map(|event| event.base().id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        let binding = SessionBinding::persistent_root(
            Arc::new(norn::session::SessionBrancher::new(
                manager,
                id,
                DurabilityPolicy::Flush,
            )),
            &resumed.entry,
            &artifacts.events,
        );
        Ok((resumed.store, binding, path, before, ids))
    })
    .await?
    .map_err(|error| error.to_string())?;
    let (store, binding, path, before, ids) = prepared;
    let store = Arc::new(store);
    let source = store.bind_view_source(&binding, uuid::Uuid::new_v4(), None)?;
    let mut state = AppState::new(
        crate::terminal::caps::TerminalCaps::baseline(),
        crate::input::history::InputHistory::in_memory(),
        norn::agent::registry::AgentRegistry::shared(),
        source,
        crate::render::fixed_panel::StatusBar::default(),
    );
    state.input_editor.paste_cells(DRAFT)?;
    state
        .transcript
        .config
        .set_history_demand(NonZeroUsize::new(20).ok_or("fixture page size")?);
    state
        .transcript
        .accept_history(&store.history_page(&state.transcript.initial_history()?)?)?;
    let initial = settle(&mut state, &store).await?;
    assert!(text(&initial).contains("newest post-compaction turn"));
    assert!(state.transcript.has_older);
    assert!(!text(&initial).contains(DESCRIPTION));
    navigation::queue(&mut state, true, 10_000)?;
    settle(&mut state, &store).await?;
    assert!(!state.transcript.has_older);
    assert!(state.screen.navigation.is_none());
    assert_eq!(
        state
            .transcript
            .projection
            .items()
            .filter(|item| matches!(item.kind, ViewItemKind::Tool(_)))
            .count(),
        1,
        "late call/result pairing must not leave duplicate tool rows"
    );
    let tool = state
        .transcript
        .projection
        .items()
        .find(|item| matches!(item.kind, ViewItemKind::Tool(_)))
        .ok_or("old tool missing after paging")?
        .id
        .clone();
    state
        .screen
        .viewport
        .select(tool.clone(), &state.transcript.projection)?;
    state.screen.viewport.scroll_to(
        ViewAnchor {
            item: tool.clone(),
            position: AnchorPosition::Header,
        },
        &state.transcript.projection,
    )?;
    crate::app::view_actions::command("expand", &mut state)?;
    let expanded = settle(&mut state, &store).await?;
    assert!(text(&expanded).contains(DESCRIPTION));
    assert!(text(&expanded).contains(OLD_RESULT), "{}", text(&expanded));
    assert_eq!(state.input_editor.text(), DRAFT);
    let references = state
        .transcript
        .projection
        .item(&tool)
        .ok_or("tool disappeared")?
        .bodies
        .clone();
    state.transcript.retain_bodies(&HashSet::new());
    assert!(
        references
            .iter()
            .all(|body| state.transcript.body(body).is_none())
    );
    state.screen.allow_body_load = true;
    let reloaded = settle(&mut state, &store).await?;
    assert!(text(&reloaded).contains(OLD_RESULT));
    assert!(
        references
            .iter()
            .all(|body| state.transcript.body(body).is_some())
    );
    crate::app::view_actions::command("follow", &mut state)?;
    let latest = settle(&mut state, &store).await?;
    assert!(state.screen.viewport.follows_tail());
    assert!(text(&latest).contains("newest post-compaction turn"));
    assert_eq!(state.input_editor.text(), DRAFT);
    assert_eq!(
        store.with_events(|events| events
            .iter()
            .map(|event| event.base().id.clone())
            .collect::<Vec<_>>()),
        ids
    );
    let after = tokio::task::spawn_blocking(move || std::fs::read(path)).await??;
    assert_eq!(after, before, "browsing must not rewrite persisted history");
    Ok(())
}
