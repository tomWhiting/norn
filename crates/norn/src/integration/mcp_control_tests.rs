use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;

use super::*;
use crate::config::mcp::fingerprint;
use crate::config::{EffectiveMcpServer, McpDefinitions, McpLayerEntry, McpPersistentScope};
use crate::error::IntegrationError;
use crate::integration::McpClient;
use crate::integration::mcp_client::{JsonRpcResponse, Transport};
use crate::tool::{ToolContext, ToolGeneration, ToolGenerationStore, ToolRegistry};

use super::super::mcp_runtime::McpRuntimeServerState;

#[derive(Default)]
pub(super) struct RecordingBuilder {
    fail: AtomicBool,
    activations: Mutex<Vec<Vec<String>>>,
}

struct ConflictBuilder {
    generations: Arc<ToolGenerationStore>,
}

#[async_trait]
impl McpCandidateBuilder for ConflictBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        let registry = ToolRegistry::with_context(request.previous().context());
        let competing = Arc::new(ToolGeneration::from_registry(&registry, request.revision()));
        self.generations
            .publish(competing)
            .map_err(McpCandidateError::Publication)?;
        let candidate = Arc::new(ToolGeneration::from_registry(&registry, request.revision()));
        Ok(McpActivationCandidate::new(
            candidate,
            request.previous_runtime(),
        ))
    }
}

struct BlockingBuilder {
    block_next: AtomicBool,
    started: Notify,
    release: Notify,
}

struct PartialOutcomeBuilder;

struct LiveFixtureTransport;

#[async_trait]
impl Transport for LiveFixtureTransport {
    async fn request(
        &self,
        _payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Ok(JsonRpcResponse {
            jsonrpc: Some("2.0".to_owned()),
            id: Some(serde_json::json!(request_id)),
            result: Some(serde_json::json!({"tools": []})),
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

fn live_fixture_client(name: &str) -> McpClient {
    McpClient::from_transport(name, Box::new(LiveFixtureTransport))
}

#[async_trait]
impl McpCandidateBuilder for PartialOutcomeBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        let active_servers = request.active_servers();
        let statuses = active_servers
            .iter()
            .cloned()
            .map(|server| {
                let state = if server.name() == "broken" {
                    McpRuntimeServerState::Failed
                } else {
                    McpRuntimeServerState::Connected
                };
                let failure = (state == McpRuntimeServerState::Failed)
                    .then(|| "private connection detail".to_owned());
                (server, state, failure)
            })
            .collect();
        let mut runtime = McpRuntime::from_test_statuses(statuses);
        let healthy = active_servers
            .iter()
            .find(|server| server.name() == "healthy")
            .ok_or_else(|| McpCandidateError::rejected("healthy fixture server was missing"))?;
        runtime.clients.insert(
            healthy.name().to_owned(),
            Arc::new(live_fixture_client(healthy.name())),
        );
        let runtime = Arc::new(runtime);
        let registry = ToolRegistry::with_context(request.previous().context());
        let generation = Arc::new(ToolGeneration::from_registry(&registry, request.revision()));
        Ok(McpActivationCandidate::new(generation, runtime))
    }
}

#[async_trait]
impl McpCandidateBuilder for BlockingBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        if self.block_next.swap(false, Ordering::SeqCst) {
            self.started.notify_one();
            self.release.notified().await;
        }
        let registry = ToolRegistry::with_context(request.previous().context());
        let generation = Arc::new(ToolGeneration::from_registry(&registry, request.revision()));
        Ok(McpActivationCandidate::new(
            generation,
            request.previous_runtime(),
        ))
    }
}

impl RecordingBuilder {
    fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    fn activations(&self) -> Vec<Vec<String>> {
        self.activations.lock().clone()
    }
}

#[async_trait]
impl McpCandidateBuilder for RecordingBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(McpCandidateError::rejected(
                "recording fixture requested failure",
            ));
        }
        self.activations.lock().push(
            request
                .active_servers()
                .iter()
                .map(|server| server.name().to_owned())
                .collect(),
        );
        let registry = ToolRegistry::with_context(request.previous().context());
        let generation = Arc::new(ToolGeneration::from_registry(&registry, request.revision()));
        let connected = request
            .active_servers()
            .iter()
            .cloned()
            .map(|server| {
                let client = live_fixture_client(server.name());
                (server, client)
            })
            .collect();
        Ok(McpActivationCandidate::new(
            generation,
            Arc::new(McpRuntime::from_test_connected_servers(connected)),
        ))
    }
}

