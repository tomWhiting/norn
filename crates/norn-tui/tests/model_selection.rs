//! Real keyboard-driven TUI model selection, observed at the next provider call.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use futures_util::stream;
use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::registry::AgentRegistry;
use norn::agent_loop::{LoopContext, config::AgentLoopConfig};
use norn::config::{ModelAliasSelection, ModelAliasSettings};
use norn::model_selection::{CatalogBackend, ModelRuntime};
use norn::provider::request::{MessageRole, ReasoningEffort, ServiceTier};
use norn::provider::{
    AgentEventSender, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream, StopReason, Usage,
};
use norn::session::EventStore;
use norn::tool::{ToolRegistry, context::ToolContext, output_budget::ToolOutputBudget};
use norn::tools::agent::AgentModel;
use norn_tui::{TuiInputs, input::InputHistory, render::fixed_panel::StatusBar};
use portable_pty::{CommandBuilder, native_pty_system};
use serde_json::{Value, json};
#[path = "support/retained_screen.rs"]
pub mod retained_screen;
use retained_screen::{Lifecycle, Screen};

const CHILD_SCENARIO: &str = "NORN_TUI_MODEL_SELECTION_SCENARIO";
const CAPTURE_PATH: &str = "NORN_TUI_MODEL_SELECTION_CAPTURE";
const SESSION_NAME: &str = "selection-fixture";
const SEED: &str = "preserve this conversation across model switches";
// Test deadlines and terminal geometry only; no production policy is defined here.
const DEADLINE: Duration = Duration::from_secs(10);

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
fn model_selection_child_entrypoint() -> TestResult {
    let Some(scenario) = std::env::var_os(CHILD_SCENARIO) else {
        tracing::info!("model-selection PTY child is launched by the parent integration tests");
        return Ok(());
    };
    let (explicit, reserve) = match scenario.to_str() {
        Some("derived") => (None, Some(30_000)),
        Some("explicit") => (Some(272_000), Some(30_000)),
        Some("reserve") => (None, Some(150_000)),
        _ => return Err(io::Error::other("unknown model-selection child scenario").into()),
    };
    let path = std::env::var_os(CAPTURE_PATH)
        .ok_or_else(|| io::Error::other("missing model-selection capture path"))?;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run_fixture(explicit, reserve, std::path::Path::new(&path)));
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("model-selection child failed: {error}");
            std::process::exit(1);
        }
    }
}

#[test]
fn keyboard_model_switch_updates_next_turn_budget_policy_and_history() -> TestResult {
    let observations = run_scenario("derived", |session| {
        session.command("/MoDeL   fast", "Switched model to gpt-5.3-codex-spark")?;
        session.assert_status("gpt-5.3-codex-spark", None, None)?;
        session.probe(1)?;
        session.command("/model astra", "Switched model to gpt-6-astra")?;
        session.command("/effort ultra", "Reasoning effort: ultra")?;
        session.command("/fast", "Service tier: fast")?;
        session.assert_status("gpt-6-astra", Some("ultra"), Some("fast"))?;
        session.probe(2)?;
        session.command("/model work", "Switched model to gpt-5.6-sol")?;
        session.probe(3)
    })?;
    assert_eq!(observations.len(), 4);
    assert_observation(
        &observations[0],
        "gpt-5.6-sol",
        272_000,
        Some("max"),
        Some("fast"),
    );
    assert_observation(&observations[1], "gpt-5.3-codex-spark", 128_000, None, None);
    assert_observation(
        &observations[2],
        "gpt-6-astra",
        372_000,
        Some("ultra"),
        Some("fast"),
    );
    assert_observation(
        &observations[3],
        "gpt-5.6-sol",
        272_000,
        Some("ultra"),
        Some("fast"),
    );
    for (index, observation) in observations.iter().enumerate() {
        let messages = observation["user_messages"]
            .as_array()
            .ok_or_else(|| io::Error::other("captured user messages are not an array"))?;
        assert_eq!(
            messages.len(),
            index + 1,
            "slash commands must not enter provider history"
        );
        assert_eq!(messages.first(), Some(&json!(SEED)));
        for turn in 1..=index {
            assert_eq!(messages.get(turn), Some(&json!(format!("probe-{turn}"))));
        }
        assert!(
            observation["system_messages"]
                .to_string()
                .contains("preserved system instruction")
        );
    }
    Ok(())
}

