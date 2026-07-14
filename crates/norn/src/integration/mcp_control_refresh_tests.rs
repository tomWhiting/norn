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
use crate::integration::{McpClient, McpRuntimeCandidateBuilder, McpToolDef};
use crate::tool::{ToolContext, ToolGeneration, ToolGenerationStore, ToolRegistry};

struct ChangingTools {
    calls: AtomicUsize,
    block_first: AtomicBool,
    started: Notify,
    release: Notify,
}

impl ChangingTools {
    fn new(block_first: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            block_first: AtomicBool::new(block_first),
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
    runtime: Arc<crate::integration::McpRuntime>,
    client: Arc<McpClient>,
}

fn harness(
    transport: Arc<ChangingTools>,
    notify_before_spawn: bool,
) -> Result<RefreshHarness, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
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
        Arc::new(McpRuntimeCandidateBuilder::new(
            project.path().to_path_buf(),
        )),
        Arc::clone(&generations),
        runtimes,
    )?;
    Ok(RefreshHarness {
        _home: home,
        _project: project,
        handle,
        generations,
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
    assert_eq!(
        handle.approve("project".to_owned()).await,
        Err(McpControlError::Approval)
    );
    Ok(())
}
