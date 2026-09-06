//! Actual-App fixture ownership, pushed PTY observations and explicit mock-provider barriers.

use std::any::Any;
use std::io::{self, BufRead as _, BufReader, Read, Write as _};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures_util::{StreamExt as _, stream};
use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::registry::AgentRegistry;
use norn::agent_loop::LoopContext;
use norn::agent_loop::config::AgentLoopConfig;
use norn::provider::mock::MockProvider;
use norn::provider::{
    AgentEvent, AgentEventSender, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream, StopReason, Usage,
};
use norn::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use norn::session::store::EventStore;
use norn::tool::ToolRegistry;
use norn_tui::input::InputHistory;
use norn_tui::render::fixed_panel::StatusBar;
use portable_pty::{
    ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use vte::{Parser, Perform};

use crate::retained_screen::{self, Lifecycle, Screen};

/// Bounded fixture waits, not a latency claim or product timeout.
const DEADLINE: Duration = Duration::from_secs(15);
const CHILD_ENV: &str = "NORN_RETAINED_WORKSPACE_CHILD";
const CONTROL_ENV: &str = "NORN_RETAINED_WORKSPACE_CONTROL";
const REPORT_ENV: &str = "NORN_RETAINED_WORKSPACE_REPORT";
const COMPOSER_ENV: &str = "NORN_RETAINED_COMPOSER_SEND_KEY";
const RECORDED_TOOL_ENV: &str = "NORN_RETAINED_RECORDED_TOOL";
/// Recorded history, never a tool executed by this fixture.
pub const TOOL_DESCRIPTION: &str = "Recorded tool selection";
const RECORDED_ASSISTANT: &str = "recorded assistant context";
const INITIAL: &str = "workspace provider held";
const RELEASED: &str = "workspace provider released";

/// Test failures retain the responsible operation's context.
pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Execute the actual TUI only in the dedicated child process.
pub fn child_entrypoint() -> TestResult {
    if std::env::var_os(CHILD_ENV).is_none() {
        return Ok(());
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(child_app());
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("retained workspace child: {error}");
            std::process::exit(1);
        }
    }
}