#[test]
fn keyboard_model_refusal_preserves_explicit_budget_and_all_policy() -> TestResult {
    let observations = run_scenario("explicit", |session| {
        session.command("/model fast", "/model failed:")?;
        session.assert_recent_contains("272000")?;
        session.assert_recent_contains("128000")?;
        session.assert_status("gpt-5.6-sol", Some("max"), Some("fast"))?;
        session.probe(1)
    })?;
    assert_eq!(observations.len(), 2);
    for observation in &observations {
        assert_observation(
            observation,
            "gpt-5.6-sol",
            272_000,
            Some("max"),
            Some("fast"),
        );
    }
    assert_eq!(observations[0]["policy"], observations[1]["policy"]);
    Ok(())
}

#[test]
fn keyboard_model_switch_cannot_disable_bound_compaction() -> TestResult {
    let observations = run_scenario("reserve", |session| {
        session.command("/model fast", "/model failed:")?;
        session.assert_recent_contains("auto_compact_reserve_tokens=150000")?;
        session.assert_recent_contains("disable automatic compaction")?;
        session.assert_status("gpt-5.6-sol", Some("max"), Some("fast"))?;
        session.probe(1)?;
        session.command("/model astra", "Switched model to gpt-6-astra")?;
        session.assert_status("gpt-6-astra", Some("max"), Some("fast"))?;
        session.probe(2)
    })?;
    assert_eq!(observations.len(), 3);
    assert_observation(
        &observations[0],
        "gpt-5.6-sol",
        272_000,
        Some("max"),
        Some("fast"),
    );
    assert_eq!(observations[0]["policy"], observations[1]["policy"]);
    assert_observation(
        &observations[2],
        "gpt-6-astra",
        372_000,
        Some("max"),
        Some("fast"),
    );
    for observation in &observations {
        assert_eq!(
            observation["system_messages"],
            observations[0]["system_messages"]
        );
    }
    assert_eq!(observations[1]["user_messages"], json!([SEED, "probe-1"]));
    assert_eq!(
        observations[2]["user_messages"],
        json!([SEED, "probe-1", "probe-2"])
    );
    Ok(())
}

#[test]
fn keyboard_alias_route_and_unknown_model_refusals_leave_session_unchanged() -> TestResult {
    let observations = run_scenario("derived", |session| {
        for (index, (command, reason)) in [
            ("/model other-profile", "provider profile 'other'"),
            ("/model chat-only", "API shape 'openai_chat_completions'"),
            (
                "/model nonexistent-selection-fixture",
                "nonexistent-selection-fixture",
            ),
        ]
        .iter()
        .enumerate()
        {
            session.command(command, "/model failed:")?;
            session.assert_recent_contains(reason)?;
            session.assert_status("gpt-5.6-sol", Some("max"), Some("fast"))?;
            session.probe(index + 1)?;
        }
        // Exact built-in IDs win even when a user alias shadows that spelling.
        session.command("/model gpt-5.6-sol", "Switched model to gpt-5.6-sol")?;
        session.probe(4)
    })?;
    assert_eq!(observations.len(), 5);
    for observation in &observations {
        assert_observation(
            observation,
            "gpt-5.6-sol",
            272_000,
            Some("max"),
            Some("fast"),
        );
    }
    Ok(())
}

