use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;

use super::*;
use crate::config::{McpConfigState, McpDefinitions};
use crate::error::IntegrationError;
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::integration::mcp_runtime::McpRuntimeServerState;
use crate::integration::{McpClient, McpRuntimeCandidateBuilder, McpToolDef};
use crate::tool::{ToolContext, ToolGeneration, ToolGenerationStore, ToolRegistry};

struct ChangingTools {
    calls: AtomicUsize,
    block_first: AtomicBool,
    fail_next: AtomicBool,
    started: Notify,
    release: Notify,
}

impl ChangingTools {
    fn new(block_first: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            block_first: AtomicBool::new(block_first),
            fail_next: AtomicBool::new(false),
            started: Notify::new(),
            release: Notify::new(),
        })
    }
}

struct RefreshTransport(Arc<ChangingTools>);

#[async_trait]
impl Transport for RefreshTransport {
    async fn request(
        &self,
        _payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        let call = self.0.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 && self.0.block_first.load(Ordering::SeqCst) {
            self.0.started.notify_one();
            self.0.release.notified().await;
        }
        if self.0.fail_next.swap(false, Ordering::SeqCst) {
            return Ok(JsonRpcResponse {
                jsonrpc: Some("2.0".to_owned()),
                id: Some(serde_json::json!(request_id)),
                result: Some(serde_json::json!({"tools": "temporarily-invalid"})),
                error: None,
            });
        }
        Ok(JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: Some(serde_json::json!(request_id)),
            result: Some(serde_json::json!({
                "tools": [{
                    "name": format!("version_{call}"),
                    "description": "refresh probe",
                    "inputSchema": {"type": "object"}
                }]
            })),
            error: None,
        })
    }

    async fn notify(&self, _payload: String) -> Result<(), IntegrationError> {
        Ok(())
    }

    fn supports_protocol_version(&self, _version: &str) -> bool {
        true
    }
}

struct ReconnectBuilder {
    transport: Arc<ChangingTools>,
    calls: AtomicUsize,
}

struct RejectReconnectBuilder;

#[async_trait]
impl McpCandidateBuilder for RejectReconnectBuilder {
    async fn build(
        &self,
        _request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        Err(McpCandidateError::rejected(
            "reconnect fixture requested failure",
        ))
    }
}

#[async_trait]
impl McpCandidateBuilder for ReconnectBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let server =
            request.active_servers().first().cloned().ok_or_else(|| {
                McpCandidateError::rejected("reconnect fixture requires one server")
            })?;
        let client = McpClient::from_transport(
            server.name(),
            Box::new(RefreshTransport(Arc::clone(&self.transport))),
        );
        let tools = client
            .discover_tools()
            .await
            .map_err(McpCandidateError::Integration)?;
        let runtime = Arc::new(crate::integration::McpRuntime::from_test_connected_servers(
            vec![(server, client.with_test_tools(tools))],
        ));
        let generation = ToolGeneration::replacing_dynamic_tools(
            request.previous().as_ref(),
            runtime.proxy_tools(),
            request.revision(),
        )?;
        Ok(McpActivationCandidate::new(Arc::new(generation), runtime))
    }
}

fn definition() -> crate::config::McpServerSettings {
    crate::config::McpServerSettings {
        command: Some("refresh-probe".to_owned()),
        ..crate::config::McpServerSettings::default()
    }
}

fn layers(user: McpDefinitions, shared: McpDefinitions) -> [McpDefinitions; 4] {
    [user, shared, BTreeMap::new(), BTreeMap::new()]
}

struct RefreshHarness {
    _home: tempfile::TempDir,
    _project: tempfile::TempDir,
    handle: McpControlHandle,
    generations: Arc<ToolGenerationStore>,
    runtimes: Arc<crate::integration::McpRuntimeStore>,
    runtime: Arc<crate::integration::McpRuntime>,
    client: Arc<McpClient>,
}

fn harness(
    transport: Arc<ChangingTools>,
    notify_before_spawn: bool,
) -> Result<RefreshHarness, Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let builder = Arc::new(McpRuntimeCandidateBuilder::new(
        project.path().to_path_buf(),
    ));
    harness_with_builder(transport, notify_before_spawn, project, builder)
}

