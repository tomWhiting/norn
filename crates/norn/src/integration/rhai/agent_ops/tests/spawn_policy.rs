use std::sync::Arc;

use super::support::{
    TestResult, build_context, build_context_with_provider, require, require_error,
    wait_for_terminal,
};
use crate::integration::rhai::context::build_norn_engine;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::usage::Usage;

#[tokio::test(flavor = "multi_thread")]
async fn spawn_agent_refused_when_host_depth_exhausted() -> TestResult {
    let mut ctx = build_context();
    ctx.child_policy.delegation.remaining_depth = 0;
    let engine = build_norn_engine(&ctx);

    let error = require_error(
        engine.eval::<crate::integration::rhai::AgentHandle>(
            r#"spawn_agent(#{ task: "t", model: "claude" })"#,
        ),
        "a zero-depth host must be refused",
    )?;
    assert!(error.to_string().contains("delegation depth exhausted"));
    assert!(ctx.registry.read().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_agent_stamps_decremented_grant() -> TestResult {
    let ctx = build_context();
    assert_eq!(ctx.child_policy.delegation.remaining_depth, 2);
    let registry = Arc::clone(&ctx.registry);

    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "t", model: "{catalog_model}" }})"#
        ))?
    };

    let registry = registry.read();
    let entry = require(
        registry.get(handle.id()),
        "spawned child must be registered",
    )?;
    assert_eq!(entry.policy.delegation.remaining_depth, 1);
    assert!(entry.path.starts_with("/spawn/"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_agent_grants_host_loop_config() -> TestResult {
    use crate::agent::child_policy::ChildLoopConfig;

    let provider = Arc::new(MockProvider::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".to_owned(),
        },
        ProviderEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            response_id: None,
        },
    ]]));
    let mut ctx = build_context_with_provider(Arc::<MockProvider>::clone(&provider));
    let granted = ChildLoopConfig {
        step_timeout_secs: Some(300),
        linger_secs: Some(30),
        context_window: None,
    };
    ctx.child_policy.loop_config = Some(granted);
    let registry = Arc::clone(&ctx.registry);

    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "t", model: "{catalog_model}" }})"#
        ))?
    };
    let child_id = handle.id();

    let child_loop_config = {
        let registry = registry.read();
        require(registry.get(child_id), "spawned child must be registered")?
            .policy
            .loop_config
    };
    assert_eq!(child_loop_config, Some(granted));
    wait_for_terminal(&registry, child_id).await?;
    Ok(())
}
