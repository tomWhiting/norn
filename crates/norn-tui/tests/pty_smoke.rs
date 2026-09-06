//! Pseudo-terminal smoke and screen-state coverage for the TUI lifecycle.

use std::any::Any;
use std::io::{self, Read, Write as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use futures_util::stream;
use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::output::AgentStopReason;
use norn::agent::registry::AgentRegistry;
use norn::agent::result_channel::ChildAgentResult;
use norn::agent_loop::LoopContext;
use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::inbound::{ChannelMessage, MessageKind, inbound_channel};
use norn::provider::mock::MockProvider;
use norn::provider::request::ToolCallKind;
use norn::provider::{
    AgentEvent, AgentEventSender, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream, StopReason, Usage,
};
use norn::session::events::{EventBase, EventUsage, SessionEvent};
use norn::session::store::EventStore;
use norn::tool::ToolRegistry;
use norn_tui::input::InputHistory;
use norn_tui::render::fixed_panel::StatusBar;
use portable_pty::{Child, CommandBuilder, ExitStatus, PtySize, native_pty_system};
#[path = "support/retained_screen.rs"]
pub mod retained_screen;
use retained_screen::{Lifecycle, Screen as TerminalScreen};

const PTY_LIFECYCLE_CHILD_ENV: &str = "NORN_TUI_RUN_TUI_PTY_CHILD";
const PTY_APP_CHILD_ENV: &str = "NORN_TUI_RUN_APP_PTY_CHILD";
const PTY_APP_SCENARIO_ENV: &str = "NORN_TUI_RUN_APP_PTY_SCENARIO";
const PTY_CAPTURE_ENV: &str = "NORN_TUI_RUN_APP_PTY_CAPTURE";
const SCREEN_ROWS: u16 = 24;
const SCREEN_COLS: u16 = 80;
// Two explicit cancellation outcomes and their visible error details must both fit.
const CHILD_RESULT_ROWS: u16 = 60;
const APP_OUTPUT_MARKER: &[u8] = b"screen harness output";
const CHILD_RESULT_MARKER: &[u8] = b"Child spawn/worker";
const CHILD_ACTIVITY_MARKER: &[u8] = b"read_file";
const ROOT_INBOUND_MARKER: &[u8] = b"root inbound wake handled";
const SOFT_WRAP_END_MARKER: &[u8] = b"wrap-omega";
const RESIZE_MARKER: &[u8] = b"resize harness output";

const TYPE_DURING_STREAM_MARKER: &[u8] = b"stream-after-input";
const SUBMIT_CLEAR_PROMPT: &str = "submit clear prompt before provider";
const SUBMIT_CLEAR_PROVIDER_MARKER: &[u8] = b"submit-clear provider output";
const EFFORT_CONFIRMATION: &str = "Reasoning effort: high";
const TOOLS_EMPTY_MARKER: &str = "No tools available.";

struct PtyRun {
    status: ExitStatus,
    output: Vec<u8>,
    runtime: Option<serde_json::Value>,
}

#[test]
fn run_tui_child_entrypoint() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(PTY_LIFECYCLE_CHILD_ENV).is_none() {
        return Ok(());
    }
    writeln!(io::stdout(), "outer-screen-sentinel")?;
    io::stdout().flush()?;
    exit_after_child_result(norn_tui::run_tui());
}

#[test]
fn run_app_child_entrypoint() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(PTY_APP_CHILD_ENV).is_none() {
        return Ok(());
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run_fixture_app());
    exit_after_child_result(result);
}

#[test]
fn run_tui_sets_up_and_restores_terminal_in_pty() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_tui_child_entrypoint",
        PTY_LIFECYCLE_CHILD_ENV,
        None,
        PtyInteraction::None,
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_tui", &run.status, &run.output).into());
    }

    assert_output_contains(&run.output, b"\x1b[?2004h", "bracketed paste enable")?;
    let lifecycle = Lifecycle::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
    lifecycle.assert_restored()?;
    assert!(lifecycle.main_text().contains("outer-screen-sentinel"));
    assert_output_contains(&run.output, b"\x1b[?2004l", "bracketed paste disable")?;

    assert_output_contains(&run.output, b"\x1b[?25h", "cursor show reset")?;
    assert_output_contains(&run.output, b"\x1b[?7h", "line wrap reset")?;

    Ok(())
}

#[test]
fn run_app_renders_provider_output_in_screen_model() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("basic"),
        PtyInteraction::WaitForOutputThenCtrlC {
            marker: APP_OUTPUT_MARKER,
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    assert!(
        screen.contains("screen harness output"),
        "assistant output missing from screen:\n{}",
        screen.debug_text(),
    );
    screen.assert_composer(1)?;
    assert!(
        screen.contains("prompt from pty harness"),
        "submitted prompt missing from screen:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

#[test]
fn run_app_publication_retains_one_prompt_and_one_accepted_assistant()
-> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("basic"),
        PtyInteraction::WaitForCommittedBasicThenExit,
        PtySizeSpec {
            rows: 40,
            cols: 180,
        },
    )?;
    if !run.status.success() {
        return Err(child_failure("actual publication", &run.status, &run.output).into());
    }
    let report = run
        .runtime
        .ok_or("publication fixture lacks actual runtime report")?;
    assert_eq!(report["provider_calls"], serde_json::json!(1));
    let users = report["user_events"]
        .as_array()
        .ok_or("user event census missing")?;
    let assistants = report["assistant_events"]
        .as_array()
        .ok_or("assistant event census missing")?;
    assert_eq!(
        users.len(),
        1,
        "runtime appended duplicate user records: {report}"
    );
    assert_eq!(
        assistants.len(),
        1,
        "runtime appended duplicate assistant records: {report}"
    );
    assert_eq!(
        users[0]["content"],
        serde_json::json!("prompt from pty harness")
    );
    assert_eq!(
        assistants[0]["content"],
        serde_json::json!("screen harness output\nsecond visible line")
    );
    assert_ne!(users[0]["id"], assistants[0]["id"]);
    let details = TerminalScreen::from_output(&run.output, 40, 180)?;
    let actual_root = report["root_id"]
        .as_str()
        .ok_or("runtime root identity missing")?;
    assert!(
        details.contains(actual_root),
        "completion source does not name the actual runtime agent: {}",
        details.debug_text()
    );
    Ok(())
}

#[test]
fn run_app_replays_resumed_session_history_in_screen_model()
-> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("resume-history"),
        PtyInteraction::InspectResumedThenExit,
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app resume-history", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    assert!(
        screen.contains("prior user resume question"),
        "prior user message missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("Thinking"),
        "prior thinking block heading missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("Earlier reasoning summary"),
        "prior thinking body missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("prior assistant resume answer"),
        "prior assistant message missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("prior tool resume result"),
        "prior tool result missing from screen:\n{}",
        screen.debug_text(),
    );
    screen.assert_composer(1)?;

    Ok(())
}