fn harness_with_builder(
    transport: Arc<ChangingTools>,
    notify_before_spawn: bool,
    project: tempfile::TempDir,
    builder: Arc<dyn McpCandidateBuilder>,
) -> Result<RefreshHarness, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().canonicalize()?,
        layers(
            BTreeMap::from([("docs".to_owned(), definition())]),
            BTreeMap::new(),
        ),
        BTreeMap::new(),
    )?;
    let effective = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or("missing effective refresh server")?;
    let client = McpClient::from_transport("docs", Box::new(RefreshTransport(transport)))
        .with_test_tools(vec![McpToolDef {
            name: "version_0".to_owned(),
            description: "initial refresh probe".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }]);
    let runtime = Arc::new(crate::integration::McpRuntime::from_test_connected_servers(
        vec![(effective, client)],
    ));
    let client = runtime
        .clients
        .get("docs")
        .cloned()
        .ok_or("missing connected refresh client")?;
    if notify_before_spawn {
        client.notify_tool_list_changed_for_test()?;
    }
    let mut registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    runtime.register_tools(&mut registry)?;
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let runtimes = Arc::new(crate::integration::McpRuntimeStore::new(
        generations.snapshot(),
        Arc::clone(&runtime),
    ));
    let handle = McpControlHandle::spawn(
        state,
        crate::config::McpApprovalStore::at_root(home.path())?,
        builder,
        Arc::clone(&generations),
        Arc::clone(&runtimes),
    )?;
    Ok(RefreshHarness {
        _home: home,
        _project: project,
        handle,
        generations,
        runtimes,
        runtime,
        client,
    })
}

fn selected_harness_with_failed_reconnect(
    transport: Arc<ChangingTools>,
) -> Result<RefreshHarness, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().canonicalize()?,
        layers(
            BTreeMap::from([
                ("docs".to_owned(), definition()),
                ("hidden".to_owned(), definition()),
            ]),
            BTreeMap::new(),
        ),
        BTreeMap::new(),
    )?;
    let snapshot = state.snapshot()?;
    let docs = snapshot
        .get("docs")
        .cloned()
        .ok_or("missing selected refresh server")?;
    let hidden = snapshot
        .get("hidden")
        .cloned()
        .ok_or("missing unselected refresh server")?;
    let docs_client = McpClient::from_transport("docs", Box::new(RefreshTransport(transport)))
        .with_test_tools(vec![McpToolDef {
            name: "version_0".to_owned(),
            description: "initial refresh probe".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }]);
    let hidden_client = McpClient::from_transport(
        "hidden",
        Box::new(RefreshTransport(ChangingTools::new(false))),
    )
    .with_test_tools(vec![McpToolDef {
        name: "hidden_only".to_owned(),
        description: "unselected survivor".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
    }]);
    let runtime = Arc::new(crate::integration::McpRuntime::from_test_connected_servers(
        vec![(docs, docs_client), (hidden, hidden_client)],
    ));
    let client = runtime
        .clients
        .get("docs")
        .cloned()
        .ok_or("missing connected refresh client")?;
    let mut registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    for tool in runtime.proxy_tools_for_servers(&["docs".to_owned()])? {
        registry.register(tool);
    }
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let runtimes = Arc::new(crate::integration::McpRuntimeStore::new(
        generations.snapshot(),
        Arc::clone(&runtime),
    ));
    let handle = McpControlHandle::spawn(
        state,
        crate::config::McpApprovalStore::at_root(home.path())?,
        Arc::new(RejectReconnectBuilder),
        Arc::clone(&generations),
        Arc::clone(&runtimes),
    )?;
    Ok(RefreshHarness {
        _home: home,
        _project: project,
        handle,
        generations,
        runtimes,
        runtime,
        client,
    })
}

async fn wait_for_revision(
    generations: &ToolGenerationStore,
    revision: u64,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while generations.snapshot().revision() < revision {
            tokio::task::yield_now().await;
        }
    })
    .await
}

#[tokio::test]
async fn pre_subscription_change_is_refreshed() -> Result<(), Box<dyn std::error::Error>> {
    let transport = ChangingTools::new(false);
    let harness = harness(Arc::clone(&transport), true)?;
    wait_for_revision(&harness.generations, 1).await?;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(
        harness
            .generations
            .snapshot()
            .names()
            .any(|name| name.contains("version_1"))
    );
    Ok(())
}

