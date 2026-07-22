use norn::config::NornSettings;
use norn::profile::Profile;
use norn::provider::request::{ReasoningEffort, ReasoningSummary, ServiceTier};

use super::AppliedOverrides;
use crate::cli::BuildError;
use crate::cli::{Cli, ReasoningEffort as CliReasoningEffort, ServiceTier as CliServiceTier};

/// Characters that signal a glob or pattern rather than an exact tool name.
const TOOL_NAME_PATTERN_CHARS: [char; 6] = ['*', '?', '[', ']', '{', '}'];

fn reject_pattern_tool_names(flag: &str, names: &[String]) -> Result<(), BuildError> {
    for name in names {
        if name.contains(TOOL_NAME_PATTERN_CHARS) {
            return Err(BuildError::Argument(format!(
                "{flag} value '{name}' contains pattern characters \
                 ({TOOL_NAME_PATTERN_CHARS:?}); tool gating matches exact registered \
                 tool names only — pass the exact name (e.g. 'bash', not 'bash*')",
            )));
        }
    }
    Ok(())
}

/// Apply every CLI flag that targets the [`Profile`].
///
/// Prompt flags stay on the returned side channel so they do not destroy the
/// resolved profile's filesystem provenance before prompt authority is derived.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when an allowed or disallowed tool value
/// contains pattern metacharacters; tool gating accepts exact names only.
pub fn apply_cli_profile_overrides(
    cli: &Cli,
    profile: &mut Profile,
) -> Result<AppliedOverrides, BuildError> {
    if let Some(model) = cli.model.as_deref() {
        model.clone_into(&mut profile.model);
    }

    let allowed_tools = cli
        .allowed_tools
        .as_deref()
        .map(split_csv)
        .unwrap_or_default();
    reject_pattern_tool_names("--allowed-tools", &allowed_tools)?;
    if cli.allowed_tools.is_some() {
        profile.tools = Some(allowed_tools.clone());
    }

    let disallowed_tools = cli
        .disallowed_tools
        .as_deref()
        .map(split_csv)
        .unwrap_or_default();
    reject_pattern_tool_names("--disallowed-tools", &disallowed_tools)?;

    if let Some(effort) = cli.reasoning_effort {
        profile.reasoning_effort = Some(convert_reasoning_effort(effort));
    }
    if let Some(tier) = cli.service_tier {
        profile.service_tier = Some(convert_service_tier(tier));
    }
    if cli.fast {
        profile.service_tier = Some(ServiceTier::Fast);
    }

    Ok(AppliedOverrides {
        disallowed_tools,
        allowed_tools,
        system_prompt: cli.system_prompt.clone(),
        append_system_prompt: cli.append_system_prompt.clone(),
    })
}

/// Apply settings-level reasoning hints when the profile does not specify them.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a reasoning or service-tier value is
/// not a documented enum value.
pub fn apply_settings_reasoning_to_profile(
    settings: &NornSettings,
    profile: &mut Profile,
) -> Result<(), BuildError> {
    let Some(agent) = settings.agent.as_ref() else {
        return Ok(());
    };
    if profile.reasoning_effort.is_none()
        && let Some(raw) = agent.reasoning_effort.as_deref()
    {
        profile.reasoning_effort = Some(parse_reasoning_effort(raw)?);
    }
    if profile.reasoning_summary.is_none()
        && let Some(raw) = agent.reasoning_summary.as_deref()
    {
        profile.reasoning_summary = Some(parse_reasoning_summary(raw)?);
    }
    if profile.service_tier.is_none()
        && let Some(raw) = agent.service_tier.as_deref()
    {
        profile.service_tier = Some(parse_service_tier(raw)?);
    }
    Ok(())
}

fn parse_reasoning_effort(raw: &str) -> Result<ReasoningEffort, BuildError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value::<ReasoningEffort>(value).map_err(|err| {
        BuildError::Argument(format!(
            "invalid value for agent.reasoning_effort: '{raw}' ({err}); expected one of none, low, medium, high, xhigh, max",
        ))
    })
}

fn parse_reasoning_summary(raw: &str) -> Result<ReasoningSummary, BuildError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value::<ReasoningSummary>(value).map_err(|err| {
        BuildError::Argument(format!(
            "invalid value for agent.reasoning_summary: '{raw}' ({err}); expected one of auto, concise, detailed",
        ))
    })
}

fn parse_service_tier(raw: &str) -> Result<ServiceTier, BuildError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value::<ServiceTier>(value).map_err(|err| {
        BuildError::Argument(format!(
            "invalid value for agent.service_tier: '{raw}' ({err}); expected one of fast",
        ))
    })
}

fn convert_reasoning_effort(value: CliReasoningEffort) -> ReasoningEffort {
    match value {
        CliReasoningEffort::None => ReasoningEffort::None,
        CliReasoningEffort::Low => ReasoningEffort::Low,
        CliReasoningEffort::Medium => ReasoningEffort::Medium,
        CliReasoningEffort::High => ReasoningEffort::High,
        CliReasoningEffort::XHigh => ReasoningEffort::XHigh,
        CliReasoningEffort::Max => ReasoningEffort::Max,
    }
}

fn convert_service_tier(value: CliServiceTier) -> ServiceTier {
    match value {
        CliServiceTier::Fast => ServiceTier::Fast,
    }
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}
