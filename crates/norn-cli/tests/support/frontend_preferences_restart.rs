//! Owned real-process PTYs and local mock services; waits are push-driven fixture deadlines.

use std::any::Any;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use portable_pty::{
    ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use serde_json::{Value, json};

use crate::mcp_fixture::{DEADLINE, ENV_VALUE, FIXTURE_FLAG, LITERAL_ARGUMENT};
use crate::retained_screen::{self, Lifecycle, PROBE_REPLY, SYNC_QUERY, Screen};

/// Fallible fixture result that can cross the owned process and reader threads.
pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const ROWS: u16 = 48;
const COLS: u16 = 160;

/// Read the actual settings bytes at the scenario's explicit destination.
pub fn document(path: &Path) -> TestResult<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

/// Seed or externally change one isolated fixture document before the next barrier.
pub fn write_document(path: &Path, value: &Value) -> TestResult {
    let parent = path.parent().ok_or("fixture settings path has no parent")?;
    std::fs::create_dir_all(parent)?;
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

/// Append once per actual MCP process, so a same-process frontend action cannot hide a restart.
pub fn record_mcp_start(arguments: &[String]) -> TestResult {
    let path = arguments
        .first()
        .ok_or("MCP startup lacks its report path")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(path).with_extension("starts"))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(())
}

/// Each scenario owns two canonical launch roots, one user home and one mock provider.
pub struct Environment {
    directory: tempfile::TempDir,
    home: PathBuf,
    roots: [PathBuf; 2],
    /// Canonical personal settings destination shared by this scenario's fresh processes.
    pub user: PathBuf,
    provider: ModelServer,
    reports: Vec<PathBuf>,
    process_ids: Vec<u32>,
}

impl Environment {
    /// Allocate isolated user/project roots and bind a local mock provider.
    pub fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let home = directory.path().join("home");
        let roots = [
            directory.path().join("first root"),
            directory.path().join("second root"),
        ];
        for path in [&home, &roots[0], &roots[1]] {
            std::fs::create_dir(path)?;
        }
        let home = home.canonicalize()?;
        let roots = [roots[0].canonicalize()?, roots[1].canonicalize()?];
        Ok(Self {
            user: home.join("settings.json"),
            directory,
            home,
            roots,
            provider: ModelServer::new()?,
            reports: Vec::new(),
            process_ids: Vec::new(),
        })
    }

    /// Workspace-local settings for one of the scenario's two launch roots.
    pub fn local(&self, root: usize) -> PathBuf {
        self.roots[root].join(".norn/settings.local.json")
    }

    /// Shared-project settings for one of the scenario's two launch roots.
    pub fn shared(&self, root: usize) -> PathBuf {
        self.roots[root].join(".norn/settings.json")
    }

    fn spawn(&mut self, root: usize) -> TestResult<App> {
        let report = self
            .directory
            .path()
            .join(format!("mcp-start-{}.json", self.reports.len()));
        let definition = json!({"mcpServers":{"restart":{
            "type":"stdio","command":std::env::current_exe()?,
            "args":[FIXTURE_FLAG,report,LITERAL_ARGUMENT,"secondary"],
            "env":{"NORN_LAUNCH_FIXTURE_VALUE":ENV_VALUE},
            "max_inbound_message_bytes":16384
        }}});
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_norn"));
        command.env_clear();
        command.env("NORN_HOME", &self.home);
        command.env("HOME", &self.home);
        command.env("NORN_OPENAI_COMPAT_API_KEY", "local-test-key");
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.cwd(&self.roots[root]);
        command.args([
            "--provider",
            "openai-compatible",
            "--model",
            "restart-fixture-model",
            "--no-session",
            "-c",
            "context_window=96000",
            "-c",
            "max_retries=0",
            "-c",
            "retry_max=1",
        ]);
        command.args([
            "-c",
            &format!("base_url=http://{}/v1", self.provider.address),
        ]);
        command.args(["--mcp-config", &definition.to_string()]);
        let app = App::spawn(command)?;
        let pid = app.pid.ok_or("actual CLI process has no process ID")?;
        assert_ne!(
            pid,
            std::process::id(),
            "test helper was mistaken for actual CLI"
        );
        assert!(
            !self.process_ids.contains(&pid),
            "restart reused a still-recorded process identity"
        );
        self.process_ids.push(pid);
        self.reports.push(report);
        Ok(app)
    }

    /// Run an actual CLI in a PTY, then observe exit, save completion and terminal restoration.
    /// Failure paths kill/reap the child and join its reader before returning diagnostics.
    pub fn session(
        &mut self,
        root: usize,
        successful_exit: bool,
        exercise: impl FnOnce(&mut App) -> TestResult,
    ) -> TestResult {
        let mut app = self.spawn(root)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.admit()?;
            exercise(&mut app)
        }))
        .map_err(|payload| panic_error(payload.as_ref(), "restart assertions"))
        .and_then(|result| result.map_err(|error| io::Error::other(error.to_string())));
        let exit = if result.is_ok() {
            app.send(b"\x03").and_then(|()| app.finish(false))
        } else {
            app.finish(true)
        };
        let raw = app.output.bytes()?;
        if let Err(error) = result {
            return Err(format!(
                "exercise: {error}; cleanup: {exit:?}; output: {}",
                String::from_utf8_lossy(&raw)
            )
            .into());
        }
        let status = exit.map_err(|error| {
            format!(
                "actual CLI cleanup: {error}; output: {}",
                String::from_utf8_lossy(&raw)
            )
        })?;
        assert_eq!(
            status.success(),
            successful_exit,
            "actual CLI exit {status:?}: {}",
            String::from_utf8_lossy(&raw)
        );
        Lifecycle::from_output(&raw, ROWS, COLS).assert_restored()?;
        let report = self
            .reports
            .last()
            .ok_or("successful startup has no MCP report path")?;
        let observed = document(report)?;
        assert_eq!(observed["cwd"], json!(self.roots[root]));
        assert_eq!(observed["channel_capable"], false);
        Ok(())
    }

    /// Verify malformed preferences fail with exact field/path context before terminal admission.
    pub fn refuse(&mut self, root: usize, field: &str, path: &Path) -> TestResult {
        let mut app = self.spawn(root)?;
        let status = app.finish(false)?;
        let raw = app.output.bytes()?;
        let text = String::from_utf8_lossy(&raw);
        assert!(!status.success(), "malformed preferences accepted: {text}");
        assert!(text.contains(field), "missing field {field}: {text}");
        assert!(
            text.contains(&path.display().to_string()),
            "missing settings path: {text}"
        );
        for forbidden in [SYNC_QUERY, b"\x1b[?1049h", b"\x1b[?2004h"] {
            assert!(
                !raw.windows(forbidden.len())
                    .any(|window| window == forbidden),
                "terminal mode entered before preference refusal: {text}"
            );
        }
        Ok(())
    }

    /// Snapshot actual HTTP provider requests; this does not count provider constructors.
    pub fn requests(&self) -> TestResult<Vec<Value>> {
        Ok(self
            .provider
            .requests
            .lock()
            .map_err(|error| io::Error::other(format!("provider census lock: {error}")))?
            .clone())
    }

    /// Check actual MCP process-start records, including unwanted same-source restarts.
    pub fn assert_mcp_launches(&self, expected: usize) -> TestResult {
        let mut observed = 0;
        for path in &self.reports {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let report: Value = serde_json::from_slice(&bytes)?;
                    assert_eq!(report["environment"], ENV_VALUE);
                    let starts = std::fs::read_to_string(path.with_extension("starts"))?;
                    let ids: Vec<u32> = starts.lines().map(str::parse).collect::<Result<_, _>>()?;
                    assert_eq!(
                        ids.len(),
                        1,
                        "MCP restarted during frontend actions: {ids:?}"
                    );
                    observed += ids.len();
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    assert_eq!(
                        expected,
                        0,
                        "expected MCP startup report absent: {}",
                        path.display()
                    );
                    assert!(
                        !path.with_extension("starts").exists(),
                        "MCP spawned before preference validation"
                    );
                }
                Err(error) => {
                    return Err(format!("MCP startup report {}: {error}", path.display()).into());
                }
            }
        }
        assert_eq!(observed, expected, "MCP startup census differs");
        Ok(())
    }

    /// Stop and join the mock provider, propagating its observed protocol errors.
    pub fn finish(&mut self) -> TestResult {
        self.provider.finish()
    }
}

