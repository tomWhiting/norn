//! Real CLI launch documents across print, driven RPC and shared interactive assembly.

#[path = "support/mcp_launch_fixture.rs"]
mod fixture;

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;

use clap::Parser;
use norn::config::{McpConfigLayer, McpConfigSource};
use norn::integration::McpChannelPolicy;
use norn_cli::cli::Cli;
use norn_cli::runtime::resolve_invocation;
use serde_json::{Value, json};

use fixture::{
    ACTIVE_MESSAGE, CHAT_ID, DEADLINE, FIXTURE_FLAG, ModelStub, STARTUP_MESSAGE, Sandbox,
    TestResult, channel_arguments, driven,
};

type Scenario = Pin<Box<dyn Future<Output = TestResult> + Send>>;
type NamedScenario = (&'static str, fn() -> Scenario);
const TUI_FLAG: &str = "--norn-tui-launch-fixture";

fn main() -> TestResult {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|argument| argument == FIXTURE_FLAG)
    {
        return fixture::run_mcp(&arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == TUI_FLAG)
    {
        let cli = Cli::try_parse_from(
            std::iter::once("norn").chain(arguments[1..].iter().map(String::as_str)),
        )?;
        std::process::exit(norn_cli::tui::run(&cli) as i32);
    }
    let options = HarnessOptions::parse(&arguments)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let scenarios: [NamedScenario; 6] = [
        (
            "inline_print_launches_disjoint_sources_and_preserves_disk",
            || Box::pin(process_launch(false)),
        ),
        (
            "relative_file_driven_launch_delivers_channels_and_exits_once",
            || Box::pin(process_launch(true)),
        ),
        ("interactive_resolution_preserves_overlay_on_reload", || {
            Box::pin(async { interactive_resolution() })
        }),
        (
            "invalid_inline_driven_returns_run_error_before_launch",
            || Box::pin(invalid_launch(true)),
        ),
        ("invalid_file_print_exits_two_before_launch", || {
            Box::pin(invalid_launch(false))
        }),
        (
            "invalid_tui_entry_exits_two_before_terminal_and_mcp",
            || Box::pin(invalid_tui_launch()),
        ),
    ];
    let mut completed = 0;
    for (name, scenario) in scenarios {
        if !options.selects(name) {
            continue;
        }
        if options.list {
            println!("{name}: test");
            continue;
        }
        println!("running MCP launch fixture {name}");
        runtime.block_on(async { tokio::time::timeout(DEADLINE, scenario()).await })??;
        println!("test {name} ... ok");
        completed += 1;
    }
    if !options.list {
        println!("MCP launch result: {completed} passed");
    }
    Ok(())
}

struct HarnessOptions {
    filter: Option<String>,
    exact: bool,
    list: bool,
    ignored_only: bool,
    skipped: Vec<String>,
}

impl HarnessOptions {
    fn parse(arguments: &[String]) -> TestResult<Self> {
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

    fn selects(&self, name: &str) -> bool {
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

fn launch_documents(
    sandbox: &Sandbox,
    relative_command: bool,
) -> TestResult<(String, String, Vec<u8>)> {
    let saved = serde_json::to_vec(&json!({"mcp_servers": {
        "messages": sandbox.definition("saved-source-must-not-launch")?,
        "disabled": sandbox.definition("disabled-source-must-not-launch")?
    }}))?;
    std::fs::write(sandbox.home.join("settings.json"), &saved)?;
    let mut disabled = sandbox.definition("disabled-source-must-not-launch")?;
    disabled["enabled"] = json!(false);
    let mut messages = sandbox.definition("messages")?;
    if relative_command {
        std::os::unix::fs::symlink(
            std::env::current_exe()?,
            sandbox.work.join("launch-fixture"),
        )?;
        messages["command"] = json!("./launch-fixture");
    }
    let first = json!({"mcpServers": {
        "messages": messages, "disabled": disabled
    }})
    .to_string();
    let second = json!({"mcpServers": {"secondary": sandbox.definition("secondary")?}}).to_string();
    Ok((first, second, saved))
}

async fn process_launch(use_driven: bool) -> TestResult {
    let sandbox = Sandbox::new()?;
    let (first, second, saved) = launch_documents(&sandbox, use_driven)?;
    let document = sandbox.work.join("launch.json");
    let first_argument = if use_driven {
        std::fs::write(&document, &first)?;
        "launch.json".to_owned()
    } else {
        first.clone()
    };
    let model = ModelStub::bind().await?;
    let mut command = sandbox.agent_command(&model.base_url);
    // Start elsewhere to prove relative document resolution follows --working-dir.
    command
        .current_dir(sandbox.root.path())
        .arg("--working-dir")
        .arg(&sandbox.work)
        .args(["--mcp-config", &first_argument, "--mcp-config", &second]);
    let requests = if use_driven {
        channel_arguments(&mut command);
        let (output, requests) = tokio::try_join!(driven(&mut command), model.serve())?;
        assert!(
            output.status.success(),
            "{}\n{}",
            output.response,
            output.stderr
        );
        assert_eq!(output.response["result"]["output"], "launch complete");
        assert_eq!(output.response["result"]["stop"]["reason"], "completed");
        assert_channel_notifications(&output.notifications);
        assert_channel_requests(&requests)?;
        assert_eq!(std::fs::read(&document)?, first.as_bytes());
        requests
    } else {
        command
            .args(["--print", "-f", "json", "Use the room reply tool"])
            .stdin(Stdio::null());
        let execution = async {
            let output = command.output().await?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(result["output"], "launch complete");
            assert_eq!(result["stop"]["reason"], "completed");
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        };
        let ((), requests) = tokio::try_join!(execution, model.serve())?;
        assert!(channel_frames(&requests[0])?.is_empty());
        requests
    };
    assert_eq!(requests.len(), 2);
    let tools = requests[0]["tools"]
        .as_array()
        .ok_or("provider tools missing")?;
    assert!(tools.iter().any(|tool| {
        tool["function"]["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("mcp_secondary_reply_"))
    }));
    assert!(!tools.iter().any(|tool| {
        tool["function"]["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("mcp_disabled_"))
    }));
    let messages = requests[1]["messages"]
        .as_array()
        .ok_or("provider messages missing")?;
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("fixture reply accepted"))
    }));
    sandbox.assert_launch("messages")?;
    sandbox.assert_launch("secondary")?;
    let call: Value = serde_json::from_slice(&std::fs::read(
        sandbox.report("messages").with_extension("call"),
    )?)?;
    assert_eq!(call["chat_id"], CHAT_ID);
    assert!(!sandbox.report("saved-source-must-not-launch").exists());
    assert!(!sandbox.report("disabled-source-must-not-launch").exists());
    assert_eq!(std::fs::read(sandbox.home.join("settings.json"))?, saved);
    Ok(())
}

fn channel_frames(request: &Value) -> TestResult<Vec<&str>> {
    Ok(request["messages"]
        .as_array()
        .ok_or("provider messages missing")?
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .filter(|content| content.starts_with("<channel source=\"messages\""))
        .collect())
}

fn assert_channel_requests(requests: &[Value]) -> TestResult {
    assert_eq!(requests.len(), 2);
    let startup = channel_frames(&requests[0])?;
    assert_eq!(startup.len(), 1);
    assert!(startup[0].contains(STARTUP_MESSAGE));
    let active = channel_frames(&requests[1])?;
    assert_eq!(active.len(), 2);
    assert!(active[1].contains(ACTIVE_MESSAGE));
    for frame in active {
        assert!(!frame.contains(" source=\"spoofed-source\""));
        assert!(frame.contains("chat_id=\"room/42?seat=rust&amp;turn=1\""));
        assert!(frame.contains("<untrusted_channel_metadata key=\"source\">spoofed-source"));
    }
    assert_eq!(requests[1]["model"], "launch-fixture-model");
    Ok(())
}

fn assert_channel_notifications(notifications: &[Value]) {
    let channels: Vec<_> = notifications
        .iter()
        .filter(|notification| {
            notification["method"] == "event/message"
                && notification["params"]["type"] == "mcp_channel"
        })
        .collect();
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0]["params"]["content"], STARTUP_MESSAGE);
    assert_eq!(channels[1]["params"]["content"], ACTIVE_MESSAGE);
    for channel in &channels {
        let params = &channel["params"];
        assert_eq!(params["source"], "messages");
        assert_eq!(params["recipient_id"], params["agent_id"]);
        assert!(
            params["generation"]
                .as_u64()
                .is_some_and(|number| number > 0)
        );
        assert!(params["event_id"].is_string());
        assert!(params["message_id"].is_string());
    }
    assert_ne!(
        channels[0]["params"]["event_id"],
        channels[1]["params"]["event_id"]
    );
}

