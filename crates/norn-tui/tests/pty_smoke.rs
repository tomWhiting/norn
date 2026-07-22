//! Pseudo-terminal smoke and screen-state coverage for the TUI lifecycle.

use std::any::Any;
use std::io::{self, Read, Write as _};
use std::sync::{Arc, Mutex};
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
use vte::{Params, Parser, Perform};

const PTY_LIFECYCLE_CHILD_ENV: &str = "NORN_TUI_RUN_TUI_PTY_CHILD";
const PTY_APP_CHILD_ENV: &str = "NORN_TUI_RUN_APP_PTY_CHILD";
const PTY_APP_SCENARIO_ENV: &str = "NORN_TUI_RUN_APP_PTY_SCENARIO";
const SCREEN_ROWS: u16 = 24;
const SCREEN_COLS: u16 = 80;
const APP_OUTPUT_MARKER: &[u8] = b"screen harness output";
const CHILD_RESULT_MARKER: &[u8] = b"[spawn/worker completed]";
const CHILD_ACTIVITY_MARKER: &[u8] = b"read_file";
const ROOT_INBOUND_MARKER: &[u8] = b"root inbound wake handled";
const SOFT_WRAP_END_MARKER: &[u8] = b"wrap-omega";
const RESIZE_MARKER: &[u8] = b"resize harness output";
const RESIZED_STREAMING_SCROLL_REGION: &[u8] = b"\x1b[1;13r";
const TYPE_DURING_STREAM_MARKER: &[u8] = b"stream-after-input";
const SUBMIT_CLEAR_PROMPT: &str = "submit clear prompt before provider";
const SUBMIT_CLEAR_PROVIDER_MARKER: &[u8] = b"submit-clear provider output";
const EFFORT_CONFIRMATION: &str = "Reasoning effort: high";
const TOOLS_EMPTY_MARKER: &str = "No tools available.";

struct PtyRun {
    status: ExitStatus,
    output: Vec<u8>,
}

