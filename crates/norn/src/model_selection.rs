//! Backend-bound model policy and prepared live transitions; no provider replacement.

/// Owner-selected operating defaults, separate from factual provider metadata.
pub mod defaults;

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::config::ModelAliasSettings;
use crate::error::ConfigError;
use crate::model_catalog::{self, ModelEntry};
use crate::provider::request::{ReasoningEffort, ServiceTier};
use crate::tool::context::ToolContext;
use crate::tool::output_budget::ToolOutputBudget;
use crate::tools::agent::AgentModel;

/// The catalogue authority advertised by a concrete provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogBackend {
    /// Provider catalogue identifier.
    pub provider: &'static str,
    /// Backend catalogue identifier.
    pub backend: &'static str,
}

impl CatalogBackend {
    /// Codex subscription route; also explicitly selected by Codex test fixtures.
    pub const CODEX: Self = Self {
        provider: "openai",
        backend: "codex_subscription",
    };
    /// Public or compatible Responses API route.
    pub const RESPONSES: Self = Self {
        provider: "openai",
        backend: "responses_api",
    };
    /// OpenAI-compatible Chat Completions route.
    pub const CHAT: Self = Self {
        provider: "openai",
        backend: "openai_compatible_chat",
    };

    /// Look up a canonical model strictly within this backend.
    #[must_use]
    pub fn model(self, model: &str) -> Option<&'static ModelEntry> {
        self.model_in(model_catalog::catalog(), model)
    }
    fn model_in(
        self,
        catalog: &model_catalog::ModelCatalog,
        model: &str,
    ) -> Option<&'static ModelEntry> {
        catalog
            .providers
            .iter()
            .find(|provider| provider.id == self.provider)?
            .backends
            .iter()
            .find(|backend| backend.id == self.backend)?
            .models
            .iter()
            .find(|entry| entry.id == model)
    }
}

/// Alias expansion, retaining any explicit provider-selection request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModelAlias {
    /// Canonical model ID, or deliberate uncatalogued ID.
    pub model: String,
    /// A configured provider profile requested by the alias.
    pub provider_profile: Option<String>,
    /// An API shape requested by the alias.
    pub api_shape: Option<String>,
}

/// Resolve exact built-in IDs, then user aliases, then bundled aliases.
#[must_use]
pub fn resolve_alias(
    model: &str,
    aliases: &BTreeMap<String, ModelAliasSettings>,
) -> ResolvedModelAlias {
    let canonical = model_catalog::resolve_model_alias(model);
    if canonical != Some(model)
        && let Some(alias) = aliases.get(model)
    {
        return ResolvedModelAlias {
            model: model_catalog::resolve_model_alias(alias.model())
                .unwrap_or_else(|| alias.model())
                .to_owned(),
            provider_profile: alias.provider_profile().map(str::to_owned),
            api_shape: alias.api_shape().map(str::to_owned),
        };
    }
    ResolvedModelAlias {
        model: canonical.unwrap_or(model).to_owned(),
        provider_profile: None,
        api_shape: None,
    }
}

/// Whether an explicit effort is declared by the selected backend.
#[must_use]
pub fn supports_effort(
    backend: Option<CatalogBackend>,
    model: &str,
    effort: ReasoningEffort,
) -> bool {
    backend
        .and_then(|route| route.model(model))
        .is_some_and(|entry| entry.supported_reasoning_efforts.contains(&effort.as_str()))
}

/// Whether a service tier is declared by the selected backend.
#[must_use]
pub fn supports_tier(backend: Option<CatalogBackend>, model: &str, tier: ServiceTier) -> bool {
    backend
        .and_then(|route| route.model(model))
        .is_some_and(|entry| {
            entry
                .service_tiers
                .iter()
                .any(|entry| entry.id == tier.as_str())
        })
}

/// Describe a refused effort without claiming unsupported capability when metadata is absent.
#[must_use]
pub fn effort_refusal_message(
    backend: Option<CatalogBackend>,
    model: &str,
    effort: ReasoningEffort,
) -> String {
    let declared = backend
        .and_then(|route| route.model(model))
        .map(|entry| entry.supported_reasoning_efforts.to_vec());
    capability_refusal_message(
        backend,
        model,
        "reasoning effort",
        effort.as_str(),
        declared,
    )
}