#[test]
fn keyboard_effort_and_tier_refusals_are_atomic_and_identify_the_command() -> TestResult {
    let observations = run_scenario("derived", |session| {
        session.command(
            "/effort typo",
            "expected low, medium, high, xhigh, max, ultra, or default",
        )?;
        session.command("/model luna", "Switched model to gpt-5.6-luna")?;
        session.command("/effort high", "Reasoning effort: high")?;
        session.command("/effort ultra", "/effort failed:")?;
        session.assert_recent_contains("ultra")?;
        session.assert_recent_contains("gpt-5.6-luna")?;
        session.assert_status("gpt-5.6-luna", Some("high"), Some("fast"))?;
        session.probe(1)?;
        session.command("/model fast", "Switched model to gpt-5.3-codex-spark")?;
        session.command("/service-tier fast", "/service-tier failed:")?;
        session.assert_recent_contains("gpt-5.3-codex-spark")?;
        session.assert_status("gpt-5.3-codex-spark", Some("high"), None)?;
        session.probe(2)?;
        session.command("/effort default", "Reasoning effort cleared.")?;
        session.command("/service-tier none", "Service tier cleared.")?;
        session.assert_status("gpt-5.3-codex-spark", None, None)?;
        session.probe(3)
    })?;
    assert_eq!(observations.len(), 4);
    assert_observation(
        &observations[1],
        "gpt-5.6-luna",
        272_000,
        Some("high"),
        Some("fast"),
    );
    assert_observation(
        &observations[2],
        "gpt-5.3-codex-spark",
        128_000,
        Some("high"),
        None,
    );
    assert_observation(&observations[3], "gpt-5.3-codex-spark", 128_000, None, None);
    Ok(())
}

fn assert_observation(
    observation: &Value,
    model: &str,
    window: u64,
    effort: Option<&str>,
    tier: Option<&str>,
) {
    let budget = ToolOutputBudget::for_context_window(Some(window));
    assert_eq!(
        observation["policy"],
        json!({
            "model": model,
            "effort": effort,
            "tier": tier,
            "child_model": model,
            "child_effort": effort,
            "budget": budget_json(&budget),
        })
    );
}

fn budget_json(budget: &ToolOutputBudget) -> Value {
    json!({
        "read_default_line_limit": budget.read_default_line_limit,
        "read_hard_line_limit": budget.read_hard_line_limit,
        "read_output_char_limit": budget.read_output_char_limit,
        "read_hard_output_char_limit": budget.read_hard_output_char_limit,
        "read_line_char_limit": budget.read_line_char_limit,
        "model_output_inline_char_limit": budget.model_output_inline_char_limit,
    })
}

struct ObservingProvider {
    context: Arc<ToolContext>,
    observations: Mutex<Vec<Value>>,
}

impl Provider for ObservingProvider {
    fn model_catalog_backend(&self) -> Option<CatalogBackend> {
        Some(CatalogBackend::CODEX)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let budget = self
            .context
            .get_extension::<ToolOutputBudget>()
            .ok_or_else(|| provider_error("missing TUI tool-output budget"))?;
        let stamp = self
            .context
            .get_extension::<AgentModel>()
            .ok_or_else(|| provider_error("missing TUI child model stamp"))?;
        let mut observations = self
            .observations
            .lock()
            .map_err(|error| provider_error(&format!("observation lock poisoned: {error}")))?;
        let index = observations.len();
        observations.push(json!({
            "policy": {
                "model": request.model,
                "effort": request.reasoning_effort,
                "tier": request.service_tier,
                "child_model": stamp.model,
                "child_effort": stamp.reasoning_effort,
                "budget": budget_json(&budget),
            },
            "user_messages": request.messages.iter()
                .filter(|message| message.role == MessageRole::User)
                .map(|message| &message.content).collect::<Vec<_>>(),
            "system_messages": request.messages.iter()
                .filter(|message| message.role == MessageRole::System)
                .map(|message| &message.content).collect::<Vec<_>>(),
        }));
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta {
                text: format!("fixture-reply-{index}"),
            }),
            Ok(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            }),
        ])))
    }
}

fn provider_error(reason: &str) -> ProviderError {
    ProviderError::StreamError {
        reason: reason.to_owned(),
        transient: None,
    }
}