async fn child_app() -> TestResult {
    let composer_mode = std::env::var_os(COMPOSER_ENV).is_some();
    let frontend_preferences = if composer_mode {
        let root = std::env::current_dir()?;
        let settings = norn::config::load_resolved_settings(
            &root,
            &norn::config::McpRuntimeOverrides::default(),
        )?;
        if !settings.mcp_servers.is_empty() {
            return Err("composer fixture unexpectedly resolved MCP servers".into());
        }
        norn_tui::frontend_preferences::FrontendPreferencesLaunch::from_layers(
            &settings.tui_preferences,
            &settings.project_root,
        )?
    } else {
        norn_tui::frontend_preferences::FrontendPreferencesLaunch::run_only()
    };
    let inner = Arc::new(MockProvider::new(vec![vec![ProviderEvent::TextDelta {
        text: INITIAL.to_owned(),
    }]]));
    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn Provider> = Arc::new(GatedProvider {
        inner: Arc::clone(&inner),
        gate: Arc::clone(&gate),
    });
    let store = Arc::new(EventStore::new());
    if std::env::var_os(RECORDED_TOOL_ENV).is_some() {
        append_recorded_tool(&store)?;
    }
    let registry = AgentRegistry::shared();
    let reservation = AgentRegistry::reserve(
        &registry,
        "/root".to_owned(),
        "lead".to_owned(),
        "gpt-5.5".to_owned(),
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
    let (sender, receiver) = tokio::sync::broadcast::channel::<AgentEvent>(32);
    let root_event_sender = AgentEventSender::new(sender, root_id, "root".to_owned());
    let control_address = std::env::var(CONTROL_ENV)?;
    let control = TcpStream::connect(&control_address).map_err(|error| {
        io::Error::other(format!(
            "fixture control connect {control_address}: {error}"
        ))
    })?;
    let control_store = Arc::clone(&store);
    let control_provider = Arc::clone(&inner);
    let control_thread = std::thread::spawn(move || {
        serve_control(control, &control_store, &control_provider, &gate)
    });
    let result = Box::pin(norn_tui::run_app(norn_tui::TuiInputs {
        frontend_preferences,
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
        executor: Arc::new(ToolRegistry::new()),
        store: Arc::clone(&store),
        registry,
        loop_context: LoopContext::default(),
        agent_config: AgentLoopConfig::default(),
        model: "gpt-5.5".to_owned(),
        tools: Vec::new(),
        history: InputHistory::in_memory(),
        status_bar: StatusBar {
            model_name: "gpt-5.5".to_owned(),
            session_name: "retained-workspace-fixture".to_owned(),
            key_hints: "^C exit".to_owned(),
            ..StatusBar::default()
        },
        root_id,
        initial_prompt: (!composer_mode).then(|| "workspace fixture prompt".to_owned()),
        data_dir: None,
        session_id: None,
        // Ephemeral fixture never opens an index; this is only its explicit required input.
        index_lock_deadline: DEADLINE,
        root_event_sender,
        agent_event_rx: receiver,
        root_inbound: None,
        mcp_control: None,
        root_cancel: tokio_util::sync::CancellationToken::new(),
    }))
    .await;
    let control_result = join(control_thread, "fixture control reader");
    let report_path =
        PathBuf::from(std::env::var_os(REPORT_ENV).ok_or("fixture final report path missing")?);
    std::fs::write(&report_path, serde_json::to_vec(&census(&store, &inner))?).map_err(
        |error| {
            io::Error::other(format!(
                "fixture final report {}: {error}",
                report_path.display()
            ))
        },
    )?;
    control_result?;
    result?;
    Ok(())
}

fn append_recorded_tool(store: &EventStore) -> TestResult {
    let call = SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: RECORDED_ASSISTANT.to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "recorded_selection_call".to_owned(),
            name: "edit".to_owned(),
            arguments: json!({"tool_use_description": TOOL_DESCRIPTION,
                "description": "ordinary argument must not become the tool label",
                "path": "/fixture/no-disk.txt", "old_string": "old fixture text", "new_string": "new fixture text"}),
            kind: norn::provider::request::ToolCallKind::Function,
            caller: norn::provider::request::ToolCallCaller::Absent,
        }],
        usage: EventUsage::default(),
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    };
    let result = SessionEvent::ToolResult {
        base: EventBase::new(Some(call.base().id.clone())),
        tool_call_id: "recorded_selection_call".to_owned(),
        tool_name: "edit".to_owned(),
        output: json!({"committed": true, "path": "/fixture/no-disk.txt"}),
        spool_ref: None,
        duration_ms: 7,
    };
    store.append(call)?;
    store.append(result)?;
    Ok(())
}

struct GatedProvider {
    inner: Arc<MockProvider>,
    gate: Arc<Notify>,
}

impl Provider for GatedProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        let initial = self.inner.stream(request)?;
        let gate = Arc::clone(&self.gate);
        let released = stream::once(async move {
            gate.notified().await;
            Ok(ProviderEvent::TextDelta {
                text: format!("\n{RELEASED}"),
            })
        });
        let done = stream::iter([Ok(ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 3,
                output_tokens: 4,
                ..Usage::default()
            },
            response_id: None,
        })]);
        Ok(Box::pin(initial.chain(released).chain(done)))
    }
}