fn interactive_resolution() -> TestResult {
    let sandbox = Sandbox::new()?;
    let (first, second, saved) = launch_documents(&sandbox, false)?;
    let original = std::env::current_dir()?;
    let result = temp_env::with_vars(
        [
            ("HOME", Some(sandbox.home.as_os_str())),
            ("NORN_HOME", Some(sandbox.home.as_os_str())),
            (
                "NORN_OPENAI_COMPAT_API_KEY",
                Some(std::ffi::OsStr::new("local-test-key")),
            ),
        ],
        || -> TestResult {
            let cli = Cli::try_parse_from([
                "norn",
                "--provider",
                "openai-compatible",
                "--model",
                "launch-fixture-model",
                "--no-session",
                "-c",
                "context_window=96000",
                "--working-dir",
                sandbox
                    .work
                    .to_str()
                    .ok_or("fixture work path is not UTF-8")?,
                "--mcp-config",
                &first,
                "--mcp-config",
                &second,
                "--channel",
                "messages=wake",
                "--channel-max-retained-messages",
                "8",
                "--channel-max-retained-bytes",
                "8192",
                "--channel-overflow",
                "reject-new",
            ])?;
            assert!(!cli.print);
            assert!(cli.protocol.is_none());
            let mut invocation = resolve_invocation(&cli)?;
            assert_eq!(invocation.project_root, sandbox.work.canonicalize()?);
            assert_eq!(invocation.mcp_servers.len(), 3);
            for source in invocation.mcp_servers.iter() {
                assert_eq!(source.source(), McpConfigSource::Cli);
                let snapshot = invocation.mcp_state.snapshot()?;
                let entry = snapshot
                    .get(source.name())
                    .ok_or("resolved source absent from state")?;
                assert_eq!(entry.definition(), source.definition());
                assert_eq!(entry.source(), McpConfigLayer::Cli);
            }
            assert!(
                !invocation
                    .mcp_servers
                    .get("disabled")
                    .ok_or("disabled source missing")?
                    .enabled()
            );
            let channels = invocation
                .channel_config
                .as_ref()
                .ok_or("named wake policy missing")?;
            assert_eq!(
                channels.sources().get("messages"),
                Some(&McpChannelPolicy::Wake)
            );
            let before = invocation.mcp_state.snapshot()?;
            assert!(!invocation.mcp_state.reload_disk()?);
            assert_eq!(invocation.mcp_state.snapshot()?, before);
            assert_eq!(std::fs::read(sandbox.home.join("settings.json"))?, saved);
            let replacement = serde_json::to_vec(&json!({"mcp_servers": {
                "messages": sandbox.definition("changed-saved-must-not-launch")?
            }}))?;
            std::fs::write(sandbox.home.join("settings.json"), &replacement)?;
            assert!(invocation.mcp_state.reload_disk()?);
            assert_eq!(invocation.mcp_state.snapshot()?, before);
            assert_eq!(
                std::fs::read(sandbox.home.join("settings.json"))?,
                replacement
            );
            assert!(
                !sandbox.report("messages").exists(),
                "resolution must not launch a server"
            );
            Ok(())
        },
    );
    std::env::set_current_dir(original)?;
    result
}