struct Harness {
    _home: tempfile::TempDir,
    project: tempfile::TempDir,
    handle: McpControlHandle,
    builder: Arc<RecordingBuilder>,
    generations: Arc<ToolGenerationStore>,
    runtimes: Arc<crate::integration::McpRuntimeStore>,
}

fn runtime_store(
    generations: &ToolGenerationStore,
    runtime: Arc<McpRuntime>,
) -> Arc<crate::integration::McpRuntimeStore> {
    Arc::new(crate::integration::McpRuntimeStore::new(
        generations.snapshot(),
        runtime,
    ))
}

impl Harness {
    fn new(persistent: [McpDefinitions; 4]) -> Result<Self, Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let state = McpConfigState::from_layers(
            project.path().canonicalize()?,
            persistent,
            BTreeMap::new(),
        )?;
        let approvals = McpApprovalStore::at_root(home.path())?;
        let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
        let initial = Arc::new(ToolGeneration::from_registry(&registry, 0));
        let generations = Arc::new(ToolGenerationStore::new(initial));
        let builder = Arc::new(RecordingBuilder::default());
        let runtimes = runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        );
        let handle = McpControlHandle::spawn(
            state,
            approvals,
            Arc::clone(&builder) as Arc<dyn McpCandidateBuilder>,
            Arc::clone(&generations),
            Arc::clone(&runtimes),
        )?;
        Ok(Self {
            _home: home,
            project,
            handle,
            builder,
            generations,
            runtimes,
        })
    }

    fn project_path(&self) -> &std::path::Path {
        self.project.path()
    }
}

fn server(command: &str) -> McpServerSettings {
    McpServerSettings {
        command: Some(command.to_owned()),
        ..McpServerSettings::default()
    }
}

fn resolved_project(
    name: &str,
    definition: McpServerSettings,
) -> Result<crate::config::ResolvedMcpServer, crate::error::ConfigError> {
    Ok(crate::config::ResolvedMcpServer {
        name: name.to_owned(),
        source: crate::config::McpConfigSource::Project,
        fingerprint: fingerprint(name, &definition)?,
        definition,
    })
}

fn layers(user: McpDefinitions, shared: McpDefinitions) -> [McpDefinitions; 4] {
    [user, shared, BTreeMap::new(), BTreeMap::new()]
}

#[tokio::test]
async fn pending_project_server_activates_no_tools_until_exact_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let shared = BTreeMap::from([("docs".to_owned(), server("project-server"))]);
    let harness = Harness::new(layers(BTreeMap::new(), shared))?;

    let status = harness.handle.list().await?;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].approval, McpApprovalState::Pending);
    assert!(!status[0].active);
    assert!(harness.builder.activations().is_empty());

    harness
        .handle
        .session_add("direct".to_owned(), server("direct-server"))
        .await?;
    assert_eq!(
        harness.builder.activations(),
        vec![vec!["direct".to_owned()]]
    );

    let approved = harness.handle.approve("docs".to_owned()).await?;
    assert!(approved.changed);
    assert_eq!(approved.revision, 2);
    assert_eq!(
        harness.builder.activations(),
        vec![
            vec!["direct".to_owned()],
            vec!["direct".to_owned(), "docs".to_owned()]
        ]
    );

    let revoked = harness.handle.revoke("docs".to_owned()).await?;
    assert!(revoked.changed);
    assert_eq!(revoked.revision, 3);
    assert_eq!(
        harness.builder.activations().last(),
        Some(&vec!["direct".to_owned()])
    );
    assert_eq!(harness.runtimes.snapshot().revision(), 3);
    assert!(Arc::ptr_eq(
        &harness.runtimes.snapshot().generation(),
        &harness.generations.snapshot()
    ));
    Ok(())
}