fn census(store: &EventStore, provider: &MockProvider) -> Value {
    store.with_events(|events| {
        json!({
            "provider_calls": provider.call_count(), "event_count": events.len(),
            "event_ids": events.iter().map(|event| event.base().id.as_str()).collect::<Vec<_>>(),
            "user_events": events.iter().filter_map(|event| match event {
                SessionEvent::UserMessage { content, .. } => Some(content.as_str()), _ => None,
            }).collect::<Vec<_>>(),
            "tool_results": events.iter().filter_map(|event| match event {
                SessionEvent::ToolResult { tool_call_id, output, .. } => Some(json!({"call_id":tool_call_id,"output":output})), _ => None,
            }).collect::<Vec<_>>(),
            "assistant_events": events.iter().filter_map(|event| match event {
                SessionEvent::AssistantMessage { content, .. } => Some(content.as_str()), _ => None,
            }).collect::<Vec<_>>(),
        })
    })
}

fn serve_control(
    stream: TcpStream,
    store: &EventStore,
    provider: &MockProvider,
    gate: &Notify,
) -> io::Result<()> {
    let mut stream = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let request: Value = serde_json::from_str(&line)?;
        let operation = request.get("operation").and_then(Value::as_str);
        let response = match operation {
            Some("snapshot") => census(store, provider),
            Some("append_history") => {
                let content = request
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| io::Error::other("append_history fixture content missing"))?;
                let event = SessionEvent::AssistantMessage {
                    base: EventBase::new(None),
                    response_items: Vec::new(),
                    content: content.to_owned(),
                    thinking: String::new(),
                    reasoning: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: EventUsage::default(),
                    stop_reason: "end_turn".to_owned(),
                    response_id: None,
                };
                let event_id = event.base().id.clone();
                store.append(event).map_err(|error| {
                    io::Error::other(format!("append_history fixture: {error}"))
                })?;
                json!({"event_id": event_id})
            }
            Some("release") => {
                gate.notify_one();
                json!({"released": true})
            }
            Some("close") => json!({"closed": true}),
            _ => return Err(io::Error::other("unknown fixture control operation")),
        };
        serde_json::to_writer(stream.get_mut(), &response)?;
        stream.get_mut().write_all(b"\n")?;
        stream.get_mut().flush()?;
        if operation == Some("close") {
            return Ok(());
        }
    }
}

/// Own every child/thread resource even when an assertion unwinds.
pub struct Workspace {
    master: Box<dyn MasterPty + Send>,
    writer: Option<Box<dyn io::Write + Send>>,
    output: Arc<Output>,
    reader: Option<JoinHandle<io::Result<()>>>,
    waiter: Option<JoinHandle<io::Result<ExitStatus>>>,
    exited: mpsc::Receiver<()>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    control: Option<BufReader<TcpStream>>,
    geometries: Vec<(u16, u16)>,
    directory: tempfile::TempDir,
    final_report: PathBuf,
    send_key: Option<String>,
    recorded_tool: bool,
    injected_history: Vec<String>,
    finished: bool,
}

/// Catch fixture assertions, then terminate/reap the child and join its reader before returning.
pub fn with_workspace(exercise: impl FnOnce(&mut Workspace) -> TestResult) -> TestResult {
    with_launch(None, false, false, |app| {
        exercise(app)?;
        Ok("workspace fixture prompt".to_owned())
    })
}

/// Replay actual recorded tool events, then hold the one ordinary provider turn.
pub fn with_recorded_tool(exercise: impl FnOnce(&mut Workspace) -> TestResult) -> TestResult {
    with_launch(None, false, true, |app| {
        exercise(app)?;
        Ok("workspace fixture prompt".to_owned())
    })
}

/// Launch idle with an actual settings layer; return the one expected accepted prompt.
pub fn with_composer(
    send_key: &str,
    exercise: impl FnOnce(&mut Workspace) -> TestResult<String>,
) -> TestResult {
    with_composer_keyboard(send_key, false, exercise)
}

/// Explicitly report or withhold Kitty disambiguation in this actual terminal peer.
pub fn with_composer_keyboard(
    send_key: &str,
    kitty_confirmed: bool,
    exercise: impl FnOnce(&mut Workspace) -> TestResult<String>,
) -> TestResult {
    with_launch(Some(send_key), kitty_confirmed, false, exercise)
}

