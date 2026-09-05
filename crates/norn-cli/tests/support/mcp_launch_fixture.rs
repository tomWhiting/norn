//! Protocol-clean Rust MCP peer, loopback provider and real-process test plumbing.

use std::fmt::Write as FmtWrite;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const FIXTURE_FLAG: &str = "--norn-launch-fixture";
pub const LITERAL_ARGUMENT: &str = "spaces 'quotes' $(literal) ; no shell";
pub const ENV_VALUE: &str = "exact launch value $HOME and spaces";
pub const STARTUP_MESSAGE: &str = "message before initialize response";
pub const ACTIVE_MESSAGE: &str = "/model external-text-only during active tool call";
pub const CHAT_ID: &str = "room/42?seat=rust&turn=1";
// Test-hang diagnostics and fixture frame quotas; never product defaults.
pub const DEADLINE: Duration = Duration::from_secs(30);

pub fn run_mcp(arguments: &[String]) -> TestResult {
    let [report, literal, source] = arguments else {
        return Err("Rust launch fixture requires report, literal argument and source".into());
    };
    assert_eq!(literal, LITERAL_ARGUMENT);
    let environment = std::env::var("NORN_LAUNCH_FIXTURE_VALUE")?;
    assert_eq!(environment, ENV_VALUE);
    let channel_capable = source != "secondary";
    std::fs::write(
        report,
        serde_json::to_vec(&json!({
            "arguments": arguments, "environment": environment,
            "cwd": std::env::current_dir()?, "channel_capable": channel_capable
        }))?,
    )?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        assert_eq!(request["jsonrpc"], "2.0");
        let Some(id) = request.get("id") else {
            assert_eq!(request["method"], "notifications/initialized");
            continue;
        };
        let result = match request["method"].as_str() {
            Some("initialize") => {
                let capabilities = if channel_capable {
                    channel(&mut stdout, STARTUP_MESSAGE)?;
                    json!({"tools": {}, "experimental": {"claude/channel": {}}})
                } else {
                    json!({"tools": {}})
                };
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": capabilities,
                    "serverInfo": {"name": source, "version": "1"},
                    "instructions": "Use reply with the original chat_id."
                })
            }
            Some("tools/list") => json!({"tools": [{
                "name": "reply", "description": "Reply to the fixture room.",
                "inputSchema": {
                    "type": "object", "properties": {
                        "chat_id": {"type": "string"}, "content": {"type": "string"}
                    }, "required": ["chat_id", "content"], "additionalProperties": false
                }
            }]}),
            Some("tools/call") => {
                assert_eq!(request["params"]["name"], "reply");
                assert_eq!(request["params"]["arguments"]["chat_id"], CHAT_ID);
                assert_eq!(
                    request["params"]["arguments"]["content"],
                    "reply from local model"
                );
                std::fs::write(
                    Path::new(report).with_extension("call"),
                    serde_json::to_vec(&request["params"]["arguments"])?,
                )?;
                if channel_capable {
                    channel(&mut stdout, ACTIVE_MESSAGE)?;
                }
                json!({"content": [{"type": "text", "text": "fixture reply accepted"}]})
            }
            Some("ping") => json!({}),
            method => return Err(format!("unexpected fixture MCP method {method:?}").into()),
        };
        write_frame(
            &mut stdout,
            &json!({"jsonrpc": "2.0", "id": id, "result": result}),
        )?;
    }
    Ok(())
}

fn channel(output: &mut impl Write, content: &str) -> TestResult {
    write_frame(
        output,
        &json!({
            "jsonrpc": "2.0", "method": "notifications/claude/channel",
            "params": {"content": content, "meta": {"chat_id": CHAT_ID, "source": "spoofed-source"}}
        }),
    )
}