#[tokio::test]
async fn workspace_local_state_is_direct_and_never_approval_gated()
-> Result<(), Box<dyn std::error::Error>> {
    let persistent = [
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::from([("docs".to_owned(), server("workspace-server"))]),
        BTreeMap::new(),
    ];
    let harness = Harness::new(persistent)?;

    let initial = harness.handle.inspect("docs".to_owned()).await?;
    assert_eq!(initial.approval, Some(McpApprovalState::NotRequired));
    assert_eq!(
        initial
            .inspection
            .effective()
            .map(EffectiveMcpServer::source),
        Some(McpConfigLayer::WorkspaceLocal)
    );
    assert_eq!(
        harness
            .handle
            .approve("docs".to_owned())
            .await
            .as_ref()
            .map_err(McpControlError::kind),
        Err(McpControlErrorKind::NotSharedProject)
    );

    harness.handle.session_disable("docs".to_owned()).await?;
    harness.handle.session_enable("docs".to_owned()).await?;
    let active = harness.handle.inspect("docs".to_owned()).await?;
    assert_eq!(active.approval, Some(McpApprovalState::NotRequired));
    assert!(active.active);
    Ok(())
}

#[tokio::test]
async fn workspace_local_persist_activates_without_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = Harness::new(layers(BTreeMap::new(), BTreeMap::new()))?;

    let result = harness
        .handle
        .persistent_upsert(
            McpPersistentScope::WorkspaceLocal,
            "docs".to_owned(),
            server("workspace-server"),
        )
        .await?;
    assert!(result.changed);
    let status = harness.handle.inspect("docs".to_owned()).await?;
    assert_eq!(status.approval, Some(McpApprovalState::NotRequired));
    assert_eq!(
        status
            .inspection
            .effective()
            .map(EffectiveMcpServer::source),
        Some(McpConfigLayer::WorkspaceLocal)
    );
    assert!(status.active);

    let settings =
        std::fs::read_to_string(harness.project_path().join(".norn/settings.local.json"))?;
    let value: serde_json::Value = serde_json::from_str(&settings)?;
    assert_eq!(
        value.pointer("/mcp_servers/docs/command"),
        Some(&serde_json::Value::String("workspace-server".to_owned()))
    );
    Ok(())
}

#[tokio::test]
async fn inherited_enable_preserves_project_approval_provenance()
-> Result<(), Box<dyn std::error::Error>> {
    let mut definition = server("project-server");
    definition.enabled = Some(false);
    let shared = BTreeMap::from([("docs".to_owned(), definition)]);
    let harness = Harness::new(layers(BTreeMap::new(), shared))?;

    let enabled = harness.handle.session_enable("docs".to_owned()).await?;
    assert!(enabled.changed);
    let pending = harness.handle.inspect("docs".to_owned()).await?;
    assert_eq!(
        pending
            .inspection
            .effective()
            .map(crate::config::EffectiveMcpServer::source),
        Some(McpConfigLayer::SharedProject)
    );
    assert_eq!(pending.approval, Some(McpApprovalState::Pending));
    assert!(!pending.active);
    assert!(matches!(
        pending.inspection.chain().last(),
        Some(McpLayerEntry::EnabledInherited)
    ));

    harness.handle.approve("docs".to_owned()).await?;
    assert!(harness.handle.inspect("docs".to_owned()).await?.active);

    harness.handle.session_remove("docs".to_owned()).await?;
    let revealed = harness.handle.inspect("docs".to_owned()).await?;
    assert!(
        !revealed
            .inspection
            .effective()
            .is_some_and(EffectiveMcpServer::enabled)
    );
    assert!(!revealed.active);
    Ok(())
}

#[tokio::test]
async fn revoke_succeeds_while_a_session_tombstone_disables_the_project_server()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let definition = server("project-server");
    let resolved = resolved_project("docs", definition.clone())?;
    let state = McpConfigState::from_layers(
        project.path().canonicalize()?,
        layers(
            BTreeMap::new(),
            BTreeMap::from([("docs".to_owned(), definition)]),
        ),
        BTreeMap::new(),
    )?;
    let approvals = McpApprovalStore::at_root(home.path())?;
    approvals.approve(project.path(), &resolved)?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let handle = McpControlHandle::spawn(
        state,
        approvals,
        Arc::new(RecordingBuilder::default()),
        Arc::clone(&generations),
        runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        ),
    )?;

    let disabled = handle.session_disable("docs".to_owned()).await?;
    let revoked = handle.revoke("docs".to_owned()).await?;
    assert!(revoked.changed);
    assert_eq!(revoked.revision, disabled.revision);
    assert_eq!(
        McpApprovalStore::at_root(home.path())?.state(project.path(), &resolved)?,
        McpApprovalState::Pending
    );
    Ok(())
}

