//! Real runner acceptance and local retirement preserve draft and recall ownership.

use std::sync::Arc;

use iridium_editor::CommandArgs;
use iridium_editor::cell_layout::CellWrapParameters;
use iridium_editor::editor::CellInputOptions;
use norn::agent::registry::AgentRegistry;
use norn::agent_loop::LoopContext;
use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::runner::{AgentStepRequest, AgentStepResult, ToolExecutor, run_agent_step};
use norn::provider::agent_event::{AgentEventSender, PublicationResolution};
use norn::provider::events::{ProviderEvent, StopReason};
use norn::provider::mock::MockProvider;
use norn::provider::usage::Usage;
use norn::session::SessionBinding;
use norn::session::events::SessionEvent;
use norn::session::persistence::SessionPersistError;
use norn::session::store::{EventStore, PersistenceSink};
use norn::tool::ToolRegistry;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;
use crate::input::history::InputHistory;
use crate::render::fixed_panel::StatusBar;
use crate::terminal::caps::TerminalCaps;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn state(history: InputHistory, store: &EventStore) -> TestResult<AppState> {
    let registry = AgentRegistry::shared();
    let guard = AgentRegistry::reserve(
        &registry,
        "/root".to_owned(),
        "lead".to_owned(),
        "fixture".to_owned(),
        None,
        norn::agent::child_policy::ChildPolicy {
            messaging: norn::agent::child_policy::MessagingScope::SiblingsAndParent,
            delegation: norn::agent::child_policy::DelegationBudget {
                remaining_depth: 5,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        None,
    )?;
    let agent = guard.id();
    guard.confirm()?;
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), agent, None)?;
    Ok(AppState::new(
        TerminalCaps::baseline(),
        history,
        registry,
        source,
        StatusBar::default(),
    ))
}

fn options() -> CellInputOptions {
    CellInputOptions {
        wrap: CellWrapParameters::new(80, 4),
        visible_rows: 10,
    }
}

fn history_witness(state: &AppState) -> TestResult<serde_json::Value> {
    Ok(serde_json::to_value(
        state.input_editor.kernel().history_snapshot(),
    )?)
}

fn provider() -> MockProvider {
    MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "fixture answer".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            response_id: None,
            usage: Usage {
                input_tokens: 4,
                output_tokens: 2,
                ..Usage::default()
            },
        },
    ]])
}

async fn run(
    store: &EventStore,
    sender: &AgentEventSender,
    provider: &MockProvider,
    text: &str,
) -> Result<AgentStepResult, norn::error::NornError> {
    let executor: Arc<dyn ToolExecutor> = Arc::new(ToolRegistry::new());
    let mut context = LoopContext::new("composer admission fixture");
    run_agent_step(AgentStepRequest {
        provider,
        executor: &executor,
        store,
        user_prompt: text,
        tools: &[],
        output_schema: None,
        model: "gpt-5.5",
        config: &AgentLoopConfig::default(),
        event_tx: Some(sender),
        inbound: None,
        loop_context: &mut context,
        cancel: None,
    })
    .await
}

#[test]
fn prepare_blank_and_pending_input_preserves_original_draft() -> TestResult {
    let store = EventStore::new();
    let mut state = state(InputHistory::in_memory(), &store)?;
    state.input_editor.paste_cells(" \r\n\t")?;
    let blank = state.input_editor.snapshot()?;
    assert!(prepare(&mut state)?.is_none());
    state.input_editor.validate_snapshot(&blank)?;
    state.input_editor.clear()?;
    state.input_editor.paste_cells("draft")?;
    let snapshot = prepare(&mut state)?.ok_or("nonblank draft omitted")?;
    let witness = history_witness(&state)?;
    let pending = state.input_editor.snapshot()?;
    let submitted = begin(&mut state, pending)?;
    assert_eq!(submitted.text, "draft");
    state.input_editor.validate_snapshot(&snapshot)?;
    assert!(prepare(&mut state)?.is_none());
    let duplicate = state.input_editor.snapshot()?;
    assert!(begin(&mut state, duplicate).is_err());
    state.input_editor.validate_snapshot(&snapshot)?;
    assert_eq!(history_witness(&state)?, witness);
    assert!(
        state
            .screen
            .feedback
            .as_deref()
            .is_some_and(|message| message.contains("previous input's acceptance"))
    );
    resolve(&mut state)?;
    assert!(state.pending_composer_submission.is_some());
    Ok(())
}

