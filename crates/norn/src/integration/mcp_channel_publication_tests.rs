//! Committed channel failures fence removed sources and preserve control-plane state.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use super::{ClientProtocolState, JsonRpcResponse, McpClient, Transport};
use crate::config::{
    McpApprovalState, McpApprovalStore, McpConfigSource, McpConfigState, McpPersistentMutation,
    McpPersistentScope, McpServerSettings, ResolvedMcpServer,
};
use crate::error::IntegrationError;
use crate::integration::mcp_control::McpControlErrorKind;
use crate::integration::{
    McpActivationCandidate, McpActivationRequest, McpCandidateBuilder, McpCandidateError,
    McpChannelHost, McpChannelInbox, McpChannelLimits, McpChannelOverflow, McpChannelPolicy,
    McpChannelRefusal, McpControlError, McpControlHandle, McpRuntime, McpRuntimeStore,
};
use crate::tool::{ToolContext, ToolGeneration, ToolGenerationStore, ToolRegistry};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct DormantTransport;

#[async_trait]
impl Transport for DormantTransport {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError> {
        Err(IntegrationError::McpError {
            reason: format!("publication fixture received request {request_id}: {payload}"),
        })
    }

    async fn notify(&self, payload: String) -> Result<(), IntegrationError> {
        Err(IntegrationError::McpError {
            reason: format!("publication fixture received notification: {payload}"),
        })
    }

    fn supports_protocol_version(&self, version: &str) -> bool {
        version == super::MCP_PROTOCOL_VERSION
    }
}

fn channel(name: &str, host: &McpChannelHost) -> TestResult<Arc<McpClient>> {
    let mut client = McpClient::from_transport(name, Box::new(DormantTransport));
    let source = host
        .attachment(McpChannelPolicy::Wake, McpChannelOverflow::RejectNew)
        .bind(name.to_owned(), client.instance_id())?;
    source.negotiated()?;
    Arc::get_mut(&mut client.inner)
        .ok_or("fixture client unexpectedly shared before protocol installation")?
        .protocol = Arc::new(ClientProtocolState::with_channel(
        Vec::new(),
        Some(source),
        Some(name.to_owned()),
    ));
    Ok(Arc::new(client))
}

fn receive(client: &McpClient, content: &str) -> TestResult {
    client
        .inner
        .protocol
        .channel_source()
        .ok_or("fixture channel source absent")?
        .receive(serde_json::json!({"content": content}));
    Ok(())
}

fn assert_refused(client: &McpClient, host: &McpChannelHost) -> TestResult {
    let previous = host.status();
    receive(client, "event after removal")?;
    let current = host.status();
    assert_eq!(current.retained_messages, previous.retained_messages);
    assert_eq!(current.rejected, previous.rejected + 1);
    let rejection = current.last_rejection.ok_or("missing retirement refusal")?;
    assert_eq!(rejection.source, client.name());
    assert_eq!(rejection.reason, McpChannelRefusal::Retired);
    Ok(())
}

struct RetiringBuilder {
    clients: BTreeMap<String, Arc<McpClient>>,
    retire_alpha: AtomicBool,
}

#[async_trait]
impl McpCandidateBuilder for RetiringBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        let mut runtime = McpRuntime::empty();
        for server in request.active_servers().iter() {
            let client = self.clients.get(server.name()).ok_or_else(|| {
                McpCandidateError::rejected("publication fixture lacks configured client")
            })?;
            runtime
                .clients
                .insert(server.name().to_owned(), Arc::clone(client));
        }
        if self.retire_alpha.load(Ordering::SeqCst) {
            self.clients
                .get("alpha")
                .ok_or_else(|| McpCandidateError::rejected("fixture alpha absent"))?
                .retire_channel()
                .map_err(|error| IntegrationError::McpError {
                    reason: error.to_string(),
                })?;
        }
        let registry = ToolRegistry::with_context(request.previous().context());
        Ok(McpActivationCandidate::new(
            Arc::new(ToolGeneration::from_registry(&registry, request.revision())),
            Arc::new(runtime),
        )
        .with_channel_lifecycle())
    }
}

struct Harness {
    inbox: McpChannelInbox,
    builder: Arc<RetiringBuilder>,
    control: McpControlHandle,
    runtimes: Arc<McpRuntimeStore>,
    generations: Arc<ToolGenerationStore>,
    approved_beta: ResolvedMcpServer,
}

impl Harness {
    fn new(home: &std::path::Path, project: &std::path::Path) -> TestResult<Self> {
        let mut state = McpConfigState::load(project, BTreeMap::new())?;
        for (name, scope) in [
            ("alpha", McpPersistentScope::User),
            ("beta", McpPersistentScope::SharedProject),
        ] {
            state.persist(
                scope,
                &McpPersistentMutation::Upsert {
                    name: name.to_owned(),
                    definition: McpServerSettings {
                        command: Some(format!("fixture-{name}")),
                        transport: Some("stdio".to_owned()),
                        ..McpServerSettings::default()
                    },
                },
            )?;
        }
        let snapshot = state.snapshot()?;
        let beta = snapshot.get("beta").ok_or("beta definition absent")?;
        let approved_beta = ResolvedMcpServer {
            name: "beta".to_owned(),
            source: McpConfigSource::Project,
            definition: beta.definition().clone(),
            fingerprint: beta.fingerprint().clone(),
        };
        let approvals = McpApprovalStore::at_root(home)?;
        approvals.approve(project, &approved_beta)?;
        let inbox = McpChannelInbox::new(uuid::Uuid::new_v4(), McpChannelLimits::new(8, 4096)?);
        let host = inbox.host();
        let builder = Arc::new(RetiringBuilder {
            clients: BTreeMap::from([
                ("alpha".to_owned(), channel("alpha", &host)?),
                ("beta".to_owned(), channel("beta", &host)?),
            ]),
            retire_alpha: AtomicBool::new(false),
        });
        let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
        let generations = Arc::new(ToolGenerationStore::from_registry(&registry));
        let runtimes = Arc::new(McpRuntimeStore::new(
            generations.snapshot(),
            Arc::new(McpRuntime::empty()),
        ));
        let control = McpControlHandle::spawn(
            state,
            approvals,
            Arc::clone(&builder) as Arc<dyn McpCandidateBuilder>,
            Arc::clone(&generations),
            Arc::clone(&runtimes),
        )?;
        Ok(Self {
            inbox,
            builder,
            control,
            runtimes,
            generations,
            approved_beta,
        })
    }