#[tokio::test]
async fn failed_candidate_rolls_back_ephemeral_state_and_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = Harness::new(layers(BTreeMap::new(), BTreeMap::new()))?;
    harness.builder.set_fail(true);

    let error = harness
        .handle
        .session_add("live".to_owned(), server("secret-command"))
        .await;
    assert_eq!(
        error.as_ref().map_err(McpControlError::kind),
        Err(McpControlErrorKind::Candidate)
    );
    assert_eq!(harness.generations.snapshot().revision(), 0);
    assert_eq!(harness.runtimes.snapshot().revision(), 0);
    assert!(
        harness
            .handle
            .inspect("live".to_owned())
            .await?
            .inspection
            .effective()
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn configuration_failure_retains_its_typed_source() -> Result<(), Box<dyn std::error::Error>>
{
    let harness = Harness::new(layers(BTreeMap::new(), BTreeMap::new()))?;
    let result = harness
        .handle
        .session_add("invalid server name".to_owned(), server("fixture"))
        .await;
    let error = result.err().ok_or("invalid server name was accepted")?;

    assert_eq!(error.kind(), McpControlErrorKind::Configuration);
    let source = std::error::Error::source(&error).ok_or("control error lost its source")?;
    assert!(source.to_string().contains("invalid config"));
    assert!(error.to_string().contains("invalid config"));
    Ok(())
}

#[tokio::test]
async fn session_disable_enable_and_remove_preserve_whole_entry_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let user = BTreeMap::from([("docs".to_owned(), server("user-server"))]);
    let harness = Harness::new(layers(user, BTreeMap::new()))?;

    harness
        .handle
        .session_add("docs".to_owned(), server("session-server"))
        .await?;
    let session = harness.handle.inspect("docs".to_owned()).await?;
    assert_eq!(
        session
            .inspection
            .effective()
            .map(crate::config::EffectiveMcpServer::source),
        Some(McpConfigLayer::Session)
    );

    harness.handle.session_disable("docs".to_owned()).await?;
    assert!(!harness.handle.inspect("docs".to_owned()).await?.active);
    harness.handle.session_enable("docs".to_owned()).await?;
    let enabled = harness.handle.inspect("docs".to_owned()).await?;
    assert!(enabled.active);
    assert_eq!(enabled.approval, Some(McpApprovalState::NotRequired));
    assert_eq!(
        harness.builder.activations().last(),
        Some(&vec!["docs".to_owned()])
    );

    harness.handle.session_remove("docs".to_owned()).await?;
    let revealed = harness.handle.inspect("docs".to_owned()).await?;
    assert_eq!(
        revealed
            .inspection
            .effective()
            .map(crate::config::EffectiveMcpServer::source),
        Some(McpConfigLayer::User)
    );
    Ok(())
}

#[tokio::test]
async fn failed_persistent_candidate_restores_project_document()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = Harness::new(layers(BTreeMap::new(), BTreeMap::new()))?;
    harness.builder.set_fail(true);

    let result = harness
        .handle
        .persistent_upsert(
            McpPersistentScope::SharedProject,
            "docs".to_owned(),
            server("project-server"),
        )
        .await;
    assert_eq!(
        result.as_ref().map_err(McpControlError::kind),
        Err(McpControlErrorKind::Candidate)
    );
    assert_eq!(harness.generations.snapshot().revision(), 0);

    let settings = std::fs::read_to_string(harness.project_path().join(".norn/settings.json"))?;
    let value: serde_json::Value = serde_json::from_str(&settings)?;
    assert_eq!(value.pointer("/mcp_servers/docs"), None);
    Ok(())
}

#[tokio::test]
async fn status_and_debug_surfaces_do_not_disclose_definition_secrets()
-> Result<(), Box<dyn std::error::Error>> {
    let secret = "sentinel-controller-secret";
    let definition = McpServerSettings {
        command: Some("server".to_owned()),
        env: Some(BTreeMap::from([("TOKEN".to_owned(), secret.to_owned())])),
        ..McpServerSettings::default()
    };
    let user = BTreeMap::from([("docs".to_owned(), definition)]);
    let harness = Harness::new(layers(user, BTreeMap::new()))?;

    let list_debug = format!("{:?}", harness.handle.list().await?);
    let inspect_debug = format!("{:?}", harness.handle.inspect("docs".to_owned()).await?);
    let handle_debug = format!("{:?}", harness.handle);
    assert!(!list_debug.contains(secret));
    assert!(!inspect_debug.contains(secret));
    assert!(!handle_debug.contains(secret));
    Ok(())
}