fn with_launch(
    send_key: Option<&str>,
    kitty_confirmed: bool,
    recorded_tool: bool,
    exercise: impl FnOnce(&mut Workspace) -> TestResult<String>,
) -> TestResult {
    let startup = Instant::now();
    let mut app = Workspace::start(send_key, kitty_confirmed, recorded_tool)?;
    if let Some(send_key) = send_key {
        eprintln!(
            "{}",
            json!({"scope":"actual App child process launch through first complete idle PTY frame", "send_key":send_key, "startup_ns":startup.elapsed().as_nanos()})
        );
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exercise(&mut app)))
        .map_err(|payload| panic_error(payload.as_ref(), "workspace assertions"))
        .and_then(|result| result.map_err(|error| io::Error::other(error.to_string())));
    let stopped = match &result {
        Ok(prompt) => app.release_and_stop(prompt),
        Err(_) => app.finish(true),
    };
    let ensured = app.finish(stopped.is_err());
    let output = app.output.bytes()?;
    match (result, stopped, ensured) {
        (Ok(_), Ok(()), Ok(())) => Ok(()),
        (primary, cleanup, ensured) => Err(io::Error::other(format!("workspace exercise: {primary:?}; child cleanup: {cleanup:?}; final teardown: {ensured:?}; captured stdout/stderr:\n{}", String::from_utf8_lossy(&output))).into()),
    }
}