/// Describe a refused tier without borrowing another route's capability declarations.
#[must_use]
pub fn tier_refusal_message(
    backend: Option<CatalogBackend>,
    model: &str,
    tier: ServiceTier,
) -> String {
    let declared = backend
        .and_then(|route| route.model(model))
        .map(|entry| entry.service_tiers.iter().map(|tier| tier.id).collect());
    capability_refusal_message(backend, model, "service tier", tier.as_str(), declared)
}

fn capability_refusal_message(
    backend: Option<CatalogBackend>,
    model: &str,
    setting: &str,
    value: &str,
    declared: Option<Vec<&str>>,
) -> String {
    let route = backend_label(backend);
    let Some(declared) = declared else {
        return format!(
            "{route} declares no capability metadata for model '{model}'; explicit {setting} '{value}' is refused until metadata for this route and model is added"
        );
    };
    let values = if declared.is_empty() {
        "no values declared".to_owned()
    } else {
        declared.join(", ")
    };
    format!(
        "{setting} '{value}' is not supported for model '{model}' on {route}; declared values: {values}"
    )
}

/// Resolve an explicit or catalogue-derived context window without borrowing another route.
///
/// # Errors
/// Rejects absent, zero, or over-ceiling windows with the selected model as referent.
pub fn resolve_window(
    backend: Option<CatalogBackend>,
    model: &str,
    explicit: Option<u64>,
) -> Result<u64, ConfigError> {
    let entry = backend.and_then(|route| route.model(model));
    resolve_entry_window(
        model,
        entry,
        explicit.or_else(|| defaults::context_window(backend, model)),
    )
    .map_err(|reason| invalid(format!("{}: {reason}", backend_label(backend))))
}

fn resolve_entry_window(
    model: &str,
    entry: Option<&ModelEntry>,
    explicit: Option<u64>,
) -> Result<u64, String> {
    let window = explicit.or_else(|| entry.map(|entry| entry.context_window)).ok_or_else(|| format!(
        "the selected route declares no capability metadata for model '{model}' and no context window is configured; set agent.context_window (-c context_window=<tokens>); child launches use child_policy.loop_config.context_window"
    ))?;
    if window == 0 {
        return Err(format!(
            "context window for model '{model}' must be greater than zero"
        ));
    }
    if let Some(entry) = entry
        && window > entry.max_context_window
    {
        return Err(format!(
            "configured context window {window} exceeds model '{model}'s maximum of {} on the selected backend; lower or remove the explicit context window",
            entry.max_context_window
        ));
    }
    Ok(window)
}

/// Whether model alias expansion is still required at this assembly boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelInput {
    /// An operator token: expand one user or bundled alias before validation.
    Raw(String),
    /// An identity already resolved by the caller: validate it literally.
    Resolved(String),
}

impl ModelInput {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Raw(model) | Self::Resolved(model) => model,
        }
    }
}

/// Effective compaction policy bound by the owner that assembled the stable prompt.
#[derive(Clone, Copy, Debug)]
enum CompactionPolicy {
    Unbound,
    Disabled,
    Reserve(u64),
}

/// A validated model selection, including the origin of its context policy.
#[derive(Clone, Debug)]
pub struct ModelRuntime {
    backend: Option<CatalogBackend>,
    model: String,
    explicit_window: Option<u64>,
    window: u64,
    effort: Option<ReasoningEffort>,
    tier: Option<ServiceTier>,
    aliases: BTreeMap<String, ModelAliasSettings>,
    provider_profile: Option<String>,
    compaction_policy: CompactionPolicy,
}

impl ModelRuntime {
    /// Validate a launch selection. Explicit effort/tier errors are never cleared.
    ///
    /// # Errors
    /// Returns a configuration error for an unsupported setting or missing context policy.
    pub fn new(
        backend: Option<CatalogBackend>,
        model: &str,
        explicit_window: Option<u64>,
        effort: Option<ReasoningEffort>,
        tier: Option<ServiceTier>,
        aliases: BTreeMap<String, ModelAliasSettings>,
    ) -> Result<Self, ConfigError> {
        Self::from_input(
            backend,
            ModelInput::Raw(model.to_owned()),
            explicit_window,
            effort,
            tier,
            aliases,
        )
    }