#[test]
fn run_tui_child_entrypoint() {
    if std::env::var_os(PTY_LIFECYCLE_CHILD_ENV).is_none() {
        return;
    }
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
    assert_output_contains(&run.output, b"\x1b[1;20r", "initial scroll region")?;
    assert_output_contains(&run.output, b"\x1b[?2004l", "bracketed paste disable")?;
    assert_output_contains(&run.output, b"\x1b[r", "scroll region reset")?;
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

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
    assert!(
        screen.contains("screen harness output"),
        "assistant output missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("gpt-5.5"),
        "status bar model missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("prompt from pty harness"),
        "submitted prompt missing from screen:\n{}",
        screen.debug_text(),
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
        PtyInteraction::WaitForOutputThenCtrlC {
            marker: b"prior assistant resume answer",
        },
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app resume-history", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
    assert!(
        screen.contains("prior user resume question"),
        "prior user message missing from screen:\n{}",
        screen.debug_text(),
    );
    assert!(
        screen.contains("Thought about"),
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
    assert!(
        screen.contains("gpt-5.5"),
        "status bar missing after replay:\n{}",
        screen.debug_text(),
    );

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

    let screen = TerminalScreen::from_output(&run.output, size.rows, size.cols);
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
    assert!(
        screen.contains("^C exit"),
        "status bar hints missing after soft-wrap output:\n{}",
        screen.debug_text(),
    );

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
        PtySizeSpec::default(),
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app child-result", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
    assert!(
        screen.contains("[spawn/worker completed]"),
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

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
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

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
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
    assert!(
        screen.contains("gpt-5.5"),
        "status bar missing after root inbound wake:\n{}",
        screen.debug_text(),
    );

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

    assert_output_contains(&run.output, RESIZE_MARKER, "resize scenario output")?;
    assert_output_contains(
        &run.output,
        RESIZED_STREAMING_SCROLL_REGION,
        "streaming scroll region after resize to 18 rows",
    )?;

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

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
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

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
    assert!(
        screen.contains("effort:high"),
        "effort status badge missing after slash command:\n{}",
        screen.debug_text(),
    );

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
        PtyInteraction::WriteWaitForOutputWriteWaitForOutputThenCtrlC {
            first_bytes: b"panel-growth-input-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz",
            first_marker: b"panel-growth-input",
            second_bytes: b"\x15",
            second_marker: b"\x1b[1;10r\x1b[?25l\x1b[11;1H",
        },
        size,
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app panel-growth", &run.status, &run.output).into());
    }

    assert_output_contains(&run.output, b"\x1b[1;8r", "expanded input scroll region")?;

    let screen = TerminalScreen::from_output(&run.output, size.rows, size.cols);
    assert!(
        screen.contains("^C exit"),
        "status bar hints missing after panel shrink:\n{}",
        screen.debug_text(),
    );
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

    let screen = TerminalScreen::from_output(&run.output, SCREEN_ROWS, SCREEN_COLS);
    assert!(
        screen.contains("effo"),
        "reasoning effort status badge missing from screen:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

#[test]
fn run_app_budgets_rows_on_small_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("idle"),
        PtyInteraction::WriteWaitForOutputThenCtrlC {
            bytes: b"",
            marker: b"gpt-5",
        },
        PtySizeSpec { rows: 8, cols: 56 },
    )?;

    if !run.status.success() {
        return Err(child_failure("run_app small-terminal", &run.status, &run.output).into());
    }

    let screen = TerminalScreen::from_output(&run.output, 8, 56);
    assert!(
        screen.contains("gpt-5"),
        "status bar missing on small terminal:\n{}",
        screen.debug_text(),
    );

    Ok(())
}

async fn run_fixture_app() -> Result<(), Box<dyn std::error::Error>> {
    let scenario = std::env::var(PTY_APP_SCENARIO_ENV).unwrap_or_else(|_| "basic".to_string());
    let (provider, initial_prompt, loop_context) = scenario_runtime(&scenario)?;
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

    norn_tui::run_app(norn_tui::TuiInputs {
        provider,
        executor,
        store,
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
    })
    .await?;
    Ok(())
}

type ScenarioRuntime = (Arc<dyn Provider>, Option<String>, LoopContext);

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
        )),
        "child-result" => {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(75)).await;
                let result = ChildAgentResult {
                    agent_id: uuid::Uuid::new_v4(),
                    agent_role: "spawn/worker".to_string(),
                    succeeded: true,
                    formatted_message: "child result arrived while root turn was active"
                        .to_string(),
                    error: None,
                    stop: None::<AgentStopReason>,
                    usage: Usage::default(),
                    subtree_usage: Usage::default(),
                };
                let _ = tx.send(result).await;
            });
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
            ))
        }
        "idle" | "child-activity" | "resume-history" => Ok((
            Arc::new(MockProvider::new(Vec::new())),
            None,
            LoopContext::default(),
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
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown PTY app scenario: {other}"),
        )
        .into()),
    }
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
    WriteWaitForOutputWriteWaitForOutputThenCtrlC {
        first_bytes: &'a [u8],
        first_marker: &'a [u8],
        second_bytes: &'a [u8],
        second_marker: &'a [u8],
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
    let _pty_test_guard = PTY_TEST_LOCK
        .lock()
        .map_err(|err| io::Error::other(format!("PTY test lock poisoned: {err}")))?;

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(std::env::current_exe()?);
    cmd.args(["--exact", test_name, "--nocapture"]);
    cmd.env(child_env, "1");
    if let Some(scenario) = scenario {
        cmd.env(PTY_APP_SCENARIO_ENV, scenario);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let output = Arc::new(Mutex::new(Vec::new()));
    let reader_handle = spawn_reader(pair.master.try_clone_reader()?, Arc::clone(&output));

    match interaction {
        PtyInteraction::None => {}
        PtyInteraction::WaitForOutputThenCtrlC { marker } => {
            wait_for_output(&output, marker, Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
        }
        PtyInteraction::WaitForOutputWaitForOutputThenCtrlC {
            first_marker,
            second_marker,
        } => {
            wait_for_output(&output, first_marker, Duration::from_secs(5))?;
            wait_for_output(&output, second_marker, Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
        }
        PtyInteraction::WaitForOutputScreenThenCancelThenCtrlC {
            marker,
            screen_needle,
        } => {
            wait_for_output(&output, marker, Duration::from_secs(5))?;
            wait_for_screen(&output, screen_needle, size, Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
            wait_for_output_count(&output, b"[cancelled]", 1, Duration::from_secs(5))?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
            wait_for_output_count(&output, b"[cancelled]", 2, Duration::from_secs(5))?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
        }
        PtyInteraction::ResizeAfterOutputThenCtrlC { marker, rows, cols } => {
            wait_for_output(&output, marker, Duration::from_secs(5))?;
            pair.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            wait_for_output(
                &output,
                RESIZED_STREAMING_SCROLL_REGION,
                Duration::from_secs(5),
            )?;
            let mut writer = pair.master.take_writer()?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
        }
        PtyInteraction::WriteWaitForOutputThenCtrlC { bytes, marker } => {
            wait_for_output(&output, b"gpt-5", Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
            if !bytes.is_empty() {
                writer.write_all(bytes)?;
                writer.flush()?;
            }
            wait_for_output(&output, marker, Duration::from_secs(5))?;
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
            wait_for_output(&output, first_marker, Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
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
                assert_screen_line_excludes(&clone_output(&output)?, size, typed_marker, forbidden)
            })
            .and_then(|()| wait_for_output(&output, second_marker, Duration::from_secs(5)));
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
            wait_for_output(&output, b"^C exit", Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
            writer.write_all(bytes)?;
            writer.flush()?;
            wait_for_screen(&output, submitted_prompt, size, Duration::from_secs(5))?;
            let snapshot = clone_output(&output)?;
            assert_screen_text_above_boundary(&snapshot, size, submitted_prompt, boundary_marker)?;
            assert_screen_text_not_below_boundary(
                &snapshot,
                size,
                submitted_prompt,
                boundary_marker,
            )?;
            if output_contains(&snapshot, provider_marker) {
                return Err(io::Error::other(format!(
                    "provider marker {:?} arrived before submit-clear assertion",
                    String::from_utf8_lossy(provider_marker),
                ))
                .into());
            }
            writer.write_all(b"\x03")?;
            writer.flush()?;
            wait_for_output(&output, b"[cancelled]", Duration::from_secs(5))?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
        }
        PtyInteraction::WriteWaitForSlashOutputThenCtrlC {
            bytes,
            marker,
            boundary_marker,
        } => {
            wait_for_output(&output, b"^C exit", Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
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
        PtyInteraction::WriteWaitForOutputWriteWaitForOutputThenCtrlC {
            first_bytes,
            first_marker,
            second_bytes,
            second_marker,
        } => {
            wait_for_output(&output, b"^C exit", Duration::from_secs(5))?;
            let mut writer = pair.master.take_writer()?;
            writer.write_all(first_bytes)?;
            writer.flush()?;
            wait_for_output(&output, first_marker, Duration::from_secs(5))?;
            writer.write_all(second_bytes)?;
            writer.flush()?;
            wait_for_output(&output, second_marker, Duration::from_secs(5))?;
            writer.write_all(b"\x03")?;
            writer.flush()?;
        }
    }

    let status = wait_for_child(&mut *child, Duration::from_secs(5))?;
    reader_handle.join().map_err(thread_panic_error)??;
    let output = clone_output(&output)?;
    Ok(PtyRun { status, output })
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    output: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    output
                        .lock()
                        .map_err(|err| {
                            io::Error::other(format!("PTY output lock poisoned: {err}"))
                        })?
                        .extend_from_slice(&buf[..n]);
                }
                Err(err) => return Err(err),
            }
        }
    })
}

fn wait_for_output(
    output: &Arc<Mutex<Vec<u8>>>,
    marker: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    wait_for_output_count(output, marker, 1, timeout)
}

fn wait_for_output_count(
    output: &Arc<Mutex<Vec<u8>>>,
    marker: &[u8],
    count: usize,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let marker_count = {
            let guard = output
                .lock()
                .map_err(|err| io::Error::other(format!("PTY output lock poisoned: {err}")))?;
            guard
                .windows(marker.len())
                .filter(|window| *window == marker)
                .count()
        };
        if marker_count >= count {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let snapshot = clone_output(output)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out waiting for PTY marker {:?} count {count}; output:\n{}",
                    String::from_utf8_lossy(marker),
                    String::from_utf8_lossy(&snapshot),
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_screen(
    output: &Arc<Mutex<Vec<u8>>>,
    marker: &str,
    size: PtySizeSpec,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = clone_output(output)?;
        let screen = TerminalScreen::from_output(&snapshot, size.rows, size.cols);
        if screen.contains(marker) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out waiting for screen marker {marker:?}; screen:\n{}",
                    screen.debug_text(),
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_screen_line_excludes(
    output: &[u8],
    size: PtySizeSpec,
    row_marker: &str,
    forbidden: &str,
) -> io::Result<()> {
    let screen = TerminalScreen::from_output(output, size.rows, size.cols);
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
    let screen = TerminalScreen::from_output(output, size.rows, size.cols);
    let debug = screen.debug_text();
    let lines = debug.lines().collect::<Vec<_>>();
    let Some(text_row) = lines.iter().position(|line| line.contains(text)) else {
        return Err(io::Error::other(format!(
            "screen text {text:?} missing; screen:\n{debug}",
        )));
    };
    let Some(boundary_row) = lines.iter().position(|line| line.contains(boundary_marker)) else {
        return Err(io::Error::other(format!(
            "fixed-panel boundary marker {boundary_marker:?} missing; screen:\n{debug}",
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
    let screen = TerminalScreen::from_output(output, size.rows, size.cols);
    let debug = screen.debug_text();
    let lines = debug.lines().collect::<Vec<_>>();
    let Some(boundary_row) = lines.iter().position(|line| line.contains(boundary_marker)) else {
        return Err(io::Error::other(format!(
            "fixed-panel boundary marker {boundary_marker:?} missing; screen:\n{debug}",
        )));
    };
    if lines
        .iter()
        .skip(boundary_row.saturating_add(1))
        .any(|line| line.contains(text))
    {
        return Err(io::Error::other(format!(
            "screen text {text:?} remained inside fixed panel; screen:\n{debug}",
        )));
    }
    Ok(())
}

fn output_contains(output: &[u8], marker: &[u8]) -> bool {
    output.windows(marker.len()).any(|window| window == marker)
}

fn clone_output(output: &Arc<Mutex<Vec<u8>>>) -> io::Result<Vec<u8>> {
    output
        .lock()
        .map(|guard| guard.clone())
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

#[derive(Clone, Copy)]
struct Cursor {
    row: usize,
    col: usize,
}

struct TerminalScreen {
    rows: usize,
    cols: usize,
    cells: Vec<Vec<char>>,
    cursor: Cursor,
    saved_cursor: Cursor,
    scroll_top: usize,
    scroll_bottom: usize,
}

impl TerminalScreen {
    fn from_output(output: &[u8], rows: u16, cols: u16) -> Self {
        let mut screen = Self::new(usize::from(rows), usize::from(cols));
        let mut parser = Parser::new();
        parser.advance(&mut screen, output);
        screen
    }

    fn new(rows: usize, cols: usize) -> Self {
        let cells = (0..rows).map(|_| vec![' '; cols]).collect();
        Self {
            rows,
            cols,
            cells,
            cursor: Cursor { row: 0, col: 0 },
            saved_cursor: Cursor { row: 0, col: 0 },
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
        }
    }

    fn contains(&self, needle: &str) -> bool {
        self.debug_text().contains(needle)
    }

    fn debug_text(&self) -> String {
        self.cells
            .iter()
            .map(|line| line.iter().collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn blank_line(&self) -> Vec<char> {
        vec![' '; self.cols]
    }

    fn put_char(&mut self, ch: char) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        self.clamp_cursor();
        self.cells[self.cursor.row][self.cursor.col] = ch;
        if self.cursor.col + 1 >= self.cols {
            self.cursor.col = 0;
            self.line_feed();
        } else {
            self.cursor.col += 1;
        }
    }

    fn line_feed(&mut self) {
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up();
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn scroll_up(&mut self) {
        if self.scroll_top >= self.scroll_bottom || self.scroll_bottom >= self.rows {
            return;
        }
        for row in self.scroll_top..self.scroll_bottom {
            self.cells[row] = self.cells[row + 1].clone();
        }
        self.cells[self.scroll_bottom] = self.blank_line();
    }

    fn set_cursor_position(&mut self, row: usize, col: usize) {
        if self.rows == 0 || self.cols == 0 {
            self.cursor = Cursor { row: 0, col: 0 };
            return;
        }
        self.cursor = Cursor {
            row: row.saturating_sub(1).min(self.rows - 1),
            col: col.saturating_sub(1).min(self.cols - 1),
        };
    }

    fn clamp_cursor(&mut self) {
        if self.rows == 0 || self.cols == 0 {
            self.cursor = Cursor { row: 0, col: 0 };
            return;
        }
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            2 | 3 => {
                for row in 0..self.rows {
                    self.cells[row] = self.blank_line();
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        self.clamp_cursor();
        match mode {
            0 => {
                for col in self.cursor.col..self.cols {
                    self.cells[self.cursor.row][col] = ' ';
                }
            }
            1 => {
                for col in 0..=self.cursor.col {
                    self.cells[self.cursor.row][col] = ' ';
                }
            }
            2 => {
                self.cells[self.cursor.row] = self.blank_line();
            }
            _ => {}
        }
    }

    fn set_scroll_region(&mut self, params: &Params) {
        let top = param_or(params, 0, 1).saturating_sub(1);
        let bottom = param_or(params, 1, self.rows).saturating_sub(1);
        if top < bottom && bottom < self.rows {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows.saturating_sub(1);
        }
        self.set_cursor_position(1, 1);
    }
}

impl Perform for TerminalScreen {
    fn print(&mut self, ch: char) {
        self.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.line_feed(),
            b'\r' => self.cursor.col = 0,
            0x08 => self.cursor.col = self.cursor.col.saturating_sub(1),
            b'\t' => self.cursor.col = (self.cursor.col + 8).min(self.cols.saturating_sub(1)),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match action {
            'H' | 'f' => {
                self.set_cursor_position(param_or(params, 0, 1), param_or(params, 1, 1));
            }
            'A' => {
                self.cursor.row = self.cursor.row.saturating_sub(param_or(params, 0, 1));
            }
            'B' => {
                self.cursor.row =
                    (self.cursor.row + param_or(params, 0, 1)).min(self.rows.saturating_sub(1));
            }
            'C' => {
                self.cursor.col =
                    (self.cursor.col + param_or(params, 0, 1)).min(self.cols.saturating_sub(1));
            }
            'D' => {
                self.cursor.col = self.cursor.col.saturating_sub(param_or(params, 0, 1));
            }
            'J' => self.erase_display(param_or(params, 0, 0)),
            'K' => self.erase_line(param_or(params, 0, 0)),
            'r' => self.set_scroll_region(params),
            's' => self.saved_cursor = self.cursor,
            'u' => self.cursor = self.saved_cursor,
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved_cursor = self.cursor,
            b'8' => self.cursor = self.saved_cursor,
            b'c' => {
                *self = Self::new(self.rows, self.cols);
            }
            _ => {}
        }
    }
}

fn param_or(params: &Params, index: usize, default: usize) -> usize {
    params
        .iter()
        .nth(index)
        .and_then(|param| param.first())
        .copied()
        .filter(|value| *value != 0)
        .map_or(default, usize::from)
}