impl Workspace {
    fn start(
        send_key: Option<&str>,
        kitty_confirmed: bool,
        recorded_tool: bool,
    ) -> TestResult<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let directory = tempfile::tempdir()?;
        let final_report = directory.path().join("final-census.json");
        let pair = native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let read = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut command = CommandBuilder::new(std::env::current_exe()?);
        command.args([
            "--exact",
            "retained_workspace_child_entrypoint",
            "--nocapture",
        ]);
        command.env(CHILD_ENV, "1");
        command.env(CONTROL_ENV, address.to_string());
        command.env(REPORT_ENV, &final_report);
        if recorded_tool {
            command.env(RECORDED_TOOL_ENV, "1");
        }
        if let Some(send_key) = send_key {
            let home = directory.path().join("norn-home");
            std::fs::create_dir(&home)?;
            std::fs::write(
                home.join("settings.json"),
                serde_json::to_vec(&json!({"tui":{"composer":{"send_key":send_key}}}))?,
            )?;
            command.env(COMPOSER_ENV, send_key);
            command.env("NORN_HOME", &home);
            command.env("HOME", directory.path());
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.cwd(directory.path());
        let mut child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let (exit_sender, exited) = mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            let result = child.wait();
            exit_sender
                .send(())
                .map_err(|error| io::Error::other(format!("child exit notification: {error}")))?;
            result
        });
        let output = Arc::new(Output::default());
        let mut app = Self {
            master: pair.master,
            writer: Some(writer),
            output: Arc::clone(&output),
            reader: None,
            waiter: Some(waiter),
            exited,
            killer,
            control: None,
            geometries: vec![(24, 100)],
            directory,
            final_report,
            send_key: send_key.map(str::to_owned),
            recorded_tool,
            injected_history: Vec::new(),
            finished: false,
        };
        app.reader = Some(std::thread::spawn(move || read_output(read, &output)));
        if let Err(error) = app.admit(listener, address, kitty_confirmed) {
            let cleanup = app.finish(true);
            return Err(io::Error::other(format!(
                "PTY admission: {error}; teardown: {cleanup:?}; captured stdout/stderr:\n{}",
                String::from_utf8_lossy(&app.output.bytes()?)
            ))
            .into());
        }
        Ok(app)
    }

    fn admit(
        &mut self,
        listener: TcpListener,
        address: SocketAddr,
        kitty_confirmed: bool,
    ) -> io::Result<()> {
        let (sender, accepted) = mpsc::sync_channel(1);
        let acceptor = std::thread::spawn(move || {
            sender.send(listener.accept()).map_err(|error| {
                io::Error::other(format!("fixture connection notification: {error}"))
            })
        });
        let accepted = accepted.recv_timeout(DEADLINE);
        if accepted.is_err() {
            // Wake the blocking acceptor so a failed child start cannot strand its thread.
            let wake = TcpStream::connect_timeout(&address, DEADLINE);
            let joined = join(acceptor, "fixture acceptor");
            return Err(io::Error::other(format!(
                "fixture control connection: {accepted:?}; accept wake: {wake:?}; join: {joined:?}"
            )));
        }
        join(acceptor, "fixture acceptor")?;
        let (control, peer) = accepted
            .map_err(|error| io::Error::other(format!("fixture connection: {error}")))??;
        if !peer.ip().is_loopback() {
            return Err(io::Error::other("fixture control peer is not loopback"));
        }
        control.set_read_timeout(Some(DEADLINE))?;
        control.set_write_timeout(Some(DEADLINE))?;
        self.control = Some(BufReader::new(control));
        self.output.wait("synchronized-output query", |bytes| {
            Ok(bytes
                .windows(retained_screen::SYNC_QUERY.len())
                .any(|part| part == retained_screen::SYNC_QUERY)
                .then_some(()))
        })?;
        if kitty_confirmed {
            self.send(b"\x1b[?1u")?;
        }
        self.send(retained_screen::PROBE_REPLY)?;
        self.frame(0, |screen| {
            self.send_key.is_some() || screen.contains(INITIAL)
        })?
        .assert_composer(1)?;
        Ok(())
    }

    /// Real provider/store counters read on an independent explicit fixture request.
    pub fn snapshot(&mut self) -> io::Result<Value> {
        self.control("snapshot")
    }

    /// Append actual accepted store history without a provider call or a fabricated UI row.
    pub fn append_history(&mut self, content: &str) -> io::Result<Value> {
        let response =
            self.control_request(&json!({"operation": "append_history", "content":content}))?;
        self.injected_history.push(content.to_owned());
        Ok(response)
    }

    fn control(&mut self, operation: &str) -> io::Result<Value> {
        self.control_request(&json!({"operation": operation}))
    }

    fn control_request(&mut self, request: &Value) -> io::Result<Value> {
        let operation = request
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("fixture control operation missing"))?;
        let stream = self
            .control
            .as_mut()
            .ok_or_else(|| io::Error::other("fixture control connection absent"))?;
        serde_json::to_writer(stream.get_mut(), request)?;
        stream.get_mut().write_all(b"\n")?;
        stream.get_mut().flush()?;
        let mut response = String::new();
        if stream.read_line(&mut response)? == 0 {
            return Err(io::Error::other(format!(
                "fixture control closed during {operation}"
            )));
        }
        serde_json::from_str(&response).map_err(io::Error::from)
    }

    /// A fixture-owned explicit export path, including paths containing spaces.
    pub fn destination(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("PTY input already closed"))?;
        writer.write_all(bytes)?;
        writer.flush()
    }

    fn frame(&self, after: usize, predicate: impl Fn(&Screen) -> bool) -> io::Result<Screen> {
        self.output.wait("matching complete frame", |bytes| {
            Ok(retained_screen::latest(bytes, &self.geometries)?
                .filter(|screen| screen.end_offset > after && predicate(screen)))
        })
    }

    /// Deliver original terminal bytes and await a newly published matching screen.
    pub fn input(
        &mut self,
        bytes: &[u8],
        predicate: impl Fn(&Screen) -> bool,
    ) -> io::Result<Screen> {
        let after = self.output.bytes()?.len();
        self.send(bytes)?;
        self.frame(after, predicate)
    }

    /// Observe the already-published frame without requiring a redundant repaint.
    pub fn screen(&self) -> io::Result<Screen> {
        self.frame(0, |_| true)
    }

    /// The physical send key selected by this fixture's actual launch preferences.
    pub fn submit_key(&self) -> &'static [u8] {
        match self.send_key.as_deref() {
            Some("shift-enter") => b"\x1b[13;2u",
            Some("alt-enter") => b"\x1b\r",
            _ => b"\r",
        }
    }

    /// Submit the same local view command as a person; no direct frontend-method calls.
    pub fn command(&mut self, command: &str, expected: &str) -> io::Result<Screen> {
        let name = command
            .split_whitespace()
            .next()
            .ok_or_else(|| io::Error::other("local command is empty"))?;
        let after = self.output.bytes()?.len();
        self.send(format!("\x15{command}").as_bytes())?;
        // Observe a nonempty local draft before Enter. Otherwise an old feedback
        // line in a redraw of Ctrl+U could falsely acknowledge the new command.
        self.frame(after, |screen| {
            let lines = screen.lines();
            screen
                .composer_rows()
                .iter()
                .any(|row| lines.get(*row).is_some_and(|line| line.contains(name)))
        })?;
        let after = self.output.bytes()?.len();
        self.send(self.submit_key())?;
        self.frame(after, |screen| {
            screen.contains(expected) && screen.cursor.0 == 0 && screen.assert_composer(1).is_ok()
        })
    }

    /// Exercise an actual terminal key sequence and wait on reader notifications.
    pub fn key(&mut self, keys: &[u8], expected: &str) -> io::Result<Screen> {
        let previous = self.output.bytes()?;
        let after = previous.len();
        let previous_copies = copies(&previous)?.len();
        let current = retained_screen::latest(&previous, &self.geometries)?
            .ok_or_else(|| io::Error::other("key request has no established current frame"))?;
        let copy_requested = expected.starts_with("Sent ");
        self.send(keys)?;
        self.output
            .wait("key feedback and requested clipboard bytes", |bytes| {
                let screen = retained_screen::latest(bytes, &self.geometries)?.filter(|screen| {
                    screen.rows == current.rows
                        && screen.cols == current.cols
                        && screen.contains(expected)
                        && (copy_requested || screen.end_offset > after)
                });
                // A repeated copy emits a new OSC payload even when feedback,
                // geometry and retained frame are byte-identical. A repaint is
                // not required; the new payload is the actual action evidence.
                if copy_requested && copies(bytes)?.len() <= previous_copies {
                    return Ok(None);
                }
                Ok(screen)
            })
    }

    /// Resize the PTY itself; observe the requested geometry in a completed frame.
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<Screen> {
        let after = self.output.bytes()?.len();
        if self.geometries.last() != Some(&(rows, cols)) {
            self.geometries.push((rows, cols));
        }
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| io::Error::other(format!("PTY resize {cols}x{rows}: {error}")))?;
        self.frame(after, |screen| screen.rows == rows && screen.cols == cols)
    }

    /// SGR mouse coordinates are one-based, anchored in the observed complete screen.
    pub fn mouse_drag(&mut self, start: u16, end: u16, row: u16) -> io::Result<Screen> {
        let after = self.output.bytes()?.len();
        self.send(
            format!("\x1b[<0;{start};{row}M\x1b[<32;{end};{row}M\x1b[<0;{end};{row}m").as_bytes(),
        )?;
        self.frame(after, |screen| {
            !screen.cursor_visible && screen.contains("View controls")
        })
    }

    /// Exact OSC 52 base64 fields emitted by the terminal owner, without querying a clipboard.
    pub fn copy_payloads(&self) -> io::Result<Vec<Vec<u8>>> {
        copies(&self.output.bytes()?)
    }

    /// Exercise F4 while its Conversation feedback is hidden by another narrow pane.
    /// Require one new clipboard payload and a valid current-geometry frame; no repaint is implied.
    pub fn copy_with_hidden_feedback(&mut self) -> io::Result<Screen> {
        let previous = self.copy_payloads()?.len();
        let current = self.screen()?;
        self.send(b"\x1bOS")?;
        self.output.wait(
            "one clipboard payload with retained pane geometry",
            |bytes| {
                let count = copies(bytes)?.len();
                if count > previous + 1 {
                    return Err(io::Error::other(
                        "one F4 press emitted multiple clipboard payloads",
                    ));
                }
                if count != previous + 1 {
                    return Ok(None);
                }
                Ok(retained_screen::latest(bytes, &self.geometries)?
                    .filter(|screen| screen.rows == current.rows && screen.cols == current.cols))
            },
        )
    }

    /// Complete the already-running provider and observe its real usage update outside the dragged pane.
    pub fn release_provider(&mut self) -> io::Result<Screen> {
        let after = self.output.bytes()?.len();
        self.control("release")?;
        self.frame(after, |screen| screen.contains("3↑ 4↓"))
    }

    /// Compare the observed transport payload with independently specified selected text.
    pub fn assert_last_copy(&self, expected: &str) -> io::Result<()> {
        use termina::escape::osc::{Osc, Selection};
        let encoded = Osc::SetSelection(Selection::CLIPBOARD, expected).to_string();
        let expected_payloads = copies(encoded.as_bytes())?;
        let actual = self.copy_payloads()?;
        if actual.last() != expected_payloads.last() || expected_payloads.len() != 1 {
            return Err(io::Error::other(format!(
                "selected text differs: expected {expected:?}; encoded actual {actual:?}"
            )));
        }
        Ok(())
    }

    fn release_and_stop(&mut self, expected_prompt: &str) -> io::Result<()> {
        // Composer scenarios may deliberately leave a multiline recalled draft.
        // Escape is the actual explicit clear action, not a test-only setter.
        if self.send_key.is_some() {
            self.send(b"\x1b")?;
            self.frame(0, |screen| {
                screen
                    .composer_rows()
                    .iter()
                    .all(|row| screen.lines()[*row].is_empty())
            })?;
        }
        self.control("release")?;
        self.command("/view follow", "Turn completed")?;
        let completed = self.snapshot()?;
        let mut expected_assistant = Vec::new();
        if self.recorded_tool {
            expected_assistant.push(RECORDED_ASSISTANT.to_owned());
        }
        expected_assistant.extend(self.injected_history.iter().cloned());
        expected_assistant.push(format!("{INITIAL}\n{RELEASED}"));
        if completed["assistant_events"] != json!(expected_assistant) {
            return Err(io::Error::other(format!(
                "completed provider output differs from exact released fixture: {completed}"
            )));
        }
        self.control("close")?;
        self.send(b"\x03\x03\x03\x03")?;
        self.finish(false)?;
        let report: Value = serde_json::from_slice(&std::fs::read(&self.final_report)?)?;
        if report["provider_calls"] != 1
            || report["user_events"] != json!([expected_prompt])
            || report != completed
        {
            return Err(io::Error::other(format!(
                "unexpected final provider admission: {report}"
            )));
        }
        Lifecycle::from_output(&self.output.bytes()?, 24, 100).assert_restored()
    }

    fn finish(&mut self, abort: bool) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        let mut errors = Vec::new();
        if let Some(control) = self.control.take()
            && let Err(error) = control.get_ref().shutdown(Shutdown::Both)
        {
            if error.kind() == io::ErrorKind::NotConnected {
                // The acknowledged `close` command may already have closed the
                // peer. This is the requested terminal state, not a failed close.
                tracing::debug!(%error, "fixture control socket is already closed");
            } else {
                errors.push(format!("control shutdown: {error}"));
            }
        }
        if abort && let Err(error) = self.killer.kill() {
            errors.push(format!("child kill: {error}"));
        }
        if let Err(error) = self.exited.recv_timeout(DEADLINE) {
            errors.push(format!("child exit notification: {error}"));
            if let Err(error) = self.killer.kill() {
                errors.push(format!("deadline child kill: {error}"));
            }
        }
        drop(self.writer.take());
        if let Some(waiter) = self.waiter.take() {
            match join(waiter, "PTY child waiter") {
                Ok(status) if !abort && !status.success() => {
                    errors.push(format!("child exit {status:?}"));
                }
                Ok(_) => {}
                Err(error) => errors.push(error.to_string()),
            }
        }
        if let Some(reader) = self.reader.take()
            && let Err(error) = join(reader, "PTY output reader")
        {
            errors.push(error.to_string());
        }
        self.finished = true;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")))
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.finish(true)
        {
            eprintln!("retained workspace cleanup: {error}");
        }
    }
}