/// One actual CLI process; no constructor exposes an `AppState` or bypasses driver assembly.
pub struct App {
    /// Process identity reported by the actual spawned executable and validated by its owner.
    pub pid: Option<u32>,
    master: Box<dyn MasterPty + Send>,
    writer: Option<Box<dyn Write + Send>>,
    output: Arc<Output>,
    reader: Option<JoinHandle<io::Result<()>>>,
    waiter: Option<JoinHandle<io::Result<ExitStatus>>>,
    exited: mpsc::Receiver<()>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    geometries: Vec<(u16, u16)>,
    finished: bool,
}

impl App {
    fn spawn(command: CommandBuilder) -> TestResult<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let pid = child.process_id();
        let (sender, exited) = mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            let result = child.wait();
            sender
                .send(())
                .map_err(|error| io::Error::other(format!("CLI exit notification: {error}")))?;
            result
        });
        let output = Arc::new(Output::default());
        let capture = Arc::clone(&output);
        Ok(Self {
            pid,
            master: pair.master,
            writer: Some(writer),
            output,
            reader: Some(std::thread::spawn(move || read_output(reader, &capture))),
            waiter: Some(waiter),
            exited,
            killer,
            geometries: vec![(ROWS, COLS)],
            finished: false,
        })
    }

    fn admit(&mut self) -> TestResult {
        self.output
            .wait("actual terminal capability query", |bytes| {
                Ok(bytes
                    .windows(SYNC_QUERY.len())
                    .any(|window| window == SYNC_QUERY)
                    .then_some(()))
            })?;
        self.send(PROBE_REPLY)?;
        self.frame(0, |_| true)?.assert_composer(1)?;
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("CLI PTY input is closed"))?;
        writer.write_all(bytes)?;
        writer.flush()
    }

    fn frame(&self, after: usize, predicate: impl Fn(&Screen) -> bool) -> io::Result<Screen> {
        self.output.wait("complete CLI frame", |bytes| {
            Ok(retained_screen::latest(bytes, &self.geometries)?
                .filter(|screen| screen.end_offset > after && predicate(screen)))
        })
    }

    /// Wait for text in a valid current retained frame, rather than in old terminal output.
    pub fn wait_contains(&self, expected: &str) -> TestResult<Screen> {
        Ok(self.frame(0, |screen| screen.contains(expected))?)
    }

    /// Enter a local command through terminal input and observe its draft being consumed.
    pub fn command(&mut self, command: &str) -> TestResult<Screen> {
        let after = self.output.bytes()?.len();
        self.send(format!("\x15{command}").as_bytes())?;
        self.frame(after, |screen| {
            let lines = screen.lines();
            screen
                .composer_rows()
                .iter()
                .any(|row| lines.get(*row).is_some_and(|line| line.contains("/view")))
        })?;
        let after = self.output.bytes()?.len();
        self.send(b"\r")?;
        Ok(self.frame(after, |screen| {
            screen.cursor.0 == 0 && screen.assert_composer(1).is_ok()
        })?)
    }

    /// Submit a command, then inspect its expected result in the current retained screen.
    pub fn observe(&mut self, command: &str, expected: &str) -> TestResult<Screen> {
        self.command(command)?;
        self.wait_contains(expected)
    }

    /// Enqueue a toggle key; the scenario's subsequent state readback acknowledges its effect.
    pub fn press(&mut self, bytes: &[u8]) -> TestResult {
        // The two toggle callers follow with /view status and persisted-value
        // assertions. An unchanged visible frame is valid for a hidden layer;
        // ordered readback, rather than a mandatory repaint, acknowledges it.
        self.send(bytes)?;
        Ok(())
    }

    /// Admit one real provider turn and inspect its reply plus the original blue user prompt.
    pub fn submit(&mut self, prompt: &str) -> TestResult {
        let after = self.output.bytes()?.len();
        self.send(format!("\x15{prompt}").as_bytes())?;
        self.frame(after, |screen| {
            screen.lines().iter().any(|line| line.contains(prompt))
        })?;
        self.send(b"\r")?;
        self.command("/view follow")?;
        let screen = self.wait_contains("Turn completed")?;
        assert!(screen.contains("restart fixture answer"));
        let prefix = format!("> {prompt}");
        let row = screen
            .lines()
            .iter()
            .position(|line| line.starts_with(&prefix))
            .ok_or_else(|| {
                io::Error::other(format!(
                    "original user prompt absent: {}",
                    screen.debug_text()
                ))
            })?;
        assert_eq!(screen.foreground_at(0, row), Some([80, 160, 220]));
        screen.assert_composer(1)?;
        Ok(())
    }

    /// Preserve unsent original draft text across narrow and restored-wide PTY geometry.
    pub fn draft_resize(&mut self, draft: &str) -> TestResult {
        let after = self.output.bytes()?.len();
        self.send(draft.as_bytes())?;
        self.frame(after, |screen| {
            screen.lines().iter().any(|line| line.contains(draft))
        })?;
        for (rows, cols) in [(9, 32), (ROWS, COLS)] {
            let after = self.output.bytes()?.len();
            if self.geometries.last() != Some(&(rows, cols)) {
                self.geometries.push((rows, cols));
            }
            self.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            let screen = self.frame(after, |screen| screen.rows == rows && screen.cols == cols)?;
            assert!(
                screen.contains(draft),
                "draft changed on resize: {}",
                screen.debug_text()
            );
            screen.assert_composer(1)?;
        }
        self.send(b"\x15")?;
        Ok(())
    }

    fn finish(&mut self, abort: bool) -> io::Result<ExitStatus> {
        if self.finished {
            return Err(io::Error::other("CLI process already reaped"));
        }
        let mut errors = Vec::new();
        if abort && let Err(error) = self.killer.kill() {
            errors.push(format!("CLI kill: {error}"));
        }
        if let Err(error) = self.exited.recv_timeout(DEADLINE) {
            errors.push(format!("CLI exit deadline: {error}"));
            if let Err(error) = self.killer.kill() {
                errors.push(format!("deadline CLI kill: {error}"));
            }
        }
        drop(self.writer.take());
        let status = if let Some(waiter) = self.waiter.take() {
            match join(waiter, "CLI waiter") {
                Ok(status) => Some(status),
                Err(error) => {
                    errors.push(error.to_string());
                    None
                }
            }
        } else {
            errors.push("CLI waiter missing".to_owned());
            None
        };
        if let Some(reader) = self.reader.take()
            && let Err(error) = join(reader, "CLI output reader")
        {
            errors.push(error.to_string());
        }
        self.finished = true;
        if !errors.is_empty() {
            return Err(io::Error::other(errors.join("; ")));
        }
        status.ok_or_else(|| io::Error::other("CLI exit status missing"))
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.finish(true)
        {
            eprintln!("preference restart cleanup: {error}");
        }
    }
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    closed: bool,
    failure: Option<String>,
}
#[derive(Default)]
struct Output {
    state: Mutex<Capture>,
    changed: Condvar,
}
impl Output {
    fn bytes(&self) -> io::Result<Vec<u8>> {
        self.state
            .lock()
            .map(|state| state.bytes.clone())
            .map_err(|error| io::Error::other(format!("CLI output lock: {error}")))
    }
    fn wait<T>(
        &self,
        label: &str,
        observe: impl Fn(&[u8]) -> io::Result<Option<T>>,
    ) -> io::Result<T> {
        let deadline = Instant::now() + DEADLINE;
        let mut state = self
            .state
            .lock()
            .map_err(|error| io::Error::other(format!("CLI capture lock: {error}")))?;
        loop {
            if let Some(value) = observe(&state.bytes)? {
                return Ok(value);
            }
            if state.closed || state.failure.is_some() || Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "{label}: closed={}, failure={:?}; output: {}",
                    state.closed,
                    state.failure,
                    String::from_utf8_lossy(&state.bytes)
                )));
            }
            state = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| io::Error::other(format!("CLI output notification: {error}")))?
                .0;
        }
    }
}

