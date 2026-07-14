use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::config::mcp::fingerprint;
use crate::config::{EffectiveMcpServer, McpConfigLayer, McpConfigSnapshot, McpServerSettings};
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::integration::{McpClient, McpToolDef, McpTransport};
use crate::tool::registry::ToolRegistry;
use tokio::sync::Mutex;

struct DormantTransport;

struct SwitchableTransport(Arc<AtomicBool>);

impl McpRuntime {
    pub(crate) fn from_test_clients(clients: Vec<McpClient>) -> Self {
        Self {
            clients: clients
                .into_iter()
                .map(|client| (client.name().to_owned(), Arc::new(client)))
                .collect(),
            failures: BTreeMap::new(),
            statuses: BTreeMap::new(),
        }
    }
}

#[async_trait]
impl Transport for DormantTransport {
    async fn request(
        &self,
        _payload: String,
        _request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Err(IntegrationError::McpError {
            reason: "dormant test transport was invoked".to_owned(),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

#[async_trait]
impl Transport for SwitchableTransport {
    async fn request(
        &self,
        _payload: String,
        _request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Err(IntegrationError::McpError {
            reason: "switchable test transport was invoked".to_owned(),
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }

    fn is_live(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(crate) fn runtime_with_servers(names: &[&str]) -> McpRuntime {
    let clients = names
        .iter()
        .map(|name| {
            McpClient::from_transport(*name, Box::new(DormantTransport)).with_test_tools(vec![
                McpToolDef {
                    name: "echo".to_owned(),
                    description: "fixture".to_owned(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
            ])
        })
        .collect();
    McpRuntime::from_test_clients(clients)
}

#[test]
fn registration_rejects_a_provider_name_already_in_the_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime = runtime_with_servers(&["alpha"]);
    let mut registry = ToolRegistry::new();
    assert_eq!(runtime.register_tools(&mut registry)?, 1);

    let second = runtime.register_tools(&mut registry);

    assert!(matches!(second, Err(IntegrationError::McpError { .. })));
    Ok(())
}

fn definition(command: &str) -> McpServerSettings {
    McpServerSettings {
        command: Some(command.to_owned()),
        ..McpServerSettings::default()
    }
}

fn effective(
    name: &str,
    source: McpConfigLayer,
    definition: McpServerSettings,
) -> Result<EffectiveMcpServer, crate::error::ConfigError> {
    let fingerprint = fingerprint(name, &definition)?;
    Ok(EffectiveMcpServer::new(
        name.to_owned(),
        source,
        definition,
        fingerprint,
    ))
}

fn snapshot(
    servers: Vec<EffectiveMcpServer>,
) -> Result<McpConfigSnapshot, Box<dyn std::error::Error>> {
    let mut by_name = BTreeMap::new();
    for server in servers {
        if by_name.insert(server.name().to_owned(), server).is_some() {
            return Err("duplicate fixture server".into());
        }
    }
    Ok(McpConfigSnapshot::new(by_name))
}

fn connected_client(config: &McpClientConfig) -> McpClient {
    let label = match &config.transport {
        McpTransport::Stdio { command, .. } => command.clone(),
        McpTransport::Http { url } => url.clone(),
    };
    McpClient::from_transport(config.name.clone(), Box::new(DormantTransport)).with_test_tools(
        vec![McpToolDef {
            name: label,
            description: "candidate fixture".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }],
    )
}

fn missing_status(name: &str) -> std::io::Error {
    std::io::Error::other(format!("missing runtime status for '{name}'"))
}

#[tokio::test]
async fn candidate_reports_connected_failed_and_disabled_servers()
-> Result<(), Box<dyn std::error::Error>> {
    let mut disabled = definition("unused");
    disabled.enabled = Some(false);
    let config = snapshot(vec![
        effective("alpha", McpConfigLayer::User, definition("alpha_tool"))?,
        effective(
            "beta",
            McpConfigLayer::SharedProject,
            definition("beta_tool"),
        )?,
        effective("off", McpConfigLayer::Session, disabled)?,
    ])?;
    let runtime = McpRuntime::from_test_clients(Vec::new());

    let candidate = runtime
        .build_candidate_with(&config, Path::new("/project"), |server| async move {
            if server.name == "beta" {
                return Err(IntegrationError::McpError {
                    reason: "fixture connection failed".to_owned(),
                });
            }
            Ok(connected_client(&server))
        })
        .await;

    let statuses: BTreeMap<_, _> = candidate
        .server_statuses()
        .map(|status| (status.name(), status))
        .collect();
    assert_eq!(
        statuses
            .get("alpha")
            .ok_or_else(|| missing_status("alpha"))?
            .state(),
        McpRuntimeServerState::Connected
    );
    assert_eq!(
        statuses
            .get("alpha")
            .ok_or_else(|| missing_status("alpha"))?
            .fingerprint(),
        config
            .get("alpha")
            .ok_or_else(|| missing_status("alpha config"))?
            .fingerprint()
    );
    assert_eq!(
        statuses
            .get("beta")
            .ok_or_else(|| missing_status("beta"))?
            .state(),
        McpRuntimeServerState::Failed
    );
    assert_eq!(
        statuses
            .get("off")
            .ok_or_else(|| missing_status("off"))?
            .state(),
        McpRuntimeServerState::Disabled
    );
    assert_eq!(candidate.failures().count(), 1);
    assert_eq!(candidate.proxy_tools().len(), 1);
    assert!(
        candidate
            .proxy_tools_for_servers(&["off".to_owned()])?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn incremental_candidate_reuses_only_healthy_unchanged_clients()
-> Result<(), Box<dyn std::error::Error>> {
    let initial = snapshot(vec![
        effective("alpha", McpConfigLayer::User, definition("alpha_tool"))?,
        effective("beta", McpConfigLayer::User, definition("beta_old"))?,
        effective("gone", McpConfigLayer::User, definition("gone_tool"))?,
    ])?;
    let empty = McpRuntime::from_test_clients(Vec::new());
    let initial_candidate = empty
        .build_candidate_with(&initial, Path::new("/project"), |server| async move {
            Ok(connected_client(&server))
        })
        .await;
    let active = initial_candidate.into_runtime();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let next = snapshot(vec![
        effective("alpha", McpConfigLayer::Session, definition("alpha_tool"))?,
        effective("beta", McpConfigLayer::User, definition("beta_new"))?,
        effective("delta", McpConfigLayer::Cli, definition("delta_tool"))?,
    ])?;

    let candidate = active
        .build_candidate_with(&next, Path::new("/project"), move |server| {
            let recorded = Arc::clone(&recorded);
            async move {
                recorded.lock().await.push(server.name.clone());
                if server.name == "beta" {
                    return Err(IntegrationError::McpError {
                        reason: "changed definition rejected".to_owned(),
                    });
                }
                Ok(connected_client(&server))
            }
        })
        .await;

    let mut connected = calls.lock().await.clone();
    connected.sort();
    assert_eq!(connected, vec!["beta", "delta"]);
    let names: Vec<_> = candidate
        .runtime()
        .server_names()
        .map(str::to_owned)
        .collect();
    assert_eq!(names, vec!["alpha", "delta"]);
    let tool_names = candidate.runtime().tool_names();
    assert_eq!(tool_names.len(), 2);
    assert!(
        tool_names
            .iter()
            .any(|name| name.starts_with("mcp_alpha_alpha_tool_"))
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name.starts_with("mcp_delta_delta_tool_"))
    );
    let statuses: BTreeMap<_, _> = candidate
        .server_statuses()
        .map(|status| (status.name(), status))
        .collect();
    let alpha = statuses
        .get("alpha")
        .ok_or_else(|| missing_status("alpha"))?;
    assert_eq!(alpha.source(), McpConfigLayer::Session);
    assert_eq!(alpha.state(), McpRuntimeServerState::Connected);
    let beta = statuses.get("beta").ok_or_else(|| missing_status("beta"))?;
    assert_eq!(beta.state(), McpRuntimeServerState::Failed);
    assert!(beta.failure().is_some());
    assert!(!statuses.contains_key("gone"));
    Ok(())
}

#[tokio::test]
async fn dead_matching_client_is_reported_failed_and_reconnected()
-> Result<(), Box<dyn std::error::Error>> {
    let effective = effective("alpha", McpConfigLayer::User, definition("alpha_tool"))?;
    let config = snapshot(vec![effective.clone()])?;
    let live = Arc::new(AtomicBool::new(true));
    let client =
        McpClient::from_transport("alpha", Box::new(SwitchableTransport(Arc::clone(&live))))
            .with_test_tools(vec![McpToolDef {
                name: "old".to_owned(),
                description: "stale fixture".to_owned(),
                input_schema: serde_json::json!({"type": "object"}),
            }]);
    let runtime = McpRuntime::from_test_connected_servers(vec![(effective, client)]);

    live.store(false, Ordering::Release);
    let status = runtime
        .reported_server_status("alpha")
        .ok_or_else(|| missing_status("alpha"))?;
    assert_eq!(status.state(), McpRuntimeServerState::Failed);
    assert!(status.failure().is_some());

    let connections = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&connections);
    let candidate = runtime
        .build_candidate_with(&config, Path::new("/project"), move |server| {
            observed.fetch_add(1, Ordering::SeqCst);
            async move { Ok(connected_client(&server)) }
        })
        .await;
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    assert_eq!(
        candidate
            .runtime()
            .server_status("alpha")
            .ok_or_else(|| missing_status("alpha reconnect"))?
            .state(),
        McpRuntimeServerState::Connected
    );
    Ok(())
}

#[tokio::test]
async fn startup_snapshot_hydration_enables_first_mutation_reuse()
-> Result<(), Box<dyn std::error::Error>> {
    let effective = effective("alpha", McpConfigLayer::User, definition("alpha_tool"))?;
    let config = snapshot(vec![effective])?;
    let config_for_client = config
        .get("alpha")
        .ok_or_else(|| missing_status("alpha config"))?;
    let unannotated = McpRuntime::from_test_clients(vec![connected_client(&McpClientConfig {
        name: config_for_client.name().to_owned(),
        transport: McpTransport::Stdio {
            command: "alpha_tool".to_owned(),
            args: Vec::new(),
        },
        env: std::collections::HashMap::new(),
        headers: std::collections::HashMap::new(),
        working_dir: Some(Path::new("/project").to_path_buf()),
        max_inbound_message_bytes: crate::integration::DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    })]);
    assert!(unannotated.server_status("alpha").is_none());
    let hydrated = unannotated.with_config_snapshot(&config);
    assert_eq!(
        hydrated
            .server_status("alpha")
            .ok_or_else(|| missing_status("hydrated alpha"))?
            .state(),
        McpRuntimeServerState::Connected
    );

    let connections = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&connections);
    let candidate = hydrated
        .build_candidate_with(&config, Path::new("/project"), move |server| {
            observed.fetch_add(1, Ordering::SeqCst);
            async move { Ok(connected_client(&server)) }
        })
        .await;
    assert_eq!(connections.load(Ordering::SeqCst), 0);
    assert_eq!(candidate.runtime().len(), 1);
    Ok(())
}
