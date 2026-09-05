//! Public assembly and model-switch policy regressions without provider calls.

use std::sync::Arc;

use norn::agent::AgentBuilder;
use norn::agent_loop::config::ToolExecutor;
use norn::error::{NornError, ProviderError};
use norn::model_selection::CatalogBackend;
use norn::provider::mock::MockProvider;
use norn::provider::request::{ProviderRequest, ReasoningEffort};
use norn::provider::traits::{Provider, ProviderStream};
use norn::tool::output_budget::ToolOutputBudget;

struct UncataloguedProvider;

impl Provider for UncataloguedProvider {
    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::InvalidRequest {
            message: format!(
                "assembly fixture must never call provider for {}",
                request.model
            ),
        })
    }
}

#[test]
fn assembled_selection_preserves_explicit_override_provenance() -> Result<(), NornError> {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let mut parts = AgentBuilder::new(provider)
        .model("astra")
        .context_window_limit(272_000)
        .working_dir(std::env::temp_dir())
        .build()?
        .into_parts();
    assert_eq!(parts.model, "gpt-6-astra");
    assert_eq!(parts.model_selection.backend(), Some(CatalogBackend::CODEX));
    assert_eq!(parts.model_selection.explicit_window(), Some(272_000));
    assert!(parts.model_selection.prepare("codex-spark").is_err());
    assert_eq!(parts.config.context_window_limit, Some(272_000));
    let prepared = parts.model_selection.prepare("sol")?;
    prepared.apply(
        &mut parts.config,
        &mut parts.loop_context,
        parts.registry.shared_context().as_deref(),
    );
    assert_eq!(parts.config.context_window_limit, Some(272_000));
    Ok(())
}

#[test]
fn assembled_derived_selection_rearms_tool_output_budget() -> Result<(), NornError> {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let mut parts = AgentBuilder::new(provider)
        .model("sol")
        .reasoning_effort(ReasoningEffort::Ultra)
        .working_dir(std::env::temp_dir())
        .build()?
        .into_parts();
    assert_eq!(parts.model_selection.explicit_window(), None);
    let prepared = parts.model_selection.prepare("codex-spark")?;
    assert_eq!(prepared.effort(), None);
    prepared.apply(
        &mut parts.config,
        &mut parts.loop_context,
        parts.registry.shared_context().as_deref(),
    );
    assert_eq!(parts.config.context_window_limit, Some(128_000));
    assert_eq!(parts.loop_context.reasoning_effort, None);
    assert_eq!(
        parts
            .registry
            .shared_context()
            .and_then(|context| context.get_extension::<ToolOutputBudget>())
            .as_deref(),
        Some(&ToolOutputBudget::for_context_window(Some(128_000)))
    );
    Ok(())
}

#[test]
fn undeclared_provider_requires_explicit_context_even_for_known_model() -> Result<(), NornError> {
    let missing = AgentBuilder::new(Arc::new(UncataloguedProvider))
        .model("gpt-6-astra")
        .working_dir(std::env::temp_dir())
        .build();
    assert!(matches!(missing, Err(NornError::Config(_))));
    let parts = AgentBuilder::new(Arc::new(UncataloguedProvider))
        .model("gpt-6-astra")
        .context_window_limit(400_000)
        .working_dir(std::env::temp_dir())
        .build()?
        .into_parts();
    assert_eq!(parts.model_selection.backend(), None);
    assert_eq!(parts.config.context_window_limit, Some(400_000));
    assert!(
        parts
            .model_selection
            .prepare("unknown-but-explicit")
            .is_ok()
    );
    Ok(())
}