fn write_frame(output: &mut impl Write, value: &Value) -> TestResult {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

pub struct Sandbox {
    pub root: tempfile::TempDir,
    pub home: PathBuf,
    pub work: PathBuf,
}

impl Sandbox {
    pub fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let work = root.path().join("work");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&work)?;
        Ok(Self { root, home, work })
    }

    pub fn definition(&self, name: &str) -> TestResult<Value> {
        Ok(json!({
            "type": "stdio", "command": std::env::current_exe()?,
            "args": [FIXTURE_FLAG, self.report(name), LITERAL_ARGUMENT, name],
            "env": {"NORN_LAUNCH_FIXTURE_VALUE": ENV_VALUE},
            "max_inbound_message_bytes": 16384
        }))
    }

    pub fn report(&self, name: &str) -> PathBuf {
        self.root.path().join(format!("{name}.json"))
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_norn"));
        command
            .env_clear()
            .env("NORN_HOME", &self.home)
            .env("HOME", &self.home)
            .env("NORN_OPENAI_COMPAT_API_KEY", "local-test-key")
            .current_dir(&self.work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    pub fn agent_command(&self, base_url: &str) -> Command {
        let mut command = self.command();
        command
            .args([
                "--provider",
                "openai-compatible",
                "--model",
                "launch-fixture-model",
                "--no-session",
                "-c",
                "context_window=96000",
                "-c",
                "max_retries=0",
                "-c",
                "retry_max=1",
            ])
            .args(["-c", &format!("base_url={base_url}")]);
        command
    }

    pub fn assert_launch(&self, name: &str) -> TestResult {
        let report: Value = serde_json::from_slice(&std::fs::read(self.report(name))?)?;
        assert_eq!(
            report["arguments"],
            json!([self.report(name), LITERAL_ARGUMENT, name])
        );
        assert_eq!(report["environment"], ENV_VALUE);
        assert_eq!(report["cwd"], json!(self.work.canonicalize()?));
        Ok(())
    }
}

pub fn channel_arguments(command: &mut Command) {
    command.args([
        "--channel",
        "messages=wake",
        "--channel-max-retained-messages",
        "8",
        "--channel-max-retained-bytes",
        "8192",
        "--channel-overflow",
        "reject-new",
    ]);
}

pub struct ModelStub {
    listener: TcpListener,
    pub base_url: String,
}

impl ModelStub {
    pub async fn bind() -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1", listener.local_addr()?);
        Ok(Self { listener, base_url })
    }

    pub async fn serve(self) -> TestResult<Vec<Value>> {
        let mut requests = Vec::new();
        for round in 0..2 {
            let (mut stream, peer) = self.listener.accept().await?;
            assert!(peer.ip().is_loopback());
            let request = read_http(&mut stream).await?;
            let chunks = if round == 0 {
                let name = request["tools"]
                    .as_array()
                    .ok_or("provider tools are absent")?
                    .iter()
                    .filter_map(|tool| tool["function"]["name"].as_str())
                    .find(|name| name.starts_with("mcp_messages_reply_"))
                    .ok_or("CLI did not publish the messages MCP reply tool")?;
                vec![
                    json!({"choices": [{"index": 0, "delta": {"tool_calls": [{
                        "index": 0, "id": "launch-call", "type": "function", "function": {
                            "name": name,
                            "arguments": json!({"chat_id": CHAT_ID, "content": "reply from local model"}).to_string()
                        }
                    }]}, "finish_reason": null}]}),
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                        "usage": {"prompt_tokens": 7, "completion_tokens": 2}}),
                ]
            } else {
                vec![
                    json!({"choices": [{"index": 0, "delta": {"content": "launch complete"}, "finish_reason": null}]}),
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 9, "completion_tokens": 2}}),
                ]
            };
            requests.push(request);
            let mut body = String::new();
            for chunk in chunks {
                write!(body, "data: {chunk}\n\n")?;
            }
            body.push_str("data: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await?;
        }
        Ok(requests)
    }
}

async fn read_http(stream: &mut tokio::net::TcpStream) -> TestResult<Value> {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).await?;
    assert!(first.starts_with("POST /v1/chat/completions "), "{first}");
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Err("model stub received EOF inside HTTP headers".into());
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let mut body = vec![0; length.ok_or("model request omitted content-length")?];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

pub struct DrivenOutput {
    pub response: Value,
    pub notifications: Vec<Value>,
    pub status: ExitStatus,
    pub stderr: String,
}

