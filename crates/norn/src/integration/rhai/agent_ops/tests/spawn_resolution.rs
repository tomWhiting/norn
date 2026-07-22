use std::sync::Arc;

use super::support::{
    TestResult, build_context, build_context_with_provider, require, require_error,
    wait_for_terminal,
};
use crate::integration::rhai::context::build_norn_engine;
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::usage::Usage;
use crate::tool::registry::ToolRegistry;

#[tokio::test(flavor = "multi_thread")]
async fn spawn_agent_variant_without_catalog_is_a_script_error() -> TestResult {
    let ctx = build_context();
    let engine = build_norn_engine(&ctx);
    let error = require_error(
        engine.eval::<crate::integration::rhai::AgentHandle>(
            r#"spawn_agent(#{ task: "t", variant: "explorer" })"#,
        ),
        "a missing variant catalog must be refused",
    )?;
    assert!(
        error
            .to_string()
            .contains("no variant catalog is available")
    );
    assert!(ctx.registry.read().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_agent_reviewer_without_model_names_config_key() -> TestResult {
    let mut ctx = build_context();
    let mut host_registry = ToolRegistry::new();
    let host_tool_ctx = crate::tool::context::ToolContext::empty();
    host_tool_ctx.insert_extension(Arc::new(crate::agent::variants::VariantCatalog::build(
        None,
        &std::env::temp_dir(),
    )?));
    host_registry.set_context(Arc::new(host_tool_ctx));
    ctx.tool_registry = Some(Arc::new(host_registry));

    let engine = build_norn_engine(&ctx);
    let error = require_error(
        engine.eval::<crate::integration::rhai::AgentHandle>(
            r#"spawn_agent(#{ task: "t", variant: "reviewer" })"#,
        ),
        "a reviewer without a model must be refused",
    )?;
    assert!(error.to_string().contains("variants.reviewer.model"));
    assert!(ctx.registry.read().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_agent_variant_resolves_role_and_model() -> TestResult {
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
    let mut ctx = build_context_with_provider(provider);
    let mut host_registry = ToolRegistry::new();
    let host_tool_ctx = crate::tool::context::ToolContext::empty();
    host_tool_ctx.insert_extension(Arc::new(crate::agent::variants::VariantCatalog::build(
        None,
        &std::env::temp_dir(),
    )?));
    host_registry.set_context(Arc::new(host_tool_ctx));
    ctx.tool_registry = Some(Arc::new(host_registry));
    let registry = Arc::clone(&ctx.registry);

    let catalog_model = crate::model_catalog::default_selection().model;
    let handle = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "t", variant: "explorer", model: "{catalog_model}" }})"#
        ))?
    };

    {
        let registry = registry.read();
        let entry = require(
            registry.get(handle.id()),
            "spawned variant must be registered",
        )?;
        assert_eq!(entry.role, "explorer");
        assert_eq!(entry.model, catalog_model);
    }
    wait_for_terminal(&registry, handle.id()).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn script_child_step_does_not_clobber_hosts_live_model_stamp() -> TestResult {
    let host_model = "gpt-5.4-mini";
    let child_model = crate::model_catalog::default_selection().model;
    assert_ne!(host_model, child_model, "test precondition");

    let done = |text: &str| {
        vec![
            ProviderEvent::TextDelta {
                text: text.to_owned(),
            },
            ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            },
        ]
    };
    let provider = Arc::new(MockProvider::new(vec![done("a done"), done("b done")]));
    let mut ctx = build_context_with_provider(provider);
    let mut host_registry = ToolRegistry::new();
    let host_tool_ctx = crate::tool::context::ToolContext::empty();
    host_tool_ctx.insert_extension(Arc::new(crate::agent::variants::VariantCatalog::build(
        None,
        &std::env::temp_dir(),
    )?));
    host_tool_ctx.insert_extension(Arc::new(crate::tools::agent::AgentModel {
        model: host_model.to_owned(),
        reasoning_effort: Some(crate::provider::request::ReasoningEffort::High),
    }));
    let host_shared = Arc::new(host_tool_ctx);
    host_registry.set_context(Arc::clone(&host_shared));
    ctx.tool_registry = Some(Arc::new(host_registry));
    let registry = Arc::clone(&ctx.registry);
    let first = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(&format!(
            r#"spawn_agent(#{{ task: "a", role: "worker", model: "{child_model}" }})"#
        ))?
    };
    wait_for_terminal(&registry, first.id()).await?;

    let live = require(
        host_shared.get_extension::<crate::tools::agent::AgentModel>(),
        "the host's model stamp must stay published",
    )?;
    assert_eq!(live.model, host_model);
    assert_eq!(
        live.reasoning_effort,
        Some(crate::provider::request::ReasoningEffort::High),
    );

    let second = {
        let engine = build_norn_engine(&ctx);
        engine.eval::<crate::integration::rhai::AgentHandle>(
            r#"spawn_agent(#{ task: "b", variant: "explorer" })"#,
        )?
    };
    {
        let registry = registry.read();
        let second_entry = require(registry.get(second.id()), "child B must be registered")?;
        assert_eq!(second_entry.model, host_model);
    }
    wait_for_terminal(&registry, second.id()).await?;
    Ok(())
}