#[test]
fn run_app_soft_wraps_long_output_in_screen_model() -> Result<(), Box<dyn std::error::Error>> {
    let size = PtySizeSpec { rows: 16, cols: 60 };
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("soft-wrap"),
        PtyInteraction::WaitForOutputThenCtrlC {
            marker: SOFT_WRAP_END_MARKER,
        },
        size,
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app soft-wrap", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, size.rows, size.cols)?;
    assert!(
        screen.contains("wrap-alpha"),
        "wrapped output start missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("wrap-omega"),
        "wrapped output end missing from screen:\n{}",
        screen.debug_text(),
    );
    screen.assert_composer(1)?;

    Ok(())
}

#[test]
fn run_app_surfaces_child_result_while_turn_is_active() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("child-result"),
        PtyInteraction::WaitForOutputScreenThenCancelThenCtrlC {
            marker: CHILD_RESULT_MARKER,
            screen_needle: "child result arrived while root turn was active",
        },
        PtySizeSpec {
            rows: CHILD_RESULT_ROWS,
            cols: SCREEN_COLS,
        },
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app child-result", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, CHILD_RESULT_ROWS, SCREEN_COLS)?;
    assert!(
        screen.contains("Child spawn/worker"),
        "child completion header missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("child result arrived while root turn was active"),
        "child result body missing from screen:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

#[test]
fn run_app_renders_child_activity_rows_in_screen_model() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("child-activity"),
        PtyInteraction::WaitForOutputThenCtrlC {
            marker: CHILD_ACTIVITY_MARKER,
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app child-activity", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    assert!(
        screen.contains("activity-child"),
        "child row missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("read_file"),
        "child activity missing from screen:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

#[test]
fn run_app_wakes_idle_root_on_inbound_steer() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("root-inbound-steer"),
        PtyInteraction::WaitForOutputWaitForOutputThenCtrlC {
            first_marker: ROOT_INBOUND_MARKER,
            second_marker: b"[3 in / 4 out",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app root-inbound-steer", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    assert!(
        !screen.contains("[root] msg delivered"),
        "root-only inbound delivery should not render a separate activity row:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("[3 in / 4 out"),
        "root inbound usage line missing from screen:\n{}",
        screen.debug_text(),
    );
    screen.assert_composer(1)?;

    Ok(())
}

#[test]
fn run_app_handles_resize_during_streaming_output() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("resize"),
        PtyInteraction::ResizeAfterOutputThenCtrlC {
            marker: RESIZE_MARKER,
            rows: 18,
            cols: 72,
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app resize", &run.status, &run.output).into());
    }

    let screen = retained_screen::latest(&run.output, &[(SCREEN_ROWS, SCREEN_COLS), (18, 72)])?
        .ok_or("resize fixture has no completed frame")?;
    assert_eq!((screen.rows, screen.cols), (18, 72));
    screen.assert_composer(1)?;
    assert_eq!(screen.occurrences("resize harness output before resize"), 1);
    assert_eq!(screen.occurrences("resize harness output after resize"), 1);

    Ok(())
}

#[test]
fn run_app_keeps_streaming_output_out_of_input_panel_after_typing()
-> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("type-during-stream"),
        PtyInteraction::WaitForOutputWriteWaitForCleanScreenThenExit {
            first_marker: b"stream-before-input",
            bytes: b"draft while running",
            second_marker: TYPE_DURING_STREAM_MARKER,
            typed_marker: "draft while running",
            forbidden: "stream-after-input",
            boundary_marker: "────────",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app type-during-stream", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    assert!(
        screen.contains("stream-after-input"),
        "streamed output missing from screen:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

#[test]
fn run_app_clears_input_panel_immediately_after_submit() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("submit-clear-before-stream"),
        PtyInteraction::WriteWaitForSubmittedPromptThenCancel {
            bytes: b"submit clear prompt before provider\r",
            submitted_prompt: SUBMIT_CLEAR_PROMPT,
            provider_marker: SUBMIT_CLEAR_PROVIDER_MARKER,
            boundary_marker: "────────",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app submit-clear", &run.status, &run.output).into());
    }

    Ok(())
}

#[test]
fn run_app_renders_effort_confirmation_above_input_panel() -> Result<(), Box<dyn std::error::Error>>
{
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("idle"),
        PtyInteraction::WriteWaitForSlashOutputThenCtrlC {
            bytes: b"/effort high\r",
            marker: EFFORT_CONFIRMATION,
            boundary_marker: "────────",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app effort-confirmation", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    screen.assert_composer(1)?;

    Ok(())
}

#[test]
fn run_app_renders_tools_block_above_input_panel() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("idle"),
        PtyInteraction::WriteWaitForSlashOutputThenCtrlC {
            bytes: b"/tools\r\r",
            marker: TOOLS_EMPTY_MARKER,
            boundary_marker: "────────",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app tools-block", &run.status, &run.output).into());
    }

    Ok(())
}

#[test]
fn run_app_grows_and_shrinks_input_panel_without_artifacts()
-> Result<(), Box<dyn std::error::Error>> {
    let size = PtySizeSpec { rows: 14, cols: 42 };
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("idle"),
        PtyInteraction::GrowAndClear {
            bytes: b"panel-growth-input-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz",
        },
        size,
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app panel-growth", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, size.rows, size.cols)?;
    screen.assert_composer(1)?;
    assert!(
        !screen.contains("panel-growth-input"),
        "cleared long input left artifacts:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

#[test]
fn run_app_handles_bracketed_paste_and_autocomplete() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("idle"),
        PtyInteraction::WriteWaitForOutputThenCtrlC {
            bytes: b"/eff\t\x1b[200~ high\x1b[201~\r",
            marker: b"Reasoning effort: high",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app paste", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS)?;
    screen.assert_composer(1)?;

    Ok(())
}

#[test]
fn run_app_budgets_rows_on_small_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("idle"),
        PtyInteraction::InspectEmptyComposerThenExit,
        PtySizeSpec { rows: 8, cols: 56 },
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app small-terminal", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, 8, 56)?;
    screen.assert_composer(1)?;

    Ok(())
}

async fn run_fixture_app() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = std::env::var(PTY_APP_SCENARIO_ENV).unwrap_or_else(|_| "basic".to_string());
    let (inner, initial_prompt, loop_context, child_sender) = scenario_runtime(&scenario)?;
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CountedProvider {
        inner,
        calls: Arc::clone(&provider_calls),
    });
    let executor = Arc::new(ToolRegistry::new());
    let store = Arc::new(fixture_store(&scenario)?);
    let registry = AgentRegistry::shared();
    let root_id = register_root_agent(&registry, "gpt-5.5")?;
    let (agent_event_tx, agent_event_rx) = tokio::sync::broadcast::channel::<AgentEvent>(32);
    let root_event_sender = AgentEventSender::new(agent_event_tx, root_id, "root".to_string());
    if scenario == "child-activity" {
        spawn_child_activity_fixture(&registry, root_id, &root_event_sender)?;
    }
    let root_inbound = if scenario == "root-inbound-steer" {
        let (tx, rx) = inbound_channel(8);
        tx.send(ChannelMessage {
            id: uuid::Uuid::new_v4(),
            sender_id: uuid::Uuid::new_v4(),
            from: "spawn/worker".to_string(),
            role: Some("worker".to_string()),
            to_id: root_id,
            content: "wake the idle root".to_string(),
            kind: MessageKind::Steer,
            seq: Some(1),
            timestamp: chrono::Utc::now(),
        })
        .await?;
        Some(rx)
    } else {
        None
    };

    let inputs = norn_tui::TuiInputs {
        frontend_preferences: norn_tui::frontend_preferences::FrontendPreferencesLaunch::run_only(),
        session_binding: Arc::new(norn::session::SessionBinding::ephemeral_root()),
        model_selection: norn::model_selection::ModelRuntime::new(
            provider.model_catalog_backend(),
            "gpt-5.5",
            Some(272_000),
            None,
            None,
            std::collections::BTreeMap::new(),
        )?,
        provider,
        executor,
        store: Arc::clone(&store),
        registry,
        loop_context,
        agent_config: AgentLoopConfig::default(),
        model: "gpt-5.5".to_string(),
        tools: Vec::new(),
        history: InputHistory::in_memory(),
        status_bar: StatusBar {
            model_name: "gpt-5.5".to_string(),
            session_name: "pty-screen".to_string(),
            key_hints: "^C exit".to_string(),
            ..StatusBar::default()
        },
        root_id,
        initial_prompt,
        data_dir: None,
        session_id: None,
        // Unused by these fixtures (ephemeral mode: `data_dir: None`
        // never constructs a SessionManager); any bound satisfies the
        // required field. Test configuration, not a production default.
        index_lock_deadline: std::time::Duration::from_secs(10),
        root_event_sender,
        agent_event_rx,
        root_inbound,
        mcp_control: None,
        // These fixtures assemble no agent tree, so the root token has no
        // descendants to cascade to; a fresh token is the honest stand-in
        // for the builder's `parts.cancel` the real driver passes.
        root_cancel: tokio_util::sync::CancellationToken::new(),
    };
    // Spawn only after all fallible fixture assembly. Retain and join the owner
    // even if the application fails, so receiver closure cannot hide the result.
    let child_delivery = child_sender.map(|sender| tokio::spawn(deliver_fixture_child(sender)));
    let app_result = Box::pin(norn_tui::run_app(inputs)).await;
    let delivery_result = match child_delivery {
        Some(task) => task
            .await
            .map_err(|error| {
                io::Error::other(format!("child-result fixture sender task failed: {error}"))
            })
            .flatten(),
        None => Ok(()),
    };
    match (app_result, delivery_result) {
        (Ok(()), Ok(())) => {}
        (Err(app), Ok(())) => return Err(app.into()),
        (Ok(()), Err(delivery)) => return Err(delivery.into()),
        (Err(app), Err(delivery)) => {
            return Err(io::Error::other(format!(
                "TUI application failed: {app}; child-result fixture also failed: {delivery}"
            ))
            .into());
        }
    }
    let capture = std::env::var_os(PTY_CAPTURE_ENV)
        .ok_or_else(|| io::Error::other("actual App fixture capture path is missing"))?;
    let events = store.events();
    let user_events: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { base, content } => Some(serde_json::json!({
                "id": base.id.as_str(), "content": content,
            })),
            _ => None,
        })
        .collect();
    let assistant_events: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantMessage { base, content, .. } => Some(serde_json::json!({
                "id": base.id.as_str(), "content": content,
            })),
            _ => None,
        })
        .collect();
    std::fs::write(
        capture,
        serde_json::to_vec(&serde_json::json!({
            "provider_calls": provider_calls.load(Ordering::SeqCst),
            "root_id": root_id.to_string(),
            "user_events": user_events,
            "assistant_events": assistant_events,
            "event_count": events.len(),
        }))?,
    )?;
    Ok(())
}