async fn run_fixture(
    explicit: Option<u64>,
    reserve: Option<u64>,
    capture: &std::path::Path,
) -> TestResult {
    let mut selection = ModelRuntime::new(
        Some(CatalogBackend::CODEX),
        "work",
        explicit,
        Some(ReasoningEffort::Max),
        Some(ServiceTier::Fast),
        fixture_aliases(),
    )?;
    let context = Arc::new(ToolContext::empty());
    let mut config = AgentLoopConfig {
        auto_compact_reserve_tokens: reserve,
        ..AgentLoopConfig::default()
    };
    let mut loop_context = LoopContext::new("preserved system instruction");
    selection.apply(&mut config, &mut loop_context, Some(&context));
    selection.bind_compaction_reserve(config.auto_compact_reserve_tokens);
    let provider = Arc::new(ObservingProvider {
        context: Arc::clone(&context),
        observations: Mutex::new(Vec::new()),
    });
    let registry = AgentRegistry::shared();
    let reservation = AgentRegistry::reserve(
        &registry,
        "/root".to_owned(),
        "lead".to_owned(),
        selection.model().to_owned(),
        None,
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 0,
                max_concurrent_children: 1,
            },
            inbound_capacity: 1,
            loop_config: None,
        },
        None,
    )?;
    let root_id = reservation.id();
    reservation.confirm()?;
    let (event_sender, agent_event_rx) = tokio::sync::broadcast::channel(8);
    Box::pin(norn_tui::run_app(TuiInputs {
        session_binding: Arc::new(norn::session::SessionBinding::ephemeral_root()),
        provider: Arc::clone(&provider) as Arc<dyn Provider>,
        executor: Arc::new(ToolRegistry::with_context(context)),
        store: Arc::new(EventStore::new()),
        registry,
        loop_context,
        agent_config: config,
        model: selection.model().to_owned(),
        model_selection: selection,
        tools: Vec::new(),
        history: InputHistory::in_memory(),
        status_bar: StatusBar {
            model_name: "gpt-5.6-sol".to_owned(),
            session_name: SESSION_NAME.to_owned(),
            reasoning_effort: Some("max".to_owned()),
            service_tier: Some("fast".to_owned()),
            key_hints: "^C exit".to_owned(),
            ..StatusBar::default()
        },
        root_id,
        initial_prompt: Some(SEED.to_owned()),
        data_dir: None,
        session_id: None,
        index_lock_deadline: DEADLINE,
        root_event_sender: AgentEventSender::new(event_sender, root_id, "root".to_owned()),
        agent_event_rx,
        root_inbound: None,
        mcp_control: None,
        root_cancel: tokio_util::sync::CancellationToken::new(),
    }))
    .await?;
    let observations = provider
        .observations
        .lock()
        .map_err(|error| io::Error::other(format!("observation lock poisoned: {error}")))?;
    std::fs::write(capture, serde_json::to_vec(&*observations)?)?;
    Ok(())
}

fn fixture_aliases() -> BTreeMap<String, ModelAliasSettings> {
    BTreeMap::from([
        (
            "work".to_owned(),
            ModelAliasSettings::Model("gpt-5.6-sol".to_owned()),
        ),
        (
            "fast".to_owned(),
            ModelAliasSettings::Model("codex-spark".to_owned()),
        ),
        (
            "gpt-5.6-sol".to_owned(),
            ModelAliasSettings::Model("codex-spark".to_owned()),
        ),
        (
            "other-profile".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                model: "gpt-6-astra".to_owned(),
                provider_profile: Some("other".to_owned()),
                api_shape: None,
            }),
        ),
        (
            "chat-only".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                model: "gpt-6-astra".to_owned(),
                provider_profile: None,
                api_shape: Some("openai_chat_completions".to_owned()),
            }),
        ),
    ])
}

struct PtySession {
    writer: Box<dyn Write + Send>,
    output: Vec<u8>,
    incoming: mpsc::Receiver<io::Result<Vec<u8>>>,
    recent_start: usize,
}

