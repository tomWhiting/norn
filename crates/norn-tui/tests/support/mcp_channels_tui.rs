//! A real TUI process with a Rust MCP source and explicit test-control connection.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::registry::AgentRegistry;
use norn::agent_loop::{LoopContext, config::AgentLoopConfig};
use norn::integration::{
    McpChannelHost, McpChannelLimits, McpChannelOverflow, McpChannelPolicy, McpClient,
    McpClientConfig, McpTransport,
};
use norn::provider::mock::MockProvider;
use norn::provider::request::MessageRole;
use norn::provider::{
    AgentEventSender, Provider, ProviderError, ProviderEvent, ProviderRequest, ProviderStream,
    StopReason, Usage,
};
use norn::session::{EventStore, events::SessionEvent};
use norn::tool::{ToolContext, ToolEnvelope, ToolRegistry};
use norn_tui::{TuiInputs, input::InputHistory, render::fixed_panel::StatusBar};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::wire::{FIXTURE_ARGUMENT, TestError};
use crate::{CHANNEL_INPUT, DEADLINE, MODEL, SOURCE};

pub async fn run(case: &str, address: &str) -> Result<(), TestError> {
    let policy = match case {
        "wake" | "retry" => McpChannelPolicy::Wake,
        "next_turn" => McpChannelPolicy::NextTurn,
        "hold" => McpChannelPolicy::Hold,
        _ => return Err(format!("unknown TUI channel fixture case {case}").into()),
    };
    let registry = AgentRegistry::shared();
    let reservation = AgentRegistry::reserve(
        &registry,
        "/root".to_owned(),
        "lead".to_owned(),
        MODEL.to_owned(),
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
    let mut context =
        LoopContext::new("Preserve the interactive channel test's system instruction.");
    context.agent_id = Some(root_id);
    let host = context.install_mcp_channel_inbox(McpChannelLimits::new(4, 8192)?)?;
    let client = McpClient::connect_with_channel(
        source_config()?,
        Vec::new(),
        host.attachment(policy, McpChannelOverflow::RejectNew),
    )
    .await?;
    client.activate_channel()?;
    let provider = Arc::new(FixtureProvider {
        refuse_replay_once: AtomicBool::new(case == "retry"),
        inner: MockProvider::new(
            (1..=2)
                .map(|turn| {
                    vec![
                        ProviderEvent::TextDelta {
                            text: format!("channel-fixture-answer-{turn}\n"),
                        },
                        ProviderEvent::Done {
                            stop_reason: StopReason::EndTurn,
                            usage: Usage {
                                input_tokens: 3,
                                output_tokens: 4,
                                ..Usage::default()
                            },
                            response_id: None,
                        },
                    ]
                })
                .collect(),
        ),
    });
    let store = Arc::new(EventStore::new());
    let root_cancel = CancellationToken::new();
    let controls = tokio::spawn(control_loop(
        TcpStream::connect(address).await?,
        client,
        host,
        Arc::clone(&provider),
        Arc::clone(&store),
        root_cancel.clone(),
    ));
    let (sender, agent_event_rx) = tokio::sync::broadcast::channel(16);
    let result = Box::pin(norn_tui::run_app(TuiInputs {
        model_selection: norn::model_selection::ModelRuntime::new(
            provider.model_catalog_backend(),
            MODEL,
            Some(272_000),
            None,
            None,
            std::collections::BTreeMap::new(),
        )?,
        provider: Arc::clone(&provider) as Arc<dyn Provider>,
        executor: Arc::new(ToolRegistry::new()),
        store,
        registry,
        loop_context: context,
        agent_config: AgentLoopConfig::default(),
        model: MODEL.to_owned(),
        tools: Vec::new(),
        history: InputHistory::in_memory(),
        status_bar: StatusBar {
            model_name: MODEL.to_owned(),
            session_name: "channel-fixture-session".to_owned(),
            key_hints: "^C exit".to_owned(),
            ..StatusBar::default()
        },
        root_id,
        initial_prompt: None,
        data_dir: None,
        session_id: None,
        index_lock_deadline: DEADLINE,
        root_event_sender: AgentEventSender::new(sender, root_id, "root".to_owned()),
        agent_event_rx,
        root_inbound: None,
        mcp_control: None,
        root_cancel,
    }))
    .await;
    // run_app cancels the shared token on every exit, allowing the control owner
    // to drop its actual MCP process without an orphaned task or protocol reader.
    controls.await??;
    result?;
    Ok(())
}

fn source_config() -> Result<McpClientConfig, TestError> {
    let path = std::env::current_exe()?;
    let command = path
        .to_str()
        .ok_or("TUI fixture executable path is not UTF-8")?
        .to_owned();
    Ok(McpClientConfig {
        name: SOURCE.to_owned(),
        transport: McpTransport::Stdio {
            command,
            args: vec![FIXTURE_ARGUMENT.to_owned(), "quiet".to_owned()],
        },
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: 16_384,
        request_timeout_ms: None,
    })
}

async fn control_loop(
    stream: TcpStream,
    client: McpClient,
    host: McpChannelHost,
    provider: Arc<FixtureProvider>,
    store: Arc<EventStore>,
    cancel: CancellationToken,
) -> Result<(), TestError> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line?,
            () = cancel.cancelled() => return Ok(()),
        };
        let Some(line) = line else {
            return Err("TUI test control connection closed before exit".into());
        };
        let command: Value = serde_json::from_str(&line)?;
        let response = if command == json!({"action": "emit"}) {
            emit(&client).await?;
            json!({"action": "emitted", "status": host.status()})
        } else if command == json!({"action": "report"}) {
            report(&host, &provider, &store)?
        } else {
            return Err(format!("invalid TUI fixture control command {command}").into());
        };
        let bytes = serde_json::to_vec(&response)?;
        write.write_all(&bytes).await?;
        write.write_all(b"\n").await?;
        write.flush().await?;
    }
}