struct CountedProvider {
    inner: Arc<dyn Provider>,
    calls: Arc<AtomicUsize>,
}

impl Provider for CountedProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn model_catalog_backend(&self) -> Option<norn::model_selection::CatalogBackend> {
        self.inner.model_catalog_backend()
    }

    fn validate_replay(
        &self,
        messages: &[norn::provider::request::Message],
    ) -> Result<(), ProviderError> {
        self.inner.validate_replay(messages)
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.stream(request)
    }
}

type ScenarioRuntime = (
    Arc<dyn Provider>,
    Option<String>,
    LoopContext,
    Option<tokio::sync::mpsc::Sender<ChildAgentResult>>,
);

fn scenario_runtime(scenario: &str) -> Result<ScenarioRuntime, Box<dyn std::error::Error>> {
    match scenario {
        "basic" => Ok((
            Arc::new(MockProvider::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "screen harness output\nsecond visible line".to_string(),
                },
                done_event(),
            ]])),
            Some("prompt from pty harness".to_string()),
            LoopContext::default(),
            None,
        )),
        "soft-wrap" => Ok((
            Arc::new(MockProvider::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: concat!(
                        "wrap-alpha beta gamma delta epsilon zeta eta theta iota kappa ",
                        "lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi ",
                        "wrap-omega",
                    )
                    .to_string(),
                },
                done_event(),
            ]])),
            Some("soft wrap prompt from pty harness".to_string()),
            LoopContext::default(),
            None,
        )),
        "resize" => Ok((
            Arc::new(DelayedProvider {
                events: vec![
                    ProviderEvent::TextDelta {
                        text: "resize harness output before resize\n".to_string(),
                    },
                    ProviderEvent::TextDelta {
                        text: "resize harness output after resize\n".to_string(),
                    },
                    done_event(),
                ],
                delay: Duration::from_millis(150),
            }),
            Some("resize prompt from pty harness".to_string()),
            LoopContext::default(),
            None,
        )),
        "type-during-stream" => Ok((
            Arc::new(DelayedProvider {
                events: vec![
                    ProviderEvent::TextDelta {
                        text: "stream-before-input\n".to_string(),
                    },
                    ProviderEvent::TextDelta {
                        text: "stream-after-input\n".to_string(),
                    },
                    ProviderEvent::TextDelta {
                        text: "stream-tail-after-assertion\n".to_string(),
                    },
                    done_event(),
                ],
                delay: Duration::from_millis(150),
            }),
            Some("type during stream prompt from pty harness".to_string()),
            LoopContext::default(),
            None,
        )),
        "submit-clear-before-stream" => Ok((
            Arc::new(DelayedProvider {
                events: vec![
                    ProviderEvent::TextDelta {
                        text: "submit-clear provider output\n".to_string(),
                    },
                    done_event(),
                ],
                delay: Duration::from_secs(30),
            }),
            None,
            LoopContext::default(),
            None,
        )),
        "child-result" => {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let mut loop_context = LoopContext::default();
            loop_context.child_result_rx.replace(rx);
            Ok((
                Arc::new(DelayedProvider {
                    events: vec![
                        ProviderEvent::TextDelta {
                            text: "root turn still streaming\n".to_string(),
                        },
                        ProviderEvent::TextDelta {
                            text: "root turn finishing after child result\n".to_string(),
                        },
                        done_event(),
                    ],
                    delay: Duration::from_millis(120),
                }),
                Some("child result prompt from pty harness".to_string()),
                loop_context,
                Some(tx),
            ))
        }
        "idle" | "child-activity" | "resume-history" => Ok((
            Arc::new(MockProvider::new(Vec::new())),
            None,
            LoopContext::default(),
            None,
        )),
        "root-inbound-steer" => Ok((
            Arc::new(MockProvider::new(vec![vec![
                ProviderEvent::TextDelta {
                    text: "root inbound wake handled\n".to_string(),
                },
                done_event(),
            ]])),
            None,
            LoopContext::default(),
            None,
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown PTY app scenario: {other}"),
        )
        .into()),
    }
}