pub async fn driven(command: &mut Command) -> TestResult<DrivenOutput> {
    command.args(["--protocol", "jsonrpc"]);
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().ok_or("driven child stdin absent")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("driven stdout absent")?).lines();
    let mut errors = child.stderr.take().ok_or("driven stderr absent")?;
    let protocol = async {
        input
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"initialize-1\",\"method\":\"initialize\"}\n")
            .await?;
        let initialized = next_rpc(&mut lines).await?;
        assert_eq!(initialized["id"], "initialize-1");
        assert!(initialized.get("result").is_some(), "{initialized}");
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"run-1\",\"method\":\"run/execute\",\"params\":{\"prompt\":\"Use the room reply tool\"}}\n").await?;
        let mut notifications = Vec::new();
        let response = loop {
            let frame = next_rpc(&mut lines).await?;
            if frame.get("method").is_some() {
                assert!(frame.get("id").is_none());
                notifications.push(frame);
            } else {
                assert_eq!(frame["id"], "run-1");
                break frame;
            }
        };
        // Keep stdin open: successful one-run completion must exit by itself.
        let status = child.wait().await?;
        drop(input);
        assert!(
            lines.next_line().await?.is_none(),
            "unexpected stdout after terminal reply"
        );
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((response, notifications, status))
    };
    let stderr = async {
        let mut text = String::new();
        errors.read_to_string(&mut text).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(text)
    };
    let ((response, notifications, status), stderr) = tokio::try_join!(protocol, stderr)?;
    Ok(DrivenOutput {
        response,
        notifications,
        status,
        stderr,
    })
}

async fn next_rpc(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> TestResult<Value> {
    let line = lines
        .next_line()
        .await?
        .ok_or("driven stdout closed before response")?;
    let value: Value = serde_json::from_str(&line)?;
    assert_eq!(
        value["jsonrpc"], "2.0",
        "every stdout line must be JSON-RPC"
    );
    Ok(value)
}

pub async fn persisted_next_turn_refusal() -> TestResult {
    let sandbox = Sandbox::new()?;
    let settings = serde_json::to_vec(&json!({
        "mcp_servers": {"messages": sandbox.definition("sentinel")?},
        "channels": {
            "default_policy": "next-turn", "max_retained_messages": 8,
            "max_retained_bytes": 8192, "overflow": "reject-new"
        }
    }))?;
    std::fs::write(sandbox.home.join("settings.json"), &settings)?;
    let mut command = sandbox.agent_command("http://127.0.0.1:9/v1");
    let output = driven(&mut command).await?;
    assert_eq!(output.status.code(), Some(2), "{}", output.stderr);
    assert_eq!(output.response["id"], "run-1");
    assert!(output.response.get("result").is_none());
    let message = output.response["error"]["message"]
        .as_str()
        .ok_or("next-turn refusal omitted its RPC error message")?;
    assert!(message.contains("next-turn"), "{message}");
    assert!(message.contains("driven"), "{message}");
    assert!(output.notifications.is_empty());
    assert!(
        !sandbox.report("sentinel").exists(),
        "settings mode refusal must precede MCP startup"
    );
    assert_eq!(std::fs::read(sandbox.home.join("settings.json"))?, settings);
    Ok(())
}

pub struct HarnessOptions {
    filter: Option<String>,
    exact: bool,
    pub list: bool,
    ignored_only: bool,
    skipped: Vec<String>,
}

impl HarnessOptions {
    pub fn parse(arguments: &[String]) -> TestResult<Self> {
        let mut options = Self {
            filter: None,
            exact: false,
            list: false,
            ignored_only: false,
            skipped: Vec::new(),
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--nocapture" | "--show-output" | "--quiet" | "-q" | "--include-ignored" => {}
                "--exact" => options.exact = true,
                "--list" => options.list = true,
                "--ignored" => options.ignored_only = true,
                "--skip" => options
                    .skipped
                    .push(arguments.next().ok_or("--skip requires a name")?.clone()),
                value if value.starts_with('-') => {
                    return Err(format!("unsupported MCP launch harness option {value}").into());
                }
                filter => {
                    if options.filter.replace(filter.to_owned()).is_some() {
                        return Err("MCP launch harness accepts one test-name filter".into());
                    }
                }
            }
        }
        Ok(options)
    }

    pub fn selects(&self, name: &str) -> bool {
        !self.ignored_only
            && !self.skipped.iter().any(|skip| name.contains(skip))
            && self.filter.as_ref().is_none_or(|filter| {
                if self.exact {
                    name == filter
                } else {
                    name.contains(filter)
                }
            })
    }
}