    /// Validate a raw token or already-resolved model identity on the actual backend.
    /// Resolved identities skip alias expansion only; all capability checks still apply.
    /// The alias map remains available for subsequent live selections.
    ///
    /// # Errors
    /// Returns a configuration error for an unsupported setting or missing context policy.
    pub fn from_input(
        backend: Option<CatalogBackend>,
        input: ModelInput,
        explicit_window: Option<u64>,
        effort: Option<ReasoningEffort>,
        tier: Option<ServiceTier>,
        aliases: BTreeMap<String, ModelAliasSettings>,
    ) -> Result<Self, ConfigError> {
        let resolved = match input {
            ModelInput::Raw(model) => {
                let model = model.trim();
                validate_model_identity(model)?;
                let resolved = resolve_alias(model, &aliases);
                validate_alias_route(&resolved, backend, None)?;
                resolved
            }
            ModelInput::Resolved(model) => ResolvedModelAlias {
                model,
                provider_profile: None,
                api_shape: None,
            },
        };
        validate_model_identity(&resolved.model)?;
        let window = resolve_window(backend, &resolved.model, explicit_window)?;
        let mut result = Self {
            backend,
            model: resolved.model,
            explicit_window,
            window,
            effort: None,
            tier: None,
            aliases,
            provider_profile: None,
            compaction_policy: CompactionPolicy::Unbound,
        };
        result.set_effort(effort)?;
        result.set_tier(tier)?;
        Ok(result)
    }

    /// Bind the effective reserve used to assemble this session's stable prompt.
    /// Live switches preserve the configured reserve and cannot change whether
    /// automatic compaction is enabled without rebuilding that prompt.
    pub fn bind_compaction_reserve(&mut self, reserve: Option<u64>) {
        self.compaction_policy =
            reserve.map_or(CompactionPolicy::Disabled, CompactionPolicy::Reserve);
    }

    /// Record the already-resolved startup profile without changing the provider.
    pub fn bind_provider_profile(&mut self, profile: Option<String>) {
        self.provider_profile = profile;
    }

    /// Prepare a complete destination, retaining explicit context policy.
    /// Unsupported inherited effort/tier settings are cleared and reported by callers.
    ///
    /// # Errors
    /// A failure leaves the original selection unchanged.
    pub fn prepare(&self, model: &str) -> Result<Self, ConfigError> {
        let resolved = resolve_alias(model.trim(), &self.aliases);
        validate_model_identity(&resolved.model)?;
        validate_alias_route(&resolved, self.backend, self.provider_profile.as_deref())?;
        let window = resolve_window(self.backend, &resolved.model, self.explicit_window)?;
        if let CompactionPolicy::Reserve(reserve) = self.compaction_policy
            && (reserve < self.window) != (reserve < window)
        {
            let change = if reserve < window {
                "enable"
            } else {
                "disable"
            };
            return Err(invalid(format!(
                "switching from model '{}' (context window {}) to '{}' (context window {window}) would {change} automatic compaction with configured auto_compact_reserve_tokens={reserve}; the stable prompt is unchanged during a model switch, so restart with the destination model and desired reserve setting",
                self.model, self.window, resolved.model
            )));
        }
        let effort = self
            .effort
            .filter(|effort| supports_effort(self.backend, &resolved.model, *effort));
        let tier = self
            .tier
            .filter(|tier| supports_tier(self.backend, &resolved.model, *tier));
        Ok(Self {
            model: resolved.model,
            window,
            effort,
            tier,
            ..self.clone()
        })
    }