fn read_output(mut reader: Box<dyn Read + Send>, output: &Output) -> io::Result<()> {
    let mut buffer = [0; 4096];
    loop {
        let result = reader.read(&mut buffer);
        let mut state = output
            .state
            .lock()
            .map_err(|error| io::Error::other(format!("CLI reader capture lock: {error}")))?;
        match result {
            Ok(0) => {
                state.closed = true;
                drop(state);
                output.changed.notify_all();
                return Ok(());
            }
            Ok(count) => state.bytes.extend_from_slice(&buffer[..count]),
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

struct ModelServer {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<Value>>>,
    worker: Option<JoinHandle<TestResult>>,
}
impl ModelServer {
    fn new() -> TestResult<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let census = Arc::clone(&requests);
        let worker = std::thread::spawn(move || serve_model(&listener, &census));
        Ok(Self {
            address,
            requests,
            worker: Some(worker),
        })
    }
    fn finish(&mut self) -> TestResult {
        if let Some(worker) = self.worker.take() {
            let notification = (|| -> io::Result<()> {
                let mut stream = TcpStream::connect(self.address)?;
                stream.set_write_timeout(Some(DEADLINE))?;
                stream.write_all(
                    b"GET /__fixture_stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
                )
            })();
            let result = worker
                .join()
                .map_err(|payload| panic_error(payload.as_ref(), "mock provider"))?;
            notification?;
            result?;
        }
        Ok(())
    }
}
impl Drop for ModelServer {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("restart mock-provider cleanup: {error}");
        }
    }
}