async fn deliver_fixture_child(
    sender: tokio::sync::mpsc::Sender<ChildAgentResult>,
) -> io::Result<()> {
    tokio::time::sleep(Duration::from_millis(75)).await;
    let result = ChildAgentResult {
        agent_id: uuid::Uuid::new_v4(),
        agent_role: "spawn/worker".to_string(),
        succeeded: true,
        formatted_message: "child result arrived while root turn was active".to_string(),
        error: None,
        stop: None::<AgentStopReason>,
        usage: Usage::default(),
        subtree_usage: Usage::default(),
    };
    sender.send(result).await.map_err(|error| {
        io::Error::other(format!(
            "child-result fixture receiver closed; undelivered result: {:?}",
            error.0
        ))
    })
}

#[test]
fn child_result_fixture_reports_closed_receiver_with_undelivered_result() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        let Err(error) = deliver_fixture_child(sender).await else {
            return Err(io::Error::other(
                "closed child fixture receiver accepted delivery",
            ));
        };
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("undelivered result: ChildAgentResult"));
        assert!(diagnostic.contains("agent_id:"));
        assert!(diagnostic.contains("spawn/worker"));
        assert!(diagnostic.contains("child result arrived while root turn was active"));
        assert!(diagnostic.contains("succeeded: true"));
        Ok(())
    })
}

fn fixture_store(scenario: &str) -> Result<EventStore, Box<dyn std::error::Error>> {
    let store = EventStore::new();
    if scenario == "resume-history" {
        store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "prior user resume question".to_string(),
        })?;
        store.append(SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "prior assistant resume answer".to_string(),
            thinking: "**Remembering context**\n\nEarlier reasoning summary".to_string(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_string(),
            response_id: None,
        })?;
        store.append(SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "call_prior_resume".to_string(),
            tool_name: "resume_tool".to_string(),
            output: serde_json::json!("prior tool resume result"),
            spool_ref: None,
            duration_ms: 12,
        })?;
    }
    Ok(store)
}

fn done_event() -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 3,
            output_tokens: 4,
            ..Usage::default()
        },
        response_id: None,
    }
}

struct DelayedProvider {
    events: Vec<ProviderEvent>,
    delay: Duration,
}

impl Provider for DelayedProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        drop(request);
        let events = self.events.clone();
        let delay = self.delay;
        let stream = stream::unfold(events.into_iter(), move |mut iter| async move {
            let event = iter.next()?;
            tokio::time::sleep(delay).await;
            Some((Ok(event), iter))
        });
        Ok(Box::pin(stream))
    }
}

fn register_root_agent(
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    model: &str,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let guard = AgentRegistry::reserve(
        registry,
        "/root".to_string(),
        "lead".to_string(),
        model.to_string(),
        None,
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        None,
    )?;
    let id = guard.id();
    guard.confirm()?;
    Ok(id)
}

fn spawn_child_activity_fixture(
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    root_id: uuid::Uuid,
    root_event_sender: &AgentEventSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let child_id = register_child_agent(registry, root_id)?;
    let child_sender = root_event_sender.for_child(child_id, "activity-child".to_string());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(75)).await;
        child_sender.send(ProviderEvent::ToolCallComplete {
            call_id: "tc-activity".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "tool_use_description": "checking child activity",
            })
            .to_string(),
            kind: ToolCallKind::Function,
        });
    });
    Ok(())
}

fn register_child_agent(
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    root_id: uuid::Uuid,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let guard = AgentRegistry::reserve(
        registry,
        "/root/activity-child".to_string(),
        "activity-child".to_string(),
        "gpt-5.5".to_string(),
        Some(root_id),
        ChildPolicy {
            messaging: MessagingScope::ParentOnly,
            delegation: DelegationBudget {
                remaining_depth: 0,
                max_concurrent_children: 0,
            },
            inbound_capacity: 8,
            loop_config: None,
        },
        None,
    )?;
    let id = guard.id();
    guard.confirm()?;
    Ok(id)
}

#[derive(Clone, Copy)]
enum PtyInteraction<'a> {
    None,
    WaitForCommittedBasicThenExit,
    InspectEmptyComposerThenExit,
    InspectResumedThenExit,
    GrowAndClear {
        bytes: &'a [u8],
    },
    WaitForOutputThenCtrlC {
        marker: &'a [u8],
    },
    WaitForOutputWaitForOutputThenCtrlC {
        first_marker: &'a [u8],
        second_marker: &'a [u8],
    },
    WaitForOutputScreenThenCancelThenCtrlC {
        marker: &'a [u8],
        screen_needle: &'a str,
    },
    ResizeAfterOutputThenCtrlC {
        marker: &'a [u8],
        rows: u16,
        cols: u16,
    },
    WriteWaitForOutputThenCtrlC {
        bytes: &'a [u8],
        marker: &'a [u8],
    },
    WaitForOutputWriteWaitForCleanScreenThenExit {
        first_marker: &'a [u8],
        bytes: &'a [u8],
        second_marker: &'a [u8],
        typed_marker: &'a str,
        forbidden: &'a str,
        boundary_marker: &'a str,
    },
    WriteWaitForSubmittedPromptThenCancel {
        bytes: &'a [u8],
        submitted_prompt: &'a str,
        provider_marker: &'a [u8],
        boundary_marker: &'a str,
    },
    WriteWaitForSlashOutputThenCtrlC {
        bytes: &'a [u8],
        marker: &'a str,
        boundary_marker: &'a str,
    },
}