async fn emit(client: &McpClient) -> Result<(), TestError> {
    let tools = client.proxy_tools();
    let tool = tools
        .first()
        .ok_or("Rust channel fixture reply tool was not discovered")?;
    let output = tool.execute(&ToolEnvelope {
        tool_call_id: Uuid::new_v4().to_string(), tool_name: tool.name().to_owned(),
        model_args: json!({"chat_id": "tui-control-chat", "emit": [{
            "content": CHANNEL_INPUT,
            "meta": {"chat_id": "tui-control-chat", "source": "forged-source", "urgent": "true"},
        }]}),
        metadata: Value::Null,
    }, &ToolContext::empty()).await?;
    if output.is_error() {
        return Err("Rust channel fixture reply failed".into());
    }
    Ok(())
}

fn report(
    host: &McpChannelHost,
    provider: &FixtureProvider,
    store: &EventStore,
) -> Result<Value, TestError> {
    let requests: Vec<Value> = provider
        .inner
        .requests()?
        .iter()
        .map(|request| {
            json!({
                "model": request.model,
                "user_messages": request.messages.iter()
                    .filter(|message| message.role == MessageRole::User)
                    .filter_map(|message| message.content.as_ref()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let user_events: Vec<Value> = store
        .events()
        .into_iter()
        .filter_map(|event| {
            if let SessionEvent::UserMessage { base, content } = event {
                Some(json!({"id": base.id.as_str(), "content": content}))
            } else {
                None
            }
        })
        .collect();
    Ok(
        json!({"action": "report", "requests": requests, "user_events": user_events,
        "status": host.status()}),
    )
}

struct FixtureProvider {
    inner: MockProvider,
    refuse_replay_once: AtomicBool,
}

impl Provider for FixtureProvider {
    fn validate_replay(
        &self,
        messages: &[norn::provider::request::Message],
    ) -> Result<(), ProviderError> {
        self.inner.validate_replay(messages)?;
        if self.refuse_replay_once.swap(false, Ordering::AcqRel) {
            return Err(ProviderError::StreamError {
                reason: "channel fixture replay refused once".to_owned(),
                transient: None,
            });
        }
        Ok(())
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.inner.stream(request)
    }
}
