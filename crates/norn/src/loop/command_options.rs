//! Shared parsing for slash-command options.

use crate::provider::request::ReasoningEffort;

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
        "ultra" => Some(EffortCommand::Set(ReasoningEffort::Ultra)),
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