#[derive(Clone, Copy)]
struct PtySizeSpec {
    rows: u16,
    cols: u16,
}

impl Default for PtySizeSpec {
    fn default() -> Self {
        Self {
            rows: SCREEN_ROWS,
            cols: SCREEN_COLS,
        }
    }
}

fn run_child_to_completion(
    test_name: &str,
    child_env: &str,
    scenario: Option<&str>,
    interaction: PtyInteraction<'_>,
    size: PtySizeSpec,
) -> Result<PtyRun, Box<dyn std::error::Error>> {
    // These fixtures exercise process-global PTY state, not concurrent TUI instances.
    static PTY_TEST_LOCK: Mutex<()> = Mutex::new(());
    let pty_test_guard = PTY_TEST_LOCK
        .lock()
        .map_err(|err| io::Error::other(format!("PTY test lock poisoned: {err}")))?;

    // Keep every PTY resource and cleanup owner inside the serialized scope.
    let run = {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let capture_directory = tempfile::tempdir()?;
        let capture = capture_directory.path().join("runtime.json");
        let mut cmd = CommandBuilder::new(std::env::current_exe()?);
        cmd.env(PTY_CAPTURE_ENV, &capture);
        cmd.args(["--exact", test_name, "--nocapture"]);
        cmd.env(child_env, "1");
        if let Some(scenario) = scenario {
            cmd.env(PTY_APP_SCENARIO_ENV, scenario);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        #[cfg(unix)]
        let initial_termios = pair
            .master
            .get_termios()
            .ok_or("PTY termios unavailable before launch")?;
        let mut child = pair.slave.spawn_command(cmd)?;
        let mut cleanup = PtyChildCleanup {
            killer: child.clone_killer(),
            armed: true,
        };
        drop(pair.slave);

        let output = Arc::new(OutputBuffer::default());
        let reader_handle = spawn_reader(pair.master.try_clone_reader()?, Arc::clone(&output));

        let mut writer = pair.master.take_writer()?;
        wait_for_output(&output, retained_screen::SYNC_QUERY, Duration::from_secs(5))?;
        writer.write_all(retained_screen::PROBE_REPLY)?;
        writer.flush()?;
        if child_env == PTY_APP_CHILD_ENV {
            wait_for_frame(
                &output,
                &[(size.rows, size.cols)],
                |_| true,
                Duration::from_secs(5),
            )?;
        }
        match interaction {
            PtyInteraction::None => {}
            PtyInteraction::InspectEmptyComposerThenExit => {
                let screen = wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| screen.composer_rows() == vec![usize::from(size.rows) - 3],
                    Duration::from_secs(5),
                )?;
                screen.assert_composer(1)?;
                assert_eq!(screen.lines()[usize::from(size.rows) - 3], "");
                assert_eq!(screen.cursor.0, 0);
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WaitForCommittedBasicThenExit => {
                let screen = wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen.contains("Turn completed")
                            && screen.contains("screen harness output")
                            && screen
                                .lines()
                                .iter()
                                .any(|line| line == "> prompt from pty harness")
                    },
                    Duration::from_secs(5),
                )?;
                assert_eq!(
                    screen.occurrences("prompt from pty harness"),
                    1,
                    "duplicate admitted prompt: {}",
                    screen.debug_text()
                );
                assert_eq!(
                    screen.occurrences("screen harness output"),
                    1,
                    "duplicate accepted assistant: {}",
                    screen.debug_text()
                );
                assert_eq!(screen.occurrences("second visible line"), 1);
                assert_eq!(screen.occurrences("Turn completed"), 1);
                screen.assert_composer(1)?;
                let user_row = screen
                    .lines()
                    .iter()
                    .position(|line| line == "> prompt from pty harness")
                    .ok_or("original submitted-user prefix missing")?;
                assert_eq!(screen.foreground_at(0, user_row), Some([80, 160, 220]));
                assert!(
                    !screen.lines().iter().any(|line| line == "Assistant"),
                    "generic assistant header replaced original prose style"
                );
                assert!(
                    !screen.contains("Accepted model:"),
                    "normal completion details expanded without request"
                );
                assert!(
                    !screen.contains("Provider completed"),
                    "provider Done and normal completion both remained visible"
                );
                let completion_row = screen
                    .lines()
                    .iter()
                    .position(|line| line.contains("Turn completed"))
                    .ok_or("completed turn row missing from observed frame")?
                    + 1;
                write!(
                    writer,
                    "\x1b[<0;1;{completion_row}M\x1b[<0;1;{completion_row}m"
                )?;
                writer.flush()?;
                let details = wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen.contains("Accepted model:")
                            && screen.contains("Provider response ID: None")
                            && screen.contains("Stop reason: EndTurn")
                    },
                    Duration::from_secs(5),
                )?;
                assert!(details.contains("Source: ViewSource"));
                assert!(details.contains("gpt-5.5"));
                assert!(details.contains("context_window: 272000"));
                assert!(details.contains("input_tokens: 3"));
                assert!(details.contains("output_tokens: 4"));
                assert!(details.contains("Elapsed:"));
                assert!(
                    !details.contains("Publication coverage incomplete"),
                    "normal completion still has unresolved publications: {}",
                    details.debug_text()
                );
                assert!(
                    !details.cursor_visible,
                    "conversation detail focus retained composer caret"
                );
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::InspectResumedThenExit => {
                let compact = wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen.contains("prior assistant resume answer")
                            && screen.contains("resume_tool")
                    },
                    Duration::from_secs(5),
                )?;
                assert!(
                    !compact.contains("prior tool resume result"),
                    "tool body expanded by default"
                );
                writer.write_all(b"/view detailed \r")?;
                writer.flush()?;
                wait_for_screen(
                    &output,
                    "prior tool resume result",
                    size,
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::GrowAndClear { bytes } => {
                writer.write_all(bytes)?;
                writer.flush()?;
                let grown = wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen.composer_rows().len() == 3 && screen.contains("panel-growth-input")
                    },
                    Duration::from_secs(5),
                )?;
                grown.assert_composer(3)?;
                writer.write_all(b"\x15")?;
                writer.flush()?;
                let cleared = wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen.composer_rows().len() == 1 && !screen.contains("panel-growth-input")
                    },
                    Duration::from_secs(5),
                )?;
                cleared.assert_composer(1)?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WaitForOutputThenCtrlC { marker } => {
                wait_for_screen(
                    &output,
                    std::str::from_utf8(marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WaitForOutputWaitForOutputThenCtrlC {
                first_marker,
                second_marker,
            } => {
                wait_for_screen(
                    &output,
                    std::str::from_utf8(first_marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                wait_for_screen(
                    &output,
                    std::str::from_utf8(second_marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WaitForOutputScreenThenCancelThenCtrlC {
                marker,
                screen_needle,
            } => {
                wait_for_screen(
                    &output,
                    std::str::from_utf8(marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                wait_for_screen(&output, screen_needle, size, Duration::from_secs(5))?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen
                            .lines()
                            .iter()
                            .filter(|line| line.starts_with("Turn cancelled ["))
                            .count()
                            == 1
                            && screen.contains("generating")
                    },
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |screen| {
                        screen
                            .lines()
                            .iter()
                            .filter(|line| line.starts_with("Turn cancelled ["))
                            .count()
                            == 2
                            && !screen.contains("generating")
                    },
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::ResizeAfterOutputThenCtrlC { marker, rows, cols } => {
                wait_for_screen(
                    &output,
                    std::str::from_utf8(marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                pair.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })?;
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols), (rows, cols)],
                    |screen| {
                        (screen.rows, screen.cols) == (rows, cols)
                            && screen.contains("resize harness output before resize")
                    },
                    Duration::from_secs(5),
                )?;
                // The resize occurs during streaming. Finish that same response before the
                // final Ctrl+C exits idle; a Ctrl+C while active only cancels its turn.
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols), (rows, cols)],
                    |screen| {
                        (screen.rows, screen.cols) == (rows, cols)
                            && screen.contains("resize harness output after resize")
                            && screen.contains("Turn completed")
                    },
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WriteWaitForOutputThenCtrlC { bytes, marker } => {
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |_| true,
                    Duration::from_secs(5),
                )?;
                if !bytes.is_empty() {
                    writer.write_all(bytes)?;
                    writer.flush()?;
                }
                wait_for_screen(
                    &output,
                    std::str::from_utf8(marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WaitForOutputWriteWaitForCleanScreenThenExit {
                first_marker,
                bytes,
                second_marker,
                typed_marker,
                forbidden,
                boundary_marker,
            } => {
                wait_for_screen(
                    &output,
                    std::str::from_utf8(first_marker)?,
                    size,
                    Duration::from_secs(5),
                )?;
                writer.write_all(bytes)?;
                writer.flush()?;
                wait_for_screen(&output, typed_marker, size, Duration::from_secs(5))?;
                wait_for_screen(&output, forbidden, size, Duration::from_secs(5))?;
                let assertion = assert_screen_text_above_boundary(
                    &clone_output(&output)?,
                    size,
                    forbidden,
                    boundary_marker,
                )
                .and_then(|()| {
                    assert_screen_line_excludes(
                        &clone_output(&output)?,
                        size,
                        typed_marker,
                        forbidden,
                    )
                })
                .and_then(|()| {
                    wait_for_screen(
                        &output,
                        std::str::from_utf8(second_marker).map_err(io::Error::other)?,
                        size,
                        Duration::from_secs(5),
                    )
                });
                writer.write_all(b"\x15")?;
                writer.write_all(b"\x03\x03\x03\x03")?;
                writer.flush()?;
                assertion?;
            }
            PtyInteraction::WriteWaitForSubmittedPromptThenCancel {
                bytes,
                submitted_prompt,
                provider_marker,
                boundary_marker,
            } => {
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |_| true,
                    Duration::from_secs(5),
                )?;
                writer.write_all(bytes)?;
                writer.flush()?;
                wait_for_screen(&output, submitted_prompt, size, Duration::from_secs(5))?;
                let snapshot = clone_output(&output)?;
                assert_screen_text_above_boundary(
                    &snapshot,
                    size,
                    submitted_prompt,
                    boundary_marker,
                )?;
                assert_screen_text_not_below_boundary(
                    &snapshot,
                    size,
                    submitted_prompt,
                    boundary_marker,
                )?;
                if rendered_history_contains(&snapshot, provider_marker, size)? {
                    return Err(io::Error::other(format!(
                        "provider marker {:?} arrived before submit-clear assertion",
                        String::from_utf8_lossy(provider_marker),
                    ))
                    .into());
                }
                writer.write_all(b"\x03")?;
                writer.flush()?;
                wait_for_screen(&output, "Turn cancelled", size, Duration::from_secs(5))?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
            PtyInteraction::WriteWaitForSlashOutputThenCtrlC {
                bytes,
                marker,
                boundary_marker,
            } => {
                wait_for_frame(
                    &output,
                    &[(size.rows, size.cols)],
                    |_| true,
                    Duration::from_secs(5),
                )?;
                writer.write_all(bytes)?;
                writer.flush()?;
                wait_for_screen(&output, marker, size, Duration::from_secs(5))?;
                let snapshot = clone_output(&output)?;
                assert_screen_text_above_boundary(&snapshot, size, marker, boundary_marker)?;
                assert_screen_text_not_below_boundary(&snapshot, size, marker, boundary_marker)?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
            }
        }

        let status = wait_for_child(&mut *child, Duration::from_secs(5))?;
        cleanup.armed = false;
        reader_handle.join().map_err(thread_panic_error)??;
        #[cfg(unix)]
        {
            let restored = pair
                .master
                .get_termios()
                .ok_or("PTY termios unavailable after exit")?;
            assert_eq!(
                restored, initial_termios,
                "actual terminal input attributes were not restored"
            );
        }
        let output = clone_output(&output)?;
        Lifecycle::from_output(&output, size.rows, size.cols).assert_restored()?;
        let runtime = if child_env == PTY_APP_CHILD_ENV && status.success() {
            Some(serde_json::from_slice(&std::fs::read(capture)?)?)
        } else {
            None
        };
        PtyRun {
            status,
            output,
            runtime,
        }
    };
    drop(pty_test_guard);
    Ok(run)
}

#[derive(Default)]
struct OutputState {
    bytes: Vec<u8>,
    closed: bool,
    failure: Option<String>,
}

#[derive(Default)]
struct OutputBuffer {
    state: Mutex<OutputState>,
    changed: Condvar,
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    output: Arc<OutputBuffer>,
) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let mut bytes = [0_u8; 4096];
        loop {
            let read = reader.read(&mut bytes);
            let mut captured = output
                .state
                .lock()
                .map_err(|error| io::Error::other(format!("PTY output lock: {error}")))?;
            match read {
                Ok(0) => {
                    captured.closed = true;
                    drop(captured);
                    output.changed.notify_all();
                    return Ok(());
                }
                Ok(count) => captured.bytes.extend_from_slice(&bytes[..count]),
                Err(error) => {
                    captured.failure = Some(error.to_string());
                    drop(captured);
                    output.changed.notify_all();
                    return Err(error);
                }
            }
            drop(captured);
            output.changed.notify_all();
        }
    })
}

fn wait_for_capture<T>(
    output: &Arc<OutputBuffer>,
    timeout: Duration,
    label: &str,
    mut observation: impl FnMut(&[u8]) -> io::Result<Option<T>>,
) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    let mut captured = output
        .state
        .lock()
        .map_err(|error| io::Error::other(format!("PTY output lock: {error}")))?;
    loop {
        if let Some(value) = observation(&captured.bytes)? {
            return Ok(value);
        }
        if captured.closed || captured.failure.is_some() || Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "PTY {label} unavailable: closed={}, failure={:?}; output:\n{}",
                captured.closed,
                captured.failure,
                String::from_utf8_lossy(&captured.bytes),
            )));
        }
        captured = output
            .changed
            .wait_timeout(captured, deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| io::Error::other(format!("PTY output notification: {error}")))?
            .0;
    }
}