async fn invalid_launch(use_driven: bool) -> TestResult {
    let sandbox = Sandbox::new()?;
    let invalid = json!({
        "mcpServers": {"sentinel": sandbox.definition("sentinel")?},
        "unexpected": "inline-secret-must-not-escape"
    })
    .to_string();
    let mut command = sandbox.agent_command("http://127.0.0.1:9/v1");
    if use_driven {
        command.args(["--mcp-config", &invalid]);
        let output = driven(&mut command).await?;
        assert_eq!(output.status.code(), Some(2), "{}", output.stderr);
        assert_eq!(output.response["id"], "run-1");
        assert!(
            output.response.get("error").is_some(),
            "{}",
            output.response
        );
        assert!(output.response.get("result").is_none());
        assert!(output.notifications.is_empty());
        assert!(
            !output
                .response
                .to_string()
                .contains("inline-secret-must-not-escape")
        );
        assert!(!output.stderr.contains("inline-secret-must-not-escape"));
    } else {
        std::fs::write(sandbox.work.join("invalid.json"), &invalid)?;
        command
            .args(["--print", "--mcp-config", "invalid.json", "unused prompt"])
            .stdin(Stdio::null());
        let output = command.output().await?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("inline-secret-must-not-escape"));
    }
    assert!(
        !sandbox.report("sentinel").exists(),
        "invalid launch started the sentinel MCP server"
    );
    Ok(())
}

async fn invalid_tui_launch() -> TestResult {
    let sandbox = Sandbox::new()?;
    let invalid = json!({
        "mcpServers": {"sentinel": sandbox.definition("sentinel")?},
        "unexpected": "inline-secret-must-not-escape"
    })
    .to_string();
    let output = tokio::process::Command::new(std::env::current_exe()?)
        .args([TUI_FLAG, "--mcp-config", &invalid])
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("NORN_HOME", &sandbox.home)
        .env("TERM", "xterm-256color")
        .current_dir(&sandbox.work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("norn: TUI error:"),
        "must exercise the TUI error mapping: {stderr}"
    );
    assert!(!stderr.contains("inline-secret-must-not-escape"));
    assert!(
        output.stdout.is_empty(),
        "invalid startup must precede terminal rendering"
    );
    assert!(!sandbox.report("sentinel").exists());
    Ok(())
}