    async fn initialize_then_retire_during_next_build(&self) -> TestResult {
        assert_eq!(self.control.initialize().await?.revision, 1);
        self.builder.retire_alpha.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn assert_committed_and_fenced(&self, error: &McpControlError) -> TestResult {
        assert_eq!(error.kind(), McpControlErrorKind::CommittedPublication);
        assert!(error.to_string().contains("committed revision 2"));
        assert!(error.to_string().contains("alpha"));
        let snapshot = self.runtimes.snapshot();
        assert_eq!(snapshot.revision(), 2);
        assert!(Arc::ptr_eq(
            &snapshot.generation(),
            &self.generations.snapshot()
        ));
        assert_eq!(
            snapshot.runtime().server_names().collect::<Vec<_>>(),
            ["alpha"]
        );
        assert_refused(
            self.builder
                .clients
                .get("beta")
                .ok_or("beta client absent")?,
            &self.inbox.host(),
        )
    }
}

#[tokio::test]
#[serial_test::serial]
async fn revoked_source_is_fenced_when_reused_source_retires_during_build() -> TestResult {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let harness = Harness::new(home.path(), project.path())?;
        harness.initialize_then_retire_during_next_build().await?;
        let error = harness
            .control
            .revoke("beta".to_owned())
            .await
            .err()
            .ok_or("retired alpha unexpectedly activated")?;
        harness.assert_committed_and_fenced(&error)?;
        let details = harness.control.inspect("beta".to_owned()).await?;
        assert_eq!(details.approval, Some(McpApprovalState::Pending));
        assert!(!details.active);
        assert_eq!(
            McpApprovalStore::at_root(home.path())?
                .state(project.path(), &harness.approved_beta)?,
            McpApprovalState::Pending,
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn session_disable_stays_committed_after_channel_activation_failure() -> TestResult {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let harness = Harness::new(home.path(), project.path())?;
        harness.initialize_then_retire_during_next_build().await?;
        let error = harness
            .control
            .session_disable("beta".to_owned())
            .await
            .err()
            .ok_or("retired alpha unexpectedly activated")?;
        harness.assert_committed_and_fenced(&error)?;
        let details = harness.control.inspect("beta".to_owned()).await?;
        assert!(
            !details
                .inspection
                .effective()
                .ok_or("beta definition absent")?
                .enabled()
        );
        assert!(!details.active);
        let repeated = harness.control.session_disable("beta".to_owned()).await?;
        assert!(!repeated.changed);
        assert_eq!(repeated.revision, 2);
        Ok(())
    })
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn persistent_disable_is_not_rolled_back_after_committed_activation_failure() -> TestResult {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let harness = Harness::new(home.path(), project.path())?;
        harness.initialize_then_retire_during_next_build().await?;
        let error = harness
            .control
            .persistent_disable(McpPersistentScope::SharedProject, "beta".to_owned())
            .await
            .err()
            .ok_or("retired alpha unexpectedly activated")?;
        harness.assert_committed_and_fenced(&error)?;
        let details = harness.control.inspect("beta".to_owned()).await?;
        assert!(
            !details
                .inspection
                .effective()
                .ok_or("beta definition absent")?
                .enabled()
        );
        let reloaded = McpConfigState::load(project.path(), BTreeMap::new())?;
        assert!(
            !reloaded
                .snapshot()?
                .get("beta")
                .ok_or("persisted beta absent")?
                .enabled()
        );
        Ok(())
    })
    .await
}

#[test]
fn every_transition_runs_when_multiple_candidate_sources_have_retired() -> TestResult {
    let mut inbox = McpChannelInbox::new(uuid::Uuid::new_v4(), McpChannelLimits::new(8, 4096)?);
    let host = inbox.host();
    let mut previous = McpRuntime::empty();
    for name in ["alpha", "beta", "charlie", "omega"] {
        previous
            .clients
            .insert(name.to_owned(), channel(name, &host)?);
    }
    previous.publish_channels(&McpRuntime::empty())?;
    let mut candidate = McpRuntime::empty();
    for name in ["alpha", "charlie"] {
        let client = previous.clients.get(name).ok_or("previous client absent")?;
        client.retire_channel()?;
        candidate
            .clients
            .insert(name.to_owned(), Arc::clone(client));
    }
    let zulu = channel("zulu", &host)?;
    receive(&zulu, "staged until publication")?;
    assert!(inbox.try_claim()?.is_none());
    candidate.clients.insert("zulu".to_owned(), zulu);
    let error = candidate
        .publish_channels(&previous)
        .err()
        .ok_or("retired sources activated")?;
    assert!(error.to_string().contains("alpha"));
    assert!(error.to_string().contains("charlie"));
    for name in ["beta", "omega"] {
        assert_refused(
            previous.clients.get(name).ok_or("removed client absent")?,
            &host,
        )?;
    }
    let delivery = inbox
        .try_claim()?
        .ok_or("later healthy source was not activated")?;
    assert_eq!(delivery.message().source, "zulu");
    Ok(())
}