fn wait_for_output(output: &Arc<OutputBuffer>, marker: &[u8], timeout: Duration) -> io::Result<()> {
    wait_for_output_count(output, marker, 1, timeout)
}

fn wait_for_output_count(
    output: &Arc<OutputBuffer>,
    marker: &[u8],
    count: usize,
    timeout: Duration,
) -> io::Result<()> {
    wait_for_capture(
        output,
        timeout,
        &format!("marker {:?}", String::from_utf8_lossy(marker)),
        |bytes| {
            Ok((bytes
                .windows(marker.len())
                .filter(|part| *part == marker)
                .count()
                >= count)
                .then_some(()))
        },
    )
}

fn wait_for_frame(
    output: &Arc<OutputBuffer>,
    geometries: &[(u16, u16)],
    predicate: impl Fn(&TerminalScreen) -> bool,
    timeout: Duration,
) -> io::Result<TerminalScreen> {
    wait_for_capture(output, timeout, "complete retained frame", |bytes| {
        Ok(retained_screen::latest(bytes, geometries)?.filter(&predicate))
    })
}

fn wait_for_screen(
    output: &Arc<OutputBuffer>,
    marker: &str,
    size: PtySizeSpec,
    timeout: Duration,
) -> io::Result<()> {
    wait_for_frame(
        output,
        &[(size.rows, size.cols)],
        |screen| screen.contains(marker),
        timeout,
    )?;
    Ok(())
}

