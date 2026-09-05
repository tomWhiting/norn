//! CLI assembly admits spawn/fork with an operator-explicit same-route window.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use clap::Parser;
use norn::agent::registry::{AgentRegistry, AgentStatus};
use norn::agent::{AgentBuilder, AgentParts};
use norn::agent_loop::runner::ToolExecutor;
use norn::config::NornSettings;
use norn::error::{ProviderError, ToolError};
use norn::model_selection::CatalogBackend;
use norn::profile::Profile;
use norn::provider::events::{ProviderEvent, StopReason};
use norn::provider::mock::MockProvider;
use norn::provider::request::{ProviderRequest, ToolCallKind};
use norn::provider::traits::{Provider, ProviderStream};
use norn::provider::usage::Usage;
use norn::session::events::SessionEvent;
use norn::tool::context::ToolContext;
use norn::tool::envelope::ToolEnvelope;
use norn::tool::output_budget::ToolOutputBudget;
use norn::tool::scheduling::ToolEffect;
use norn::tool::traits::{Tool, ToolOutput};
use norn::tools::agent::AgentHandles;
use norn_cli::cli::Cli;
use norn_cli::config::{CliProfileSource, apply_cli_profile_overrides};
use norn_cli::runtime::{DEFAULT_DELEGATION_DEPTH, builder_from_cli, cli_coordination_envelope};
use parking_lot::Mutex;
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

// Explicit fixture window selected to distinguish its derived tool budget
// from both the no-window budget and the Codex entry with the same model ID.
const OPERATOR_WINDOW: u64 = 96_000;
const MODEL: &str = "gpt-5.5";
const WAIT_LIMIT: Duration = Duration::from_secs(10);

struct RouteProvider {
    backend: CatalogBackend,
    mock: MockProvider,
}

impl Provider for RouteProvider {
    fn model_catalog_backend(&self) -> Option<CatalogBackend> {
        Some(self.backend)
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.mock.stream(request)
    }
}

struct WindowProbe(Arc<Mutex<Option<ToolOutputBudget>>>);

#[async_trait]
impl Tool for WindowProbe {
    fn name(&self) -> &'static str {
        "window_probe"
    }

    fn description(&self) -> &'static str {
        "Observe this child's installed window budget."
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let budget = context.require_extension::<ToolOutputBudget>()?;
        *self.0.lock() = Some(*budget);
        Ok(ToolOutput::success(
            json!({"call_id": envelope.tool_call_id}),
        ))
    }
}

fn tool_response(name: &str, arguments: &Value) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolCallDelta {
            item_id: format!("call-{name}"),
            call_id: None,
            name: Some(name.to_owned()),
            arguments_delta: arguments.to_string(),
            kind: ToolCallKind::Function,
        },
        ProviderEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            response_id: None,
        },
    ]
}

fn scripted_provider(backend: CatalogBackend, tool: &str) -> Arc<RouteProvider> {
    let finish = if tool == "fork" {
        tool_response(
            "structured_output",
            &json!({"response": "done", "requirements": {}}),
        )
    } else {
        vec![ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        }]
    };
    Arc::new(RouteProvider {
        backend,
        mock: MockProvider::new(vec![tool_response("window_probe", &json!({})), finish]),
    })
}

fn cli_builder(
    provider: Arc<dyn Provider>,
    working_dir: &Path,
    from_settings: bool,
    explicit: bool,
) -> TestResult<AgentBuilder> {
    let mut arguments = vec!["norn", "--no-session", "--model", MODEL];
    if explicit && !from_settings {
        arguments.extend(["-c", "context_window=96000"]);
    }
    let cli = Cli::try_parse_from(arguments)?;
    let settings: NornSettings = if explicit && from_settings {
        serde_json::from_value(json!({"agent": {"context_window": OPERATOR_WINDOW}}))?
    } else {
        NornSettings::default()
    };
    let mut profile = Profile {
        model: MODEL.to_owned(),
        tools: Some(vec![
            "spawn_agent".to_owned(),
            "fork".to_owned(),
            "window_probe".to_owned(),
        ]),
        ..Profile::default()
    };
    let applied = apply_cli_profile_overrides(&cli, &mut profile)?;
    Ok(builder_from_cli(
        &cli,
        provider,
        profile,
        CliProfileSource::Operator,
        &settings,
        &applied,
    )?
    .working_dir(working_dir.to_path_buf()))
}

fn arguments_for(tool: &str, model: &str) -> Value {
    if tool == "fork" {
        json!({"request": "Inspect the installed window.", "model": model, "requirements": []})
    } else {
        json!({"task": "Inspect the installed window.", "model": model, "role": "worker"})
    }
}