#[test]
fn stale_begin_refuses_before_creating_pending_submission() -> TestResult {
    let store = EventStore::new();
    let mut state = state(InputHistory::in_memory(), &store)?;
    state.input_editor.paste_cells("before")?;
    let old = prepare(&mut state)?.ok_or("draft absent")?;
    state.input_editor.paste_cells(" newer")?;
    let current = state.input_editor.snapshot()?;
    let history = history_witness(&state)?;
    let rows = state.transcript.projection.items().count();
    assert!(begin(&mut state, old).is_err());
    assert!(state.pending_composer_submission.is_none());
    state.input_editor.validate_snapshot(&current)?;
    assert_eq!(state.input_editor.text(), "before newer");
    assert_eq!(history_witness(&state)?, history);
    assert_eq!(state.transcript.projection.items().count(), rows);
    Ok(())
}

#[tokio::test]
async fn actual_opening_acceptance_retires_once_and_undo_restores_without_resend() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("history.txt");
    let store = EventStore::new();
    let mut state = state(InputHistory::load_from(&path), &store)?;
    state
        .input_editor
        .paste_cells("accepted original α\r\nline")?;
    let snapshot = prepare(&mut state)?.ok_or("draft absent")?;
    let pending = state.input_editor.snapshot()?;
    let submitted = begin(&mut state, pending)?;
    let source = state.transcript.projection.source();
    let (tx, receiver) = broadcast::channel(32);
    let root = AgentEventSender::new(tx, source.agent_id, "fixture".to_owned());
    let (sender, observation) = root.observe_execution(&store, source, Uuid::new_v4())?;
    bind(&mut state, Some(&submitted.local), Some(&observation))?;
    let provider = provider();
    assert!(matches!(
        run(&store, &sender, &provider, &submitted.text).await?,
        AgentStepResult::Completed { .. }
    ));
    assert!(matches!(
        observation.opening_input(),
        Some(PublicationResolution::Accepted(_))
    ));
    state.input_editor.validate_snapshot(&snapshot)?;
    resolve(&mut state)?;
    assert!(state.input_editor.is_empty());
    assert!(state.pending_composer_submission.is_none());
    resolve(&mut state)?;
    let reloaded = InputHistory::load_from(&path);
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded.entry(0), Some(snapshot.text()));
    state
        .input_editor
        .run_cell_command("history.undo", CommandArgs::NONE, options())?;
    assert_eq!(state.input_editor.text(), snapshot.text());
    assert_eq!(
        state.input_editor.kernel().state().cursor,
        *snapshot.cursor()
    );
    assert_eq!(provider.call_count(), 1);
    drop(receiver);
    Ok(())
}

struct RejectOpening;

impl PersistenceSink for RejectOpening {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        Err(SessionPersistError::Io(std::io::Error::other(format!(
            "fixture refuses event {} before publication",
            event.base().id,
        ))))
    }
}