#[tokio::test]
async fn change_during_refresh_schedules_the_latest_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let transport = ChangingTools::new(true);
    let harness = harness(Arc::clone(&transport), false)?;
    harness.client.notify_tool_list_changed_for_test()?;
    tokio::time::timeout(Duration::from_secs(2), transport.started.notified()).await?;
    harness.client.notify_tool_list_changed_for_test()?;
    transport.release.notify_one();
    wait_for_revision(&harness.generations, 2).await?;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert!(
        harness
            .generations
            .snapshot()
            .names()
            .any(|name| name.contains("version_2"))
    );
    Ok(())
}

#[tokio::test]
async fn failed_refresh_reconnects_without_another_server_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let transport = ChangingTools::new(false);
    transport.fail_next.store(true, Ordering::SeqCst);
    let project = tempfile::tempdir()?;
    let builder = Arc::new(ReconnectBuilder {
        transport: Arc::clone(&transport),
        calls: AtomicUsize::new(0),
    });
    let harness = harness_with_builder(
        Arc::clone(&transport),
        false,
        project,
        Arc::clone(&builder) as Arc<dyn McpCandidateBuilder>,
    )?;

    harness.client.notify_tool_list_changed_for_test()?;
    wait_for_revision(&harness.generations, 1).await?;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(builder.calls.load(Ordering::SeqCst), 1);
    assert!(!harness.client.is_live());
    assert!(
        harness
            .generations
            .snapshot()
            .names()
            .any(|name| name.contains("version_2"))
    );
    Ok(())
}

#[tokio::test]
async fn failed_refresh_and_reconnect_publish_an_honest_disconnected_surface()
-> Result<(), Box<dyn std::error::Error>> {
    let transport = ChangingTools::new(false);
    transport.fail_next.store(true, Ordering::SeqCst);
    let harness = selected_harness_with_failed_reconnect(Arc::clone(&transport))?;

    harness.client.notify_tool_list_changed_for_test()?;
    wait_for_revision(&harness.generations, 1).await?;

    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(!harness.client.is_live());
    assert!(
        !harness
            .generations
            .snapshot()
            .names()
            .any(|name| name.contains("version_0") || name.contains("hidden_only"))
    );
    let runtime = harness.runtimes.snapshot().runtime();
    assert!(!runtime.server_names().any(|name| name == "docs"));
    assert!(runtime.server_names().any(|name| name == "hidden"));
    let status = runtime
        .reported_server_status("docs")
        .ok_or("disconnected server status was lost")?;
    assert_eq!(status.state(), McpRuntimeServerState::Failed);
    assert!(
        status
            .failure()
            .is_some_and(|failure| failure.contains("reconnect fixture requested failure"))
    );
    Ok(())
}

#[tokio::test]
async fn removed_client_is_not_retained_by_its_watcher() -> Result<(), Box<dyn std::error::Error>> {
    let transport = ChangingTools::new(false);
    let harness = harness(transport, false)?;
    let weak = Arc::downgrade(&harness.client);
    harness.handle.session_disable("docs".to_owned()).await?;
    drop(harness.client);
    drop(harness.runtime);
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn unavailable_approval_store_keeps_direct_control_live()
-> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().canonicalize()?,
        layers(
            BTreeMap::from([("user".to_owned(), definition())]),
            BTreeMap::from([("project".to_owned(), definition())]),
        ),
        BTreeMap::new(),
    )?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let runtimes = Arc::new(crate::integration::McpRuntimeStore::new(
        generations.snapshot(),
        Arc::new(crate::integration::McpRuntime::empty()),
    ));
    let handle = McpControlHandle::spawn(
        state,
        None,
        Arc::new(super::tests::RecordingBuilder::default()),
        generations,
        runtimes,
    )?;
    let status = handle.list().await?;
    assert_eq!(status[0].approval, crate::config::McpApprovalState::Pending);
    assert_eq!(
        status[1].approval,
        crate::config::McpApprovalState::NotRequired
    );
    handle
        .session_add("session".to_owned(), definition())
        .await?;
    let result = handle.approve("project".to_owned()).await;
    assert_eq!(
        result.as_ref().map_err(McpControlError::kind),
        Err(McpControlErrorKind::Approval)
    );
    Ok(())
}
