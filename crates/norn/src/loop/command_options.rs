//! Shared parsing and model support checks for slash-command options.

use crate::provider::request::{ReasoningEffort, ServiceTier};

/// Parsed reasoning-effort slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortCommand {
    /// Set a concrete effort value.
    Set(ReasoningEffort),
    /// Clear the override.
    Clear,
}

/// Parse `/effort` and `/reasoning-effort` arguments.
#[must_use]
pub fn parse_effort_command(value: &str) -> Option<EffortCommand> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(EffortCommand::Set(ReasoningEffort::None)),
        "low" => Some(EffortCommand::Set(ReasoningEffort::Low)),
        "medium" => Some(EffortCommand::Set(ReasoningEffort::Medium)),
        "high" => Some(EffortCommand::Set(ReasoningEffort::High)),
        "xhigh" => Some(EffortCommand::Set(ReasoningEffort::XHigh)),
        "max" => Some(EffortCommand::Set(ReasoningEffort::Max)),
        "default" | "off" | "clear" => Some(EffortCommand::Clear),
        _ => None,
    }
}

/// Display label for a reasoning-effort value.
#[must_use]
pub fn effort_label(effort: ReasoningEffort) -> &'static str {
    effort.as_str()
}

/// Parsed `/service-tier` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceTierCommand {
    /// Set `service_tier=fast`.
    Fast,
    /// Clear the override.
    Clear,
}

/// Parse `/service-tier` arguments.
#[must_use]
pub fn parse_service_tier_command(value: &str) -> Option<ServiceTierCommand> {
    match value.trim().to_ascii_lowercase().as_str() {
        "fast" => Some(ServiceTierCommand::Fast),
        "none" | "off" | "default" => Some(ServiceTierCommand::Clear),
        _ => None,
    }
}

/// Whether `tier` is present for `model` in the generated model catalog.
#[must_use]
pub fn service_tier_supported_for_model(model: &str, tier: ServiceTier) -> bool {
    crate::model_catalog::find_service_tier(
        crate::model_catalog::DEFAULT_PROVIDER,
        crate::model_catalog::DEFAULT_BACKEND,
        model,
        tier.as_str(),
    )
    .is_some()
}

/// Whether `effort` is declared for `model` in the generated model catalog.
#[must_use]
pub fn reasoning_effort_supported_for_model(model: &str, effort: ReasoningEffort) -> bool {
    crate::model_catalog::find_model(
        crate::model_catalog::DEFAULT_PROVIDER,
        crate::model_catalog::DEFAULT_BACKEND,
        model,
    )
    .is_some_and(|entry| entry.supported_reasoning_efforts.contains(&effort.as_str()))
}

/// Standard unsupported-reasoning-effort diagnostic.
#[must_use]
pub fn unsupported_reasoning_effort_message(model: &str, effort: &str) -> String {
    format!("norn: reasoning effort '{effort}' is not supported for model '{model}'")
}

/// Standard unsupported-service-tier diagnostic.
#[must_use]
pub fn unsupported_service_tier_message(model: &str, tier: &str) -> String {
    format!("norn: service tier '{tier}' is not supported for model '{model}'")
}