#[tokio::test]
async fn unchanged_mutation_does_not_publish_a_new_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = Harness::new(layers(BTreeMap::new(), BTreeMap::new()))?;
    harness
        .handle
        .session_add("docs".to_owned(), server("server"))
        .await?;
    let unchanged = harness
        .handle
        .session_add("docs".to_owned(), server("server"))
        .await?;
    assert!(!unchanged.changed);
    assert_eq!(unchanged.revision, 1);
    assert_eq!(harness.builder.activations().len(), 1);
    Ok(())
}

#[test]
fn construction_without_a_tokio_runtime_returns_a_typed_error()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().to_path_buf(),
        layers(BTreeMap::new(), BTreeMap::new()),
        BTreeMap::new(),
    )?;
    let approvals = McpApprovalStore::at_root(home.path())?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let initial = Arc::new(ToolGeneration::from_registry(&registry, 0));
    let generations = Arc::new(ToolGenerationStore::new(initial));
    let result = McpControlHandle::spawn(
        state,
        approvals,
        Arc::new(RecordingBuilder::default()),
        Arc::clone(&generations),
        runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        ),
    );
    assert!(result.is_err_and(|error| error.kind() == McpControlErrorKind::Unavailable));
    Ok(())
}

#[tokio::test]
async fn mailbox_admits_exactly_one_queued_successor() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().to_path_buf(),
        layers(BTreeMap::new(), BTreeMap::new()),
        BTreeMap::new(),
    )?;
    let approvals = McpApprovalStore::at_root(home.path())?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let builder = Arc::new(BlockingBuilder {
        block_next: AtomicBool::new(true),
        started: Notify::new(),
        release: Notify::new(),
    });
    let handle = McpControlHandle::spawn(
        state,
        approvals,
        Arc::clone(&builder) as Arc<dyn McpCandidateBuilder>,
        Arc::clone(&generations),
        runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        ),
    )?;
    let started = builder.started.notified();
    let first_handle = handle.clone();
    let first = tokio::spawn(async move {
        first_handle
            .session_add("first".to_owned(), server("first"))
            .await
    });
    started.await;

    let second_handle = handle.clone();
    let second = tokio::spawn(async move {
        second_handle
            .session_add("second".to_owned(), server("second"))
            .await
    });
    while handle.sender.capacity() != 0 {
        tokio::task::yield_now().await;
    }
    let third = handle.session_add("third".to_owned(), server("third"));
    futures_util::pin_mut!(third);
    assert!(matches!(
        futures_util::poll!(third.as_mut()),
        std::task::Poll::Pending
    ));

    builder.release.notify_one();
    assert!(first.await??.changed);
    assert!(second.await??.changed);
    Ok(())
}

#[tokio::test]
async fn changed_definition_failure_restores_the_previous_project_value()
-> Result<(), Box<dyn std::error::Error>> {
    let shared = BTreeMap::from([("docs".to_owned(), server("old-server"))]);
    let harness = Harness::new(layers(BTreeMap::new(), shared))?;
    harness.builder.set_fail(true);

    let result = harness
        .handle
        .persistent_upsert(
            McpPersistentScope::SharedProject,
            "docs".to_owned(),
            server("new-server"),
        )
        .await;
    assert_eq!(
        result.as_ref().map_err(McpControlError::kind),
        Err(McpControlErrorKind::Candidate)
    );
    let settings = std::fs::read_to_string(harness.project_path().join(".norn/settings.json"))?;
    let value: serde_json::Value = serde_json::from_str(&settings)?;
    assert_eq!(
        value.pointer("/mcp_servers/docs/command"),
        Some(&serde_json::Value::String("old-server".to_owned()))
    );
    assert_eq!(harness.generations.snapshot().revision(), 0);
    Ok(())
}