#[derive(Default)]
struct OutputState {
    bytes: Vec<u8>,
    closed: bool,
    failure: Option<String>,
}
#[derive(Default)]
struct Output {
    state: Mutex<OutputState>,
    changed: Condvar,
}

impl Output {
    fn bytes(&self) -> io::Result<Vec<u8>> {
        self.state
            .lock()
            .map(|state| state.bytes.clone())
            .map_err(|error| io::Error::other(format!("PTY capture lock: {error}")))
    }
    fn wait<T>(
        &self,
        label: &str,
        mut observe: impl FnMut(&[u8]) -> io::Result<Option<T>>,
    ) -> io::Result<T> {
        let deadline = Instant::now() + DEADLINE;
        let mut state = self
            .state
            .lock()
            .map_err(|error| io::Error::other(format!("PTY capture lock: {error}")))?;
        loop {
            if let Some(value) = observe(&state.bytes)? {
                return Ok(value);
            }
            if state.closed || state.failure.is_some() || Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "PTY {label}: closed={}, failure={:?}; stdout/stderr:\n{}",
                    state.closed,
                    state.failure,
                    String::from_utf8_lossy(&state.bytes)
                )));
            }
            state = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| io::Error::other(format!("PTY capture notification: {error}")))?
                .0;
        }
    }
}