fn assert_screen_line_excludes(
    output: &[u8],
    size: PtySizeSpec,
    row_marker: &str,
    forbidden: &str,
) -> io::Result<()> {
    let screen = TerminalScreen::from_output(output, size.rows, size.cols)?;
    let debug = screen.debug_text();
    let Some(line) = debug.lines().find(|line| line.contains(row_marker)) else {
        return Err(io::Error::other(format!(
            "screen row marker {row_marker:?} missing; screen:\n{debug}",
        )));
    };
    if line.contains(forbidden) {
        return Err(io::Error::other(format!(
            "screen row {row_marker:?} unexpectedly contains {forbidden:?}; screen:\n{debug}",
        )));
    }
    Ok(())
}

fn assert_screen_text_above_boundary(
    output: &[u8],
    size: PtySizeSpec,
    text: &str,
    boundary_marker: &str,
) -> io::Result<()> {
    let screen = TerminalScreen::from_output(output, size.rows, size.cols)?;
    let debug = screen.debug_text();
    let lines = debug.lines().collect::<Vec<_>>();
    let Some(text_row) = lines.iter().position(|line| line.contains(text)) else {
        return Err(io::Error::other(format!(
            "screen text {text:?} missing; screen:\n{debug}",
        )));
    };
    let Some(boundary_row) = screen.composer_rows().first().copied() else {
        return Err(io::Error::other(format!(
            "painted composer boundary for {boundary_marker:?} missing; screen:\n{debug}",
        )));
    };
    if text_row >= boundary_row {
        return Err(io::Error::other(format!(
            "screen text {text:?} appeared inside fixed panel; screen:\n{debug}",
        )));
    }
    Ok(())
}

fn assert_screen_text_not_below_boundary(
    output: &[u8],
    size: PtySizeSpec,
    text: &str,
    boundary_marker: &str,
) -> io::Result<()> {
    let screen = TerminalScreen::from_output(output, size.rows, size.cols)?;
    let debug = screen.debug_text();
    let lines = debug.lines().collect::<Vec<_>>();
    let Some(boundary_row) = screen.composer_rows().first().copied() else {
        return Err(io::Error::other(format!(
            "painted composer boundary for {boundary_marker:?} missing; screen:\n{debug}",
        )));
    };
    if lines
        .iter()
        .skip(boundary_row)
        .any(|line| line.contains(text))
    {
        return Err(io::Error::other(format!(
            "screen text {text:?} remained inside fixed panel; screen:\n{debug}",
        )));
    }
    Ok(())
}

fn rendered_history_contains(output: &[u8], marker: &[u8], size: PtySizeSpec) -> io::Result<bool> {
    let marker = std::str::from_utf8(marker).map_err(io::Error::other)?;
    // Preserve the negative assertion across every published frame: a provider
    // marker must not escape detection merely because a later redraw hid it.
    for (index, bytes) in output.windows(retained_screen::FRAME_END.len()).enumerate() {
        if bytes == retained_screen::FRAME_END
            && let Some(screen) = retained_screen::latest(
                &output[..index + retained_screen::FRAME_END.len()],
                &[(size.rows, size.cols)],
            )?
            && screen.contains(marker)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn clone_output(output: &Arc<OutputBuffer>) -> io::Result<Vec<u8>> {
    output
        .state
        .lock()
        .map(|guard| guard.bytes.clone())
        .map_err(|err| io::Error::other(format!("PTY output lock poisoned: {err}")))
}

fn thread_panic_error(payload: Box<dyn Any + Send + 'static>) -> io::Error {
    let message = match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_string(),
            Err(_) => "non-string panic payload".to_string(),
        },
    };
    io::Error::other(format!("PTY reader thread panicked: {message}"))
}

