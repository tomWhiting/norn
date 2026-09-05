//! Ancestor cancellation reaches Rhai script-spawned children (retry-forever
//! DESIGN D4, commit C3).
//!
//! Script children used to launch with `cancel: None`, so no ancestor —
//! not `close_agent`, not a host Ctrl-C, not a TUI teardown — could stop
//! their run. With the loop's retry policy now unbounded by default, a
//! script child parked in a provider call is an unbounded retry loop with
//! no off switch. The launch site now derives the child's run token from
//! the spawning host's published
//! [`AgentCancellation`](crate::tools::agent::AgentCancellation), exactly
//! as the spawn/fork tools do.

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream;
use tokio_util::sync::CancellationToken;

use super::support::{TestResult, build_context_with_provider, require, wait_for_terminal};
use crate::integration::rhai::context::build_norn_engine;
use crate::provider::events::ProviderEvent;
use crate::provider::traits::{Provider, ProviderStream};
use crate::provider::{ProviderError, ProviderRequest};
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;
use crate::tools::agent::AgentCancellation;

/// A provider whose every call parks in a never-yielding stream, notifying
/// once the run is genuinely mid-flight so the test cancels against an
/// in-flight step rather than a race.
struct ParkingProvider {
    parked: Arc<tokio::sync::Notify>,
}

impl Provider for ParkingProvider {
    // This scripted provider represents the catalogued Codex models used by these tests.
    fn model_catalog_backend(&self) -> Option<crate::model_selection::CatalogBackend> {
        Some(crate::model_selection::CatalogBackend::CODEX)
    }

    fn stream(&self, _request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        self.parked.notify_one();
        Ok(Box::pin(stream::pending::<
            Result<ProviderEvent, ProviderError>,
        >()))
    }
}

/// Cancelling the SPAWNING host's token terminates a script-spawned
/// child's in-flight step: the child's run token is a `child_token` of the
/// host's published [`AgentCancellation`], so the W3.5 cascade reaches
/// Rhai children exactly as it reaches tool-spawned ones.
///
/// Pre-fix the child ran with `cancel: None` and stayed parked in its
/// provider call forever — `wait_for_terminal` timed out.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_the_host_token_cancels_a_script_spawned_child() -> TestResult {
    let parked = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(ParkingProvider {
        parked: Arc::clone(&parked),
    });

    // The host's shared tool context — the surface the launch site reads
    // the spawner's token from, published exactly as `AgentBuilder` does.
    let host_cancel = CancellationToken::new();
    let host_ctx = ToolContext::empty();
    host_ctx.insert_extension(Arc::new(AgentCancellation(host_cancel.clone())));

    let mut ctx = build_context_with_provider(provider);
    ctx.tool_registry = Some(Arc::new(ToolRegistry::with_context(Arc::new(host_ctx))));
    let registry = Arc::clone(&ctx.registry);

    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "park", model: "{catalog_model}" }})"#
        ))?
    };
    let child_id = handle.id();

    // The child's step is genuinely in flight before the cancel.
    tokio::time::timeout(Duration::from_secs(60), parked.notified()).await?;
    require(
        registry.read().get(child_id).map(|entry| entry.status),
        "the script child must be registered before the cancel",
    )?;

    host_cancel.cancel();

    wait_for_terminal(&registry, child_id).await?;
    Ok(())
}

/// A host that publishes NO token still launches script children — with a
/// free-standing token, exactly the documented root boundary — and the
/// child is still cancellable through its own lineage rather than being
/// token-less. Cancelling an unrelated token must not touch it.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_less_host_still_launches_script_children() -> TestResult {
    let parked = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(ParkingProvider {
        parked: Arc::clone(&parked),
    });

    let ctx = build_context_with_provider(provider);
    let registry = Arc::clone(&ctx.registry);

    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "park", model: "{catalog_model}" }})"#
        ))?
    };

    tokio::time::timeout(Duration::from_secs(60), parked.notified()).await?;
    let status = require(
        registry.read().get(handle.id()).map(|entry| entry.status),
        "a token-less host must still register its script child",
    )?;
    assert!(
        !status.is_terminal(),
        "an unrelated cancel must not end the child: {status:?}",
    );
    Ok(())
}
