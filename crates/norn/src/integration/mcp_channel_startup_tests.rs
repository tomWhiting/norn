//! Initial activation is explicit, retryable and independent of configuration changes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;

use super::{
    McpActivationCandidate, McpActivationRequest, McpCandidateBuilder, McpCandidateError,
    McpControlHandle,
};
use crate::config::McpConfigState;
use crate::integration::{McpRuntime, McpRuntimeStore};
use crate::tool::{ToolContext, ToolGeneration, ToolGenerationStore, ToolRegistry};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Default)]
struct InitialBuilder {
    calls: AtomicUsize,
    fail: AtomicBool,
}

#[async_trait]
impl McpCandidateBuilder for InitialBuilder {
    async fn build(
        &self,
        request: McpActivationRequest,
    ) -> Result<McpActivationCandidate, McpCandidateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(McpCandidateError::Rejected {
                reason: "initial fixture failure",
            });
        }
        let registry = ToolRegistry::with_context(request.previous().context());
        Ok(McpActivationCandidate::new(
            Arc::new(ToolGeneration::from_registry(&registry, request.revision())),
            request.previous_runtime(),
        ))
    }
}

fn control(
    directory: &std::path::Path,
    builder: Arc<InitialBuilder>,
) -> Result<(McpControlHandle, Arc<McpRuntimeStore>), Box<dyn std::error::Error + Send + Sync>> {
    let state = McpConfigState::from_layers(
        directory.canonicalize()?,
        std::array::from_fn(|_| BTreeMap::new()),
        BTreeMap::new(),
    )?;
    let registry = ToolRegistry::with_context(Arc::new(ToolContext::empty()));
    let generations = Arc::new(ToolGenerationStore::from_registry(&registry));
    let runtimes = Arc::new(McpRuntimeStore::new(
        generations.snapshot(),
        Arc::new(McpRuntime::empty()),
    ));
    let handle = McpControlHandle::spawn(state, None, builder, generations, Arc::clone(&runtimes))?;
    Ok((handle, runtimes))
}

#[tokio::test]
#[serial_test::serial]
async fn initial_activation_runs_once_despite_unchanged_configuration() -> TestResult {
    let home = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let directory = tempfile::tempdir()?;
        let builder = Arc::new(InitialBuilder::default());
        let (handle, runtimes) = control(directory.path(), Arc::clone(&builder))?;
        let unchanged = handle.reload().await?;
        assert!(!unchanged.changed);
        assert_eq!(builder.calls.load(Ordering::SeqCst), 0);

        let initial = handle.initialize().await?;
        assert!(initial.changed);
        assert_eq!(initial.revision, 1);
        assert_eq!(runtimes.snapshot().revision(), initial.revision);
        let repeated = handle.initialize().await?;
        assert!(!repeated.changed);
        assert_eq!(repeated.revision, initial.revision);
        assert_eq!(builder.calls.load(Ordering::SeqCst), 1);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn refused_initial_activation_does_not_publish_and_can_retry() -> TestResult {
    let home = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let directory = tempfile::tempdir()?;
        let builder = Arc::new(InitialBuilder::default());
        builder.fail.store(true, Ordering::SeqCst);
        let (handle, runtimes) = control(directory.path(), Arc::clone(&builder))?;
        assert!(handle.initialize().await.is_err());
        assert_eq!(runtimes.snapshot().revision(), 0);
        builder.fail.store(false, Ordering::SeqCst);
        let retried = handle.initialize().await?;
        assert!(retried.changed);
        assert_eq!(retried.revision, 1);
        assert_eq!(builder.calls.load(Ordering::SeqCst), 2);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
}