fn wait_for_child(
    child: &mut dyn Child,
    timeout: Duration,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let status = child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("PTY child timed out after {timeout:?}; status after kill: {status:?}"),
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn child_failure(label: &str, status: &ExitStatus, output: &[u8]) -> io::Error {
    io::Error::other(format!(
        "{label} child exited unsuccessfully: {status:?}\n{}",
        String::from_utf8_lossy(output),
    ))
}

fn exit_after_child_result(result: Result<(), impl std::fmt::Display>) -> ! {
    match result {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn assert_output_contains(
    output: &[u8],
    needle: &[u8],
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if output.windows(needle.len()).any(|window| window == needle) {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "missing {label} sequence {needle:?} in PTY output:\n{}",
        String::from_utf8_lossy(output),
    ))
    .into())
}

struct PtyChildCleanup {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    armed: bool,
}
impl Drop for PtyChildCleanup {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.killer.kill()
        {
            eprintln!("failed to stop canonical PTY child during cleanup: {error}");
        }
    }
}

// These oracle cases live in the normal test harness: the shared screen module
// is also imported by the MCP harness=false target, which cannot register #[test].
mod retained_screen_tests {
    use super::retained_screen::{FRAME_END, Lifecycle, Screen, latest};
    use std::io;

    fn frame(rows: u16, cols: u16, text: &str) -> Vec<u8> {
        let top = format!("───🮠 steer 🮣{}", "─".repeat(usize::from(cols) - 12));
        let bottom = format!("{}🮠 m 🮣───", "─".repeat(usize::from(cols) - 8));
        format!(
            "\x1b[?2026h\x1b[?25l\x1b[0m\x1b[2J\x1b[1;1H{text}\x1b[{};1H{top}\x1b[{};1H{bottom}\x1b[{rows};1H^C exit\x1b[{};1H\x1b[?25h\x1b[?2026l",
            rows - 3, rows - 1, rows - 2
        ).into_bytes()
    }

    #[test]
    fn completed_frame_preserves_unicode_cells_and_full_width_composer() -> io::Result<()> {
        let raw = String::from_utf8(frame(8, 32, "e\u{301}界👩‍💻!"))
            .map_err(|error| io::Error::other(format!("oracle fixture UTF-8: {error}")))?
            .replace(
                "\x1b[6;1H\x1b[?25h",
                "\x1b[6;1H\x1b[38;2;80;160;220;48;2;29;35;43m\x1b[39;49m \x1b[6;1H\x1b[?25h",
            );
        let screen = Screen::from_output(raw.as_bytes(), 8, 32)?;
        assert_eq!(screen.foreground_at(0, 5), None);
        assert_eq!(screen.lines()[0], "e\u{301}界👩‍💻!");
        assert_eq!(screen.cell(1, 0), Some("界"));
        assert_eq!(screen.cell(3, 0), Some("👩‍💻"));
        assert_eq!(screen.cell(5, 0), Some("!"));
        assert_eq!(screen.cursor, (0, 5));
        screen.assert_composer(1)
    }

    #[test]
    fn partial_frame_and_resize_epoch_cannot_masquerade_as_current_screen() -> io::Result<()> {
        let old = frame(8, 32, "old");
        assert!(latest(&old[..old.len() - FRAME_END.len()], &[(8, 32)])?.is_none());
        let new = frame(7, 24, "new");
        let mut output = old;
        output.extend_from_slice(&new[..new.len() - FRAME_END.len()]);
        let pending = latest(&output, &[(8, 32), (7, 24)])?
            .ok_or_else(|| io::Error::other("old frame disappeared"))?;
        assert_eq!((pending.rows, pending.cols), (8, 32));
        assert_eq!(pending.occurrences("old"), 1);
        output.extend_from_slice(FRAME_END);
        let resized = latest(&output, &[(8, 32), (7, 24)])?
            .ok_or_else(|| io::Error::other("new frame missing"))?;
        assert_eq!((resized.rows, resized.cols), (7, 24));
        assert_eq!(resized.occurrences("new"), 1);
        assert!(!resized.contains("old"));
        assert!(latest(&output, &[(7, 24)]).is_err());
        Ok(())
    }

    #[test]
    fn frame_oracle_rejects_out_of_bounds_and_control_injection() -> io::Result<()> {
        for forbidden in [
            "\x1b[9;1Hbad",
            "\x1b[1;33Hbad",
            "\x1b[0;1Hbad",
            "\x1b[1;3r",
            "\x1b]52;c;injection\x07",
            "bad\rtext",
            "123456789012345678901234567890123456789overflow",
            "\u{301}",
        ] {
            assert!(
                Screen::from_output(&frame(8, 32, forbidden), 8, 32).is_err(),
                "invalid frame accepted: {forbidden:?}"
            );
        }
        let styled = String::from_utf8(frame(8, 32, "body"))
            .map_err(|error| io::Error::other(format!("oracle fixture UTF-8: {error}")))?
            .replace(
                "\x1b[6;1H\x1b[?25h",
                "\x1b[6;1H\x1b[48;2;29;35;43mfilled\x1b[6;1H\x1b[?25h",
            );
        let screen = Screen::from_output(styled.as_bytes(), 8, 32)?;
        assert!(
            screen.assert_composer(1).is_err(),
            "blue filled composer was accepted as original style"
        );
        let missing_rule = String::from_utf8(frame(8, 32, "body"))
            .map_err(|error| io::Error::other(format!("oracle fixture UTF-8: {error}")))?
            .replace("───🮠 steer", "───  steer");
        assert!(Screen::from_output(missing_rule.as_bytes(), 8, 32).is_err());
        Ok(())
    }

    #[test]
    fn outer_screen_and_modes_are_observed_separately_from_printed_history() -> io::Result<()> {
        let mut output = b"sentinel\x1b[?1049h\x1b[?7l\x1b[?2004h\x1b[?1002h\x1b[?1006h".to_vec();
        output.extend(frame(8, 32, "inner"));
        output.extend_from_slice(
            b"\x1b[?2026l\x1b[0m\x1b[?1002l\x1b[?1006l\x1b[?2004l\x1b[?7h\x1b[?25h\x1b[?1049l",
        );
        let restored = Lifecycle::from_output(&output, 8, 32);
        restored.assert_restored()?;
        assert!(restored.main_text().contains("sentinel"));
        assert!(!restored.main_text().contains("inner"));
        output.extend_from_slice(b"\x1b[2J");
        assert!(
            !Lifecycle::from_output(&output, 8, 32)
                .main_text()
                .contains("sentinel")
        );
        output.extend_from_slice(b"\x1b[1;3r");
        assert!(
            Lifecycle::from_output(&output, 8, 32)
                .assert_restored()
                .is_err()
        );
        assert!(
            Lifecycle::from_output(b"\x1b[?1049h", 8, 32)
                .assert_restored()
                .is_err()
        );
        Ok(())
    }
}