#[tokio::test]
async fn publication_conflict_rolls_back_session_state() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().to_path_buf(),
        layers(BTreeMap::new(), BTreeMap::new()),
        BTreeMap::new(),
    )?;
    let approvals = McpApprovalStore::at_root(home.path())?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let handle = McpControlHandle::spawn(
        state,
        approvals,
        Arc::new(ConflictBuilder {
            generations: Arc::clone(&generations),
        }),
        Arc::clone(&generations),
        runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        ),
    )?;

    let result = handle
        .session_add("docs".to_owned(), server("server"))
        .await;
    assert_eq!(
        result.as_ref().map_err(McpControlError::kind),
        Err(McpControlErrorKind::Publication)
    );
    assert!(
        handle
            .inspect("docs".to_owned())
            .await?
            .inspection
            .effective()
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn approval_is_compensated_when_generation_publication_conflicts()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let previous = resolved_project("docs", server("old-server"))?;
    let desired_definition = server("new-server");
    let desired = resolved_project("docs", desired_definition.clone())?;
    let shared = BTreeMap::from([("docs".to_owned(), desired_definition)]);
    let state = McpConfigState::from_layers(
        project.path().to_path_buf(),
        layers(BTreeMap::new(), shared),
        BTreeMap::new(),
    )?;
    let approvals = McpApprovalStore::at_root(home.path())?;
    approvals.approve(project.path(), &previous)?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let handle = McpControlHandle::spawn(
        state,
        approvals,
        Arc::new(ConflictBuilder {
            generations: Arc::clone(&generations),
        }),
        Arc::clone(&generations),
        runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        ),
    )?;

    let result = handle.approve("docs".to_owned()).await;
    assert_eq!(
        result.as_ref().map_err(McpControlError::kind),
        Err(McpControlErrorKind::Publication)
    );
    let statuses = handle.list().await?;
    assert_eq!(statuses[0].approval, McpApprovalState::Pending);
    assert!(!statuses[0].active);
    let restored = McpApprovalStore::at_root(home.path())?;
    assert_eq!(
        restored.state(project.path(), &previous)?,
        McpApprovalState::Approved
    );
    assert_eq!(
        restored.state(project.path(), &desired)?,
        McpApprovalState::Pending
    );
    Ok(())
}

#[tokio::test]
async fn runtime_outcome_controls_active_and_failed_status()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let user = BTreeMap::from([("docs".to_owned(), server("server"))]);
    let state = McpConfigState::from_layers(
        project.path().to_path_buf(),
        layers(user, BTreeMap::new()),
        BTreeMap::new(),
    )?;
    let effective = state
        .snapshot()?
        .get("docs")
        .cloned()
        .ok_or("missing effective test server")?;
    let runtime = Arc::new(McpRuntime::from_test_statuses(vec![(
        effective,
        McpRuntimeServerState::Failed,
        Some("sentinel-private-runtime-detail".to_owned()),
    )]));
    let approvals = McpApprovalStore::at_root(home.path())?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let handle = McpControlHandle::spawn(
        state,
        approvals,
        Arc::new(RecordingBuilder::default()),
        Arc::clone(&generations),
        runtime_store(generations.as_ref(), runtime),
    )?;

    let statuses = handle.list().await?;
    assert_eq!(
        statuses[0].runtime_state,
        Some(McpRuntimeServerState::Failed)
    );
    assert!(statuses[0].failure_present);
    assert!(!statuses[0].active);
    assert!(!format!("{statuses:?}").contains("sentinel-private-runtime-detail"));
    let details = handle.inspect("docs".to_owned()).await?;
    assert_eq!(details.runtime_state, Some(McpRuntimeServerState::Failed));
    assert!(details.failure_present);
    Ok(())
}

#[tokio::test]
async fn one_optional_server_failure_publishes_the_healthy_peer()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        project.path().canonicalize()?,
        layers(BTreeMap::new(), BTreeMap::new()),
        BTreeMap::new(),
    )?;
    let approvals = McpApprovalStore::at_root(home.path())?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::new(Arc::new(
        ToolGeneration::from_registry(&registry, 0),
    )));
    let handle = McpControlHandle::spawn(
        state,
        approvals,
        Arc::new(PartialOutcomeBuilder),
        Arc::clone(&generations),
        runtime_store(
            generations.as_ref(),
            Arc::new(McpRuntime::from_test_clients(Vec::new())),
        ),
    )?;

    handle
        .session_add("healthy".to_owned(), server("healthy-server"))
        .await?;
    let result = handle
        .session_add("broken".to_owned(), server("broken-server"))
        .await?;
    assert_eq!(result.revision, 2);
    let statuses = handle.list().await?;
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0].name, "broken");
    assert_eq!(
        statuses[0].runtime_state,
        Some(McpRuntimeServerState::Failed)
    );
    assert!(!statuses[0].active);
    assert_eq!(statuses[1].name, "healthy");
    assert_eq!(
        statuses[1].runtime_state,
        Some(McpRuntimeServerState::Connected)
    );
    assert!(statuses[1].active);
    Ok(())
}