fn serve_model(listener: &TcpListener, requests: &Mutex<Vec<Value>>) -> TestResult {
    loop {
        let (mut stream, peer) = listener.accept()?;
        assert!(peer.ip().is_loopback());
        stream.set_read_timeout(Some(DEADLINE))?;
        stream.set_write_timeout(Some(DEADLINE))?;
        let mut reader = BufReader::new(&mut stream);
        let mut first = String::new();
        reader.read_line(&mut first)?;
        let mut length = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err("mock provider request ended in headers".into());
            }
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>()?);
            }
        }
        if first.starts_with("GET /__fixture_stop ") {
            return Ok(());
        }
        assert!(
            first.starts_with("POST /v1/chat/completions "),
            "unexpected provider route {first}"
        );
        let mut body = vec![0; length.ok_or("mock provider request lacks content-length")?];
        reader.read_exact(&mut body)?;
        let value = serde_json::from_slice(&body)?;
        requests
            .lock()
            .map_err(|error| io::Error::other(format!("mock provider census lock: {error}")))?
            .push(value);
        drop(reader);
        let output = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"restart fixture answer"},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}})
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{output}",
            output.len()
        )?;
        stream.flush()?;
    }
}

fn join<T>(worker: JoinHandle<io::Result<T>>, label: &str) -> io::Result<T> {
    worker
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
    io::Error::other(format!("{label}: {message}"))
}