impl PtySession {
    fn wait_from(&mut self, marker: &str, start: usize) -> TestResult {
        let deadline = Instant::now() + DEADLINE;
        while !self.output[start..]
            .windows(marker.len())
            .any(|chunk| chunk == marker.as_bytes())
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = self.incoming.recv_timeout(remaining).map_err(|error| {
                io::Error::other(format!(
                    "waiting for PTY marker {marker:?}: {error}; output:\n{}",
                    String::from_utf8_lossy(&self.output),
                ))
            })??;
            self.output.extend(chunk);
        }
        Ok(())
    }

    fn wait_frame(
        &mut self,
        start: usize,
        predicate: impl Fn(&Screen) -> bool,
    ) -> TestResult<Screen> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(screen) = retained_screen::latest(&self.output, &[(24, 180)])?
                && screen.end_offset > start
                && predicate(&screen)
            {
                screen.assert_composer(1)?;
                return Ok(screen);
            }
            let chunk = self
                .incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| {
                    io::Error::other(format!(
                        "waiting for completed model-selection frame: {error}; output:\n{}",
                        String::from_utf8_lossy(&self.output)
                    ))
                })??;
            self.output.extend(chunk);
        }
    }

    fn command(&mut self, command: &str, confirmation: &str) -> TestResult {
        self.recent_start = self.output.len();
        self.writer.write_all(command.as_bytes())?;
        // A space dismisses slash completion; ordinary prompts remain byte-exact.
        if command.starts_with('/') {
            self.writer.write_all(b" ")?;
        }
        self.writer.write_all(b"\r")?;
        // Typing pins the reading viewport. Settings and provider replies are
        // appended at the tail; only the explicit status view keeps its anchor.
        if command != "/view status" {
            self.writer.write_all(b"/view follow \r")?;
        }
        self.writer.flush()?;
        self.wait_frame(self.recent_start, |screen| screen.contains(confirmation))?;
        Ok(())
    }

    fn probe(&mut self, index: usize) -> TestResult {
        self.command(&format!("probe-{index}"), &format!("fixture-reply-{index}"))?;
        // The command already follows the live tail. A completed frame from
        // this probe remains valid; an idempotent follow need not repaint it.
        self.wait_frame(self.recent_start, |screen| {
            let lines = screen.lines();
            let answer = lines
                .iter()
                .rposition(|line| line.contains(&format!("fixture-reply-{index}")));
            let completion = lines
                .iter()
                .rposition(|line| line.contains("Turn completed"));
            matches!((answer, completion), (Some(answer), Some(completion)) if completion > answer)
        })?;
        Ok(())
    }

    fn assert_recent_contains(&self, text: &str) -> TestResult {
        let screen = Screen::from_output(&self.output, 24, 180)?;
        if screen.end_offset > self.recent_start && screen.contains(text) {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "PTY command frame missing {text:?}: {}",
                screen.debug_text()
            ))
            .into())
        }
    }

    fn assert_status(
        &mut self,
        model: &str,
        effort: Option<&str>,
        tier: Option<&str>,
    ) -> TestResult {
        self.command("/view status", "Current view and runtime status")?;
        let expected = [
            format!("Model: {model}"),
            format!("Session: {SESSION_NAME}"),
            format!("Reasoning effort: {}", effort.unwrap_or("unset")),
            format!("Service tier: {}", tier.unwrap_or("unset")),
        ];
        self.wait_frame(self.recent_start, |screen| {
            let lines = screen.lines();
            lines
                .iter()
                .rposition(|line| line.trim() == "Current view and runtime status")
                .and_then(|header| lines.get(header + 1..header + 1 + expected.len()))
                .is_some_and(|fields| {
                    fields
                        .iter()
                        .zip(&expected)
                        .all(|(field, expected)| field.trim() == expected)
                })
        })?;
        // Return the local reading viewport to the live tail before the next provider probe.
        self.writer.write_all(b"/view follow \r")?;
        self.writer.flush()?;
        Ok(())
    }
}