fn read_output(mut reader: Box<dyn Read + Send>, output: &Output) -> io::Result<()> {
    let mut bytes = [0_u8; 4096];
    loop {
        let result = reader.read(&mut bytes);
        let mut state = output
            .state
            .lock()
            .map_err(|error| io::Error::other(format!("PTY reader capture lock: {error}")))?;
        match result {
            Ok(0) => {
                state.closed = true;
                drop(state);
                output.changed.notify_all();
                return Ok(());
            }
            Ok(count) => state.bytes.extend_from_slice(&bytes[..count]),
            Err(error) => {
                state.failure = Some(error.to_string());
                drop(state);
                output.changed.notify_all();
                return Err(error);
            }
        }
        drop(state);
        output.changed.notify_all();
    }
}

#[derive(Default)]
struct Copies {
    payloads: Vec<Vec<u8>>,
    error: Option<String>,
}

fn copies(output: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let mut observed = Copies::default();
    Parser::new().advance(&mut observed, output);
    if let Some(error) = observed.error {
        return Err(io::Error::other(error));
    }
    Ok(observed.payloads)
}
impl Perform for Copies {
    fn osc_dispatch(&mut self, params: &[&[u8]], terminated_by_bell: bool) {
        if params.first() == Some(&b"52".as_slice()) {
            if let [b"52", b"c", payload] = params
                && !terminated_by_bell
            {
                self.payloads.push(payload.to_vec());
                return;
            }
            self.error =
                Some("unexpected OSC 52 selection/framing emitted by actual App".to_owned());
        }
    }
}

fn join<T>(thread: JoinHandle<io::Result<T>>, label: &str) -> io::Result<T> {
    thread
        .join()
        .map_err(|payload| panic_error(payload.as_ref(), label))?
}
fn panic_error(payload: &(dyn Any + Send), label: &str) -> io::Error {
    let message = if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        format!("panic payload type {:?}", payload.type_id())
    };
    io::Error::other(format!("{label} panicked: {message}"))
}