#[tokio::test]
async fn actual_opening_rejection_preserves_draft_undo_and_empty_recall() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("history.txt");
    let store = EventStore::with_sink(Box::new(RejectOpening));
    let mut state = state(InputHistory::load_from(&path), &store)?;
    state.input_editor.paste_cells("retain rejected draft")?;
    let snapshot = prepare(&mut state)?.ok_or("draft absent")?;
    let witness = history_witness(&state)?;
    let pending = state.input_editor.snapshot()?;
    let submitted = begin(&mut state, pending)?;
    let source = state.transcript.projection.source();
    let (tx, receiver) = broadcast::channel(32);
    let root = AgentEventSender::new(tx, source.agent_id, "fixture".to_owned());
    let (sender, observation) = root.observe_execution(&store, source, Uuid::new_v4())?;
    bind(&mut state, Some(&submitted.local), Some(&observation))?;
    let provider = provider();
    assert!(
        run(&store, &sender, &provider, &submitted.text)
            .await
            .is_err()
    );
    assert!(matches!(
        observation.opening_input(),
        Some(PublicationResolution::NotAccepted(_))
    ));
    resolve(&mut state)?;
    assert!(state.pending_composer_submission.is_none());
    state.input_editor.validate_snapshot(&snapshot)?;
    assert_eq!(state.input_editor.text(), snapshot.text());
    assert_eq!(history_witness(&state)?, witness);
    assert_eq!(provider.call_count(), 0);
    assert!(InputHistory::load_from(&path).is_empty());
    assert!(
        state
            .screen
            .feedback
            .as_deref()
            .is_some_and(|message| message.contains("not accepted"))
    );
    drop(receiver);
    Ok(())
}

#[test]
fn accepted_stale_snapshot_keeps_new_draft_and_records_only_accepted_text() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("history.txt");
    let store = EventStore::new();
    let mut state = state(InputHistory::load_from(&path), &store)?;
    state.input_editor.paste_cells("accepted")?;
    let accepted = prepare(&mut state)?.ok_or("draft absent")?;
    state.input_editor.paste_cells(" newer")?;
    let current = state.input_editor.snapshot()?;
    let witness = history_witness(&state)?;
    accepted_local(&mut state, &accepted)?;
    state.input_editor.validate_snapshot(&current)?;
    assert_eq!(history_witness(&state)?, witness);
    assert_eq!(state.input_editor.text(), "accepted newer");
    let reloaded = InputHistory::load_from(&path);
    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded.entry(0), Some("accepted"));
    assert!(state.screen.feedback.as_deref().is_some_and(|message| {
        message.contains("Input accepted") && message.contains("not been resent")
    }));
    Ok(())
}

#[test]
fn accepted_history_failure_is_explicit_and_original_input_remains_undoable() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("history.txt");
    let history = InputHistory::load_from(&path);
    assert_eq!(history.path().as_deref(), Some(path.as_path()));
    // Bind first, then make only the expected destination unwritable as a file.
    std::fs::create_dir(&path)?;
    let store = EventStore::new();
    let mut state = state(history, &store)?;
    state
        .input_editor
        .paste_cells("accepted before history failure")?;
    let snapshot = prepare(&mut state)?.ok_or("draft absent")?;
    let original = TuiError::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "accepted fixture command could not finish its secondary effect",
    ));
    let returned = accepted_with_error(&mut state, &snapshot, original);
    assert!(matches!(
        &returned,
        TuiError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    assert!(returned.to_string().contains("secondary effect"));
    assert!(state.input_editor.is_empty());
    assert!(state.screen.feedback.as_deref().is_some_and(|message| {
        message.contains("Input accepted")
            && message.contains("recall history could not be saved")
            && message.contains("Do not resend")
    }));
    assert!(path.is_dir());
    assert!(!state.input_editor.history_prev()?);
    state
        .input_editor
        .run_cell_command("history.undo", CommandArgs::NONE, options())?;
    assert_eq!(state.input_editor.text(), snapshot.text());
    Ok(())
}

#[test]
fn accepted_auth_and_live_definition_secrets_do_not_enter_recall() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("history.txt");
    let store = EventStore::new();
    let mut state = state(InputHistory::load_from(&path), &store)?;
    for text in [
        "/AUTH private-auth-value",
        "/mcp add local stdio command --env TOKEN=private-env-value",
        "/mcp add remote http https://example.test/private --header Authorization=private-header-value",
    ] {
        state.input_editor.paste_cells(text)?;
        let snapshot = prepare(&mut state)?.ok_or("secret command absent")?;
        assert_eq!(snapshot.text(), text);
        accepted_local(&mut state, &snapshot)?;
        assert!(state.input_editor.is_empty());
        assert!(!state.input_editor.history_prev()?);
    }
    assert!(InputHistory::load_from(&path).is_empty());
    assert!(!path.exists());
    Ok(())
}