    /// Canonical selected model ID.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Actual provider route, absent for uncatalogued custom providers.
    #[must_use]
    pub const fn backend(&self) -> Option<CatalogBackend> {
        self.backend
    }
    /// Effective input/context policy for this backend.
    #[must_use]
    pub const fn window(&self) -> u64 {
        self.window
    }
    /// Original explicit override; never inferred from numerical equality.
    #[must_use]
    pub const fn explicit_window(&self) -> Option<u64> {
        self.explicit_window
    }
    /// Current explicit reasoning effort.
    #[must_use]
    pub const fn effort(&self) -> Option<ReasoningEffort> {
        self.effort
    }
    /// Current requested service tier.
    #[must_use]
    pub const fn tier(&self) -> Option<ServiceTier> {
        self.tier
    }

    /// Validate before changing the reasoning effort.
    ///
    /// # Errors
    /// Refuses undeclared efforts on this backend/model.
    pub fn set_effort(&mut self, effort: Option<ReasoningEffort>) -> Result<(), ConfigError> {
        if let Some(value) = effort
            && !supports_effort(self.backend, &self.model, value)
        {
            return Err(invalid(effort_refusal_message(
                self.backend,
                &self.model,
                value,
            )));
        }
        self.effort = effort;
        Ok(())
    }

    /// Validate before changing the service tier.
    ///
    /// # Errors
    /// Refuses undeclared service tiers on this backend/model.
    pub fn set_tier(&mut self, tier: Option<ServiceTier>) -> Result<(), ConfigError> {
        if let Some(value) = tier
            && !supports_tier(self.backend, &self.model, value)
        {
            return Err(invalid(tier_refusal_message(
                self.backend,
                &self.model,
                value,
            )));
        }
        self.tier = tier;
        Ok(())
    }

    /// Publish prepared budgets and the parent model stamp under the driver's exclusive ownership.
    pub fn apply(
        &self,
        config: &mut crate::agent_loop::config::AgentLoopConfig,
        context: &mut crate::agent_loop::loop_context::LoopContext,
        tools: Option<&ToolContext>,
    ) {
        config.context_window_limit = Some(self.window);
        context.reasoning_effort = self.effort;
        context.service_tier = self.tier;
        if let Some(environment) = context.environment.as_mut() {
            environment.model.clone_from(&self.model);
        }
        if let Some(tools) = tools {
            tools.insert_extension(Arc::new(ToolOutputBudget::for_context_window(Some(
                self.window,
            ))));
            tools.insert_extension(Arc::new(AgentModel {
                model: self.model.clone(),
                reasoning_effort: self.effort,
            }));
            crate::agent::arming::publish_parent_context_window(tools, self);
        }
    }
}

fn validate_model_identity(model: &str) -> Result<(), ConfigError> {
    if model.is_empty() {
        return Err(invalid("model must not be empty".to_owned()));
    }
    if model.chars().any(char::is_whitespace) {
        return Err(invalid(format!(
            "model identifier {model:?} contains whitespace; provide one model ID or alias"
        )));
    }
    Ok(())
}

fn validate_alias_route(
    alias: &ResolvedModelAlias,
    backend: Option<CatalogBackend>,
    current_profile: Option<&str>,
) -> Result<(), ConfigError> {
    if let Some(profile) = &alias.provider_profile
        && Some(profile.as_str()) != current_profile
    {
        return Err(invalid(format!(
            "model alias requests provider profile '{profile}' which is not bound to the current provider; resolve that profile before building the agent, or start a new session to change providers"
        )));
    }
    if let Some(shape) = &alias.api_shape {
        let matches = matches!(
            (backend, shape.as_str()),
            (
                Some(CatalogBackend::CODEX | CatalogBackend::RESPONSES),
                "openai_responses"
            ) | (Some(CatalogBackend::CHAT), "openai_chat_completions")
        );
        if !matches {
            return Err(invalid(format!(
                "model alias requests API shape '{shape}' which differs from the selected backend"
            )));
        }
    }
    Ok(())
}

fn backend_label(backend: Option<CatalogBackend>) -> String {
    backend.map_or_else(
        || "provider without a model catalogue".to_owned(),
        |route| format!("{}.{}", route.provider, route.backend),
    )
}

fn invalid(reason: String) -> ConfigError {
    ConfigError::InvalidConfig { reason }
}

#[cfg(test)]
mod tests;