fn run_scenario(
    scenario: &str,
    interaction: impl FnOnce(&mut PtySession) -> TestResult,
) -> TestResult<Vec<Value>> {
    let directory = tempfile::tempdir()?;
    let capture = directory.path().join("observations.json");
    let pair = native_pty_system().openpty(portable_pty::PtySize {
        rows: 24,
        cols: 180,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    #[cfg(unix)]
    let initial_termios = pair
        .master
        .get_termios()
        .ok_or_else(|| io::Error::other("model-selection PTY termios unavailable"))?;
    let mut command = CommandBuilder::new(std::env::current_exe()?);
    command.args(["--exact", "model_selection_child_entrypoint", "--nocapture"]);
    command.env(CHILD_SCENARIO, scenario);
    command.env(CAPTURE_PATH, &capture);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let mut process = ChildCleanup {
        killer: child.clone_killer(),
        armed: true,
    };
    let (exit_tx, exit_rx) = mpsc::sync_channel(1);
    let child_thread = std::thread::spawn(move || -> io::Result<()> {
        exit_tx
            .send(child.wait())
            .map_err(|error| io::Error::other(format!("PTY exit receiver lost: {error}")))
    });
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || -> io::Result<()> {
        let mut bytes = [0_u8; 4096];
        loop {
            let read = match reader.read(&mut bytes) {
                Ok(0) => return Ok(()),
                Ok(count) => Ok(bytes[..count].to_vec()),
                Err(error) => Err(error),
            };
            let finished = read.is_err();
            output_tx
                .send(read)
                .map_err(|error| io::Error::other(format!("PTY output receiver lost: {error}")))?;
            if finished {
                return Ok(());
            }
        }
    });
    let mut session = PtySession {
        writer,
        output: Vec::new(),
        incoming: output_rx,
        recent_start: 0,
    };
    let result = session
        .wait_from(std::str::from_utf8(retained_screen::SYNC_QUERY)?, 0)
        .and_then(|()| {
            session.writer.write_all(retained_screen::PROBE_REPLY)?;
            session.writer.flush()?;
            session.wait_frame(0, |screen| {
                screen.contains("fixture-reply-0") && screen.contains("Turn completed")
            })?;
            interaction(&mut session)
        });
    if result.is_err() {
        process.killer.kill()?;
    } else {
        session.writer.write_all(b"/exit \r")?;
        session.writer.flush()?;
    }
    let status = match exit_rx.recv_timeout(DEADLINE) {
        Ok(status) => status?,
        Err(error) => {
            process.killer.kill()?;
            return Err(
                io::Error::other(format!("model-selection child did not exit: {error}")).into(),
            );
        }
    };
    process.armed = false;
    child_thread
        .join()
        .map_err(|payload| thread_panic_error("PTY child wait", payload.as_ref()))??;
    reader_thread
        .join()
        .map_err(|payload| thread_panic_error("PTY reader", payload.as_ref()))??;
    for chunk in session.incoming.try_iter() {
        session.output.extend(chunk?);
    }
    result?;
    #[cfg(unix)]
    assert_eq!(
        pair.master.get_termios(),
        Some(initial_termios),
        "model-selection PTY termios changed"
    );
    Lifecycle::from_output(&session.output, 24, 180).assert_restored()?;
    assert!(
        status.success(),
        "model-selection child exited with {status:?}"
    );
    Ok(serde_json::from_slice(&std::fs::read(capture)?)?)
}

fn thread_panic_error(thread: &str, payload: &(dyn std::any::Any + Send)) -> io::Error {
    let message = if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        format!(
            "non-string panic payload with type ID {:?}",
            payload.type_id()
        )
    };
    io::Error::other(format!("{thread} thread panicked: {message}"))
}

// Assertions and I/O failures must not leave an interactive child running.
struct ChildCleanup {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    armed: bool,
}

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.killer.kill()
        {
            eprintln!("failed to stop model-selection PTY child during cleanup: {error}");
        }
    }
}