async fn check_child(
    working_dir: &Path,
    backend: CatalogBackend,
    tool: &str,
    from_settings: bool,
) -> TestResult {
    let provider = scripted_provider(backend, tool);
    let captured = Arc::new(Mutex::new(None));
    let agent_registry = AgentRegistry::shared();
    let envelope = cli_coordination_envelope(DEFAULT_DELEGATION_DEPTH);
    let mut parts = cli_builder(
        Arc::<RouteProvider>::clone(&provider),
        working_dir,
        from_settings,
        true,
    )?
    .agent_registry(Arc::clone(&agent_registry))
    .child_policy(envelope.child_policy.clone())
    .child_result_capacity(envelope.child_result_capacity)
    .inbound_capacity(envelope.child_policy.inbound_capacity)
    .register_root("/root".to_owned(), "lead".to_owned())
    .terminal_reclamation(false)
    .tool(Box::new(WindowProbe(Arc::clone(&captured))))
    .build()?
    .into_parts();
    assert_eq!(parts.config.context_window_limit, Some(OPERATOR_WINDOW));
    assert_eq!(
        parts.model_selection.explicit_window(),
        Some(OPERATOR_WINDOW)
    );

    let before = agent_registry.read().len();
    let rejected = parts
        .registry
        .execute(
            tool,
            "different-model",
            arguments_for(tool, "different-model"),
        )
        .await;
    let message = match rejected {
        Err(error) => error.to_string(),
        Ok(value) => return Err(format!("unrelated model admitted: {value}").into()),
    };
    assert!(
        message.contains(&format!("{}.{}", backend.provider, backend.backend)),
        "{message}"
    );
    assert!(!message.contains("typo"), "{message}");
    assert_eq!(
        agent_registry.read().len(),
        before,
        "rejection must precede reservation"
    );
    assert_eq!(provider.mock.call_count(), 0);

    // An explicit replacement policy without loop_config clears inherited
    // overrides. The operator window must not silently reappear afterwards.
    let mut replacement_args = arguments_for(tool, MODEL);
    replacement_args["child_policy"] =
        serde_json::to_value(envelope.child_policy.grant_for_child(None)?)?;
    assert!(
        parts
            .registry
            .execute(tool, "replacement-policy", replacement_args)
            .await
            .is_err()
    );
    assert_eq!(agent_registry.read().len(), before);
    assert_eq!(provider.mock.call_count(), 0);

    // The settings case also proves that a prepared live selection republishes
    // the explicit policy, rather than retaining the root's startup model.
    let child_model = if from_settings {
        let prepared = parts.model_selection.prepare("live-local-model")?;
        let context = parts.registry.shared_context();
        prepared.apply(
            &mut parts.config,
            &mut parts.loop_context,
            context.as_deref(),
        );
        parts.model = prepared.model().to_owned();
        parts.model_selection = prepared;
        "live-local-model"
    } else {
        MODEL
    };
    let child_arguments = if tool == "spawn_agent" {
        json!({"task": "Inspect the installed window.", "role": "worker"})
    } else {
        arguments_for(tool, child_model)
    };
    let output = parts
        .registry
        .execute(tool, "admitted-child", child_arguments)
        .await?;
    let child_id = uuid::Uuid::parse_str(
        output
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or("child id missing")?,
    )?;
    let child_events = wait_and_close(&parts, child_id).await?;
    assert_probe_succeeded(&child_events, backend, tool);
    assert_eq!(
        *captured.lock(),
        Some(ToolOutputBudget::for_context_window(Some(OPERATOR_WINDOW)))
    );
    let requests = provider.mock.requests()?;
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|request| request.model == child_model));
    Ok(())
}

fn assert_probe_succeeded(events: &[SessionEvent], backend: CatalogBackend, tool: &str) {
    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult {
                tool_name, output, ..
            } if tool_name == "window_probe" => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "{tool} on {}.{} must dispatch exactly one probe: {events:?}",
        backend.provider,
        backend.backend
    );
    assert!(
        results.iter().all(|output| output.get("error").is_none()),
        "{tool} on {}.{} probe failed: {results:?}",
        backend.provider,
        backend.backend
    );
}

async fn wait_and_close(parts: &AgentParts, child_id: uuid::Uuid) -> TestResult<Vec<SessionEvent>> {
    let context = parts
        .registry
        .shared_context()
        .ok_or("root tool context missing")?;
    let handles = context.require_extension::<AgentHandles>()?;
    let mut status = handles.status_rx(child_id).ok_or("child status missing")?;
    tokio::time::timeout(
        WAIT_LIMIT,
        status.wait_for(|value| *value == AgentStatus::Idle || value.is_terminal()),
    )
    .await??;
    assert_ne!(
        *status.borrow(),
        AgentStatus::Failed,
        "scripted child must run its probe successfully"
    );
    let handle = handles.remove(child_id).ok_or("child handle missing")?;
    let child_store = Arc::clone(&handle.event_store);
    handle.cancel.cancel();
    tokio::time::timeout(WAIT_LIMIT, handle.join_handle).await??;
    Ok(child_store.events())
}

#[test]
fn cli_assembly_spawn_and_fork_keep_operator_window_on_api_and_chat() -> TestResult {
    let home = tempfile::tempdir()?;
    let work = tempfile::tempdir()?;
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home.path().as_os_str())),
            ("HOME", Some(home.path().as_os_str())),
        ],
        || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            runtime.block_on(async {
                for backend in [CatalogBackend::RESPONSES, CatalogBackend::CHAT] {
                    let provider = scripted_provider(backend, "spawn_agent");
                    let missing = cli_builder(provider, work.path(), false, false)?.build();
                    let message = match missing {
                        Err(error) => error.to_string(),
                        Ok(agent) => {
                            drop(agent);
                            return Err("missing operator window accepted".into());
                        }
                    };
                    assert!(
                        message.contains(&format!("{}.{}", backend.provider, backend.backend)),
                        "{message}"
                    );
                    assert!(!message.contains("typo"), "{message}");
                    for tool in ["spawn_agent", "fork"] {
                        for from_settings in [false, true] {
                            check_child(work.path(), backend, tool, from_settings).await?;
                        }
                    }
                }
                Ok(())
            })
        },
    )
}
