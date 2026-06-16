//! CLI flag overrides applied to a loaded [`Profile`] and the agent
//! configuration triple (NC-004 R2 / R8).
//!
//! Order of application matters: profile loaders (R1) construct the base
//! [`Profile`]; this module mutates that profile in place from the user's
//! CLI flags; then [`apply_loop_config_overrides`] folds CLI-derived
//! values onto an [`AgentLoopConfig`]. The orchestrator (R8) then layers
//! the `-c key=value` [`ConfigOverrides`] on top.
//!
//! The disallowed-tools list lives on the [`AppliedOverrides`] return
//! type rather than the `Profile` because libnorn has no top-level
//! `disallowed_tools` field on [`Profile`]; the brief calls this out and
//! recommends carrying the list separately through to the runtime
//! bundle.

use std::path::PathBuf;
use std::time::Duration;

use norn::agent_loop::config::{AgentLoopConfig, ConversationStateMode};
use norn::agent_loop::retry::RetryPolicy;
use norn::config::NornSettings;
use norn::profile::Profile;
use norn::provider::request::{ReasoningEffort, ReasoningSummary, ServiceTier};

use crate::cli::BuildError;
use crate::cli::{Cli, ReasoningEffort as CliReasoningEffort, ServiceTier as CliServiceTier};
use crate::config::{ConfigOverrides, ProviderConfigOverrides, parse_duration};

/// Side-channel outputs produced when applying CLI overrides that do not
/// fit on the [`Profile`] type itself.
#[derive(Debug, Default, Clone)]
pub struct AppliedOverrides {
    /// Tool names added by `--disallowed-tools` (exact names, matching
    /// the `--allowed-tools` semantics). `build_runtime` applies them to
    /// the registry via [`norn::tool::registry::ToolRegistry::set_disallowed`]
    /// — deny wins over the allow-list — and also carries them on the
    /// runtime bundle for downstream audit surfaces.
    pub disallowed_tools: Vec<String>,
    /// Tool names supplied via the `--allowed-tools` flag specifically
    /// (empty when the flag is absent). Kept separately from
    /// [`Profile::tools`] — which may also be populated by the profile
    /// file — so `build_runtime` can warn about flag-supplied names that
    /// match no registered tool without flagging profile-declared lists.
    pub allowed_tools: Vec<String>,
}

/// Characters that signal a glob / pattern rather than an exact tool
/// name. Tool gating (`--allowed-tools` / `--disallowed-tools`) matches
/// exact registered names only, so any of these in a value means the
/// user expected pattern semantics that do not exist — silently treating
/// `'bash*'` as a literal name would gate nothing.
const TOOL_NAME_PATTERN_CHARS: [char; 6] = ['*', '?', '[', ']', '{', '}'];

/// Reject `flag` values that contain glob / pattern metacharacters.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] naming the flag and the offending
/// value when any name contains one of [`TOOL_NAME_PATTERN_CHARS`].
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

/// Apply every `--*` CLI flag in NC3 that targets the [`Profile`].
///
/// `profile` is mutated in place. The return value collects the override
/// side-channels — the `--disallowed-tools` list and the flag-sourced
/// `--allowed-tools` list — that have no home on the [`Profile`] type.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a `--allowed-tools` or
/// `--disallowed-tools` value contains glob / pattern metacharacters
/// (see [`reject_pattern_tool_names`]) — gating matches exact registered
/// names only, and silently treating `'bash*'` as a literal would
/// enforce nothing.
pub fn apply_cli_profile_overrides(
    cli: &Cli,
    profile: &mut Profile,
) -> Result<AppliedOverrides, BuildError> {
    if let Some(model) = cli.model.as_deref() {
        model.clone_into(&mut profile.model);
    }

    if let Some(prompt) = cli.system_prompt.as_deref() {
        profile.system_instructions = vec![prompt.to_owned()];
    }

    if let Some(appended) = cli.append_system_prompt.as_deref() {
        profile.system_instructions.push(appended.to_owned());
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
    })
}

/// Apply CLI flags that target [`AgentLoopConfig`] (`--max-turns`,
/// `--timeout`). Mutates `config` in place.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `--timeout` is not parseable as
/// a duration via [`parse_duration`].
pub fn apply_loop_config_overrides(
    cli: &Cli,
    config: &mut AgentLoopConfig,
) -> Result<(), BuildError> {
    if let Some(max_turns) = cli.max_turns {
        config.max_iterations = Some(max_turns);
    }
    if let Some(timeout_str) = cli.timeout.as_deref() {
        config.step_timeout = Some(parse_duration(timeout_str)?);
    }
    Ok(())
}

/// Fold a parsed [`ConfigOverrides`] (from `-c key=value` flags) onto an
/// [`AgentLoopConfig`].
///
/// Every `-c` value, when present, overwrites the field unconditionally.
/// The precedence pipeline in [`crate::runtime::build_runtime`] applies
/// settings → `-c` → CLI `--flag`, so the explicit `--flag` form is
/// layered on top of this function's output via
/// [`apply_loop_config_overrides`].
pub fn apply_config_overrides_to_loop(overrides: &ConfigOverrides, config: &mut AgentLoopConfig) {
    if let Some(timeout) = overrides.timeout {
        config.step_timeout = Some(timeout);
    }
    if let Some(max_turns) = overrides.max_turns {
        config.max_iterations = Some(max_turns);
    }
    if let Some(budget) = overrides.schema_budget {
        config.schema_attempt_budget = budget;
    }
    if let Some(window) = overrides.context_window {
        config.context_window_limit = Some(window);
    }
    if let Some(threshold) = overrides.compact_threshold {
        config.auto_compact_threshold_pct = Some(threshold);
    }
    if let Some(keep) = overrides.compact_keep_turns {
        config.auto_compact_keep_recent_turns = keep;
    }
    if let Some(mode) = overrides.conversation_state {
        config.conversation_state = mode;
    }
    if let Some(threshold) = overrides.server_compaction_threshold_tokens {
        config.server_compaction_threshold_tokens = Some(threshold);
    }
}

/// Apply merged [`NornSettings`] to an [`AgentLoopConfig`] as defaults
/// below the `-c` and `--flag` layers (NC-004 R2).
///
/// Every field in the `agent` section maps directly to the
/// corresponding [`AgentLoopConfig`] field; duration strings are parsed
/// via [`humantime::parse_duration`]. Settings always overwrite the
/// `AgentLoopConfig` field when present — the runtime defaults baked
/// into [`AgentLoopConfig::default`] only survive when no settings
/// layer supplies a value.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a duration string fails to
/// parse, embedding the field name (`agent.step_timeout`) so the user
/// can locate the offending value in their settings file.
pub fn apply_settings_to_agent_config(
    settings: &NornSettings,
    config: &mut AgentLoopConfig,
) -> Result<(), BuildError> {
    let Some(agent) = settings.agent.as_ref() else {
        return Ok(());
    };
    if let Some(max_turns) = agent.max_turns {
        config.max_iterations = Some(max_turns);
    }
    if let Some(step_timeout) = agent.step_timeout.as_deref() {
        config.step_timeout = Some(parse_settings_duration("agent.step_timeout", step_timeout)?);
    }
    if let Some(budget) = agent.schema_budget {
        config.schema_attempt_budget = budget;
    }
    if let Some(window) = agent.context_window {
        config.context_window_limit = Some(window);
    }
    if let Some(threshold) = agent.compact_threshold {
        config.auto_compact_threshold_pct = Some(threshold);
    }
    if let Some(keep) = agent.compact_keep_turns {
        config.auto_compact_keep_recent_turns = keep;
    }
    if let Some(mode) = agent.conversation_state.as_deref() {
        config.conversation_state = parse_settings_conversation_state(mode)?;
    }
    if let Some(threshold) = agent.server_compaction_threshold_tokens {
        config.server_compaction_threshold_tokens = Some(threshold);
    }
    Ok(())
}

/// Build a [`ProviderConfigOverrides`] from merged [`NornSettings`] as
/// defaults below the `-c` layer (NC-004 R3 + NC-005 R4).
///
/// Maps the `provider` section verbatim: `base_url`, `timeout`,
/// `max_retries`, `options`, `api_key_env`, `debug_dump_dir`, `rate_limit`,
/// `rate_limit_interval`, `retry_backoff`, `retry_after_ceiling`, and
/// `runner_path`. Duration strings are parsed here; `runner_path`
/// converts to a [`PathBuf`] and is consumed only by the Claude-Runner
/// backend in `print/provider.rs::build_provider`.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `provider.timeout`,
/// `provider.rate_limit_interval`, `provider.retry_backoff`, or
/// `provider.retry_after_ceiling` is present but fails to parse as a
/// humantime duration.
pub fn provider_overrides_from_settings(
    settings: &NornSettings,
) -> Result<ProviderConfigOverrides, BuildError> {
    let mut overrides = ProviderConfigOverrides::default();
    let Some(provider) = settings.provider.as_ref() else {
        return Ok(overrides);
    };
    if let Some(base_url) = provider.base_url.as_deref() {
        overrides.base_url = Some(base_url.to_owned());
    }
    if let Some(timeout) = provider.timeout.as_deref() {
        overrides.request_timeout = Some(parse_settings_duration("provider.timeout", timeout)?);
    }
    if let Some(max_retries) = provider.max_retries {
        overrides.max_retries = Some(max_retries);
    }
    if let Some(options) = provider.options.as_ref() {
        overrides.provider_options = Some(options.clone());
    }
    if let Some(api_key_env) = provider.api_key_env.as_deref() {
        overrides.api_key_env = Some(api_key_env.to_owned());
    }
    if let Some(dump_dir) = provider.debug_dump_dir.as_deref() {
        overrides.debug_dump_dir = Some(PathBuf::from(dump_dir));
    }
    if let Some(rate_limit) = provider.rate_limit {
        overrides.rate_limit = Some(rate_limit);
    }
    if let Some(interval) = provider.rate_limit_interval.as_deref() {
        overrides.rate_limit_interval = Some(parse_settings_duration(
            "provider.rate_limit_interval",
            interval,
        )?);
    }
    if let Some(backoff) = provider.retry_backoff.as_deref() {
        overrides.retry_backoff = Some(parse_settings_duration("provider.retry_backoff", backoff)?);
    }
    if let Some(ceiling) = provider.retry_after_ceiling.as_deref() {
        overrides.retry_after_ceiling = Some(parse_settings_duration(
            "provider.retry_after_ceiling",
            ceiling,
        )?);
    }
    if let Some(runner_path) = provider.runner_path.as_deref() {
        overrides.runner_path = Some(PathBuf::from(runner_path));
    }
    Ok(overrides)
}

/// Overlay `-c` provider values on top of a settings-derived
/// [`ProviderConfigOverrides`] (NC-004 R5). Each `-c` field, when
/// present, overwrites the settings-derived value.
pub fn overlay_cli_provider_overrides(
    overrides: &mut ProviderConfigOverrides,
    cli: &ConfigOverrides,
) {
    if let Some(base_url) = cli.base_url.as_deref() {
        overrides.base_url = Some(base_url.to_owned());
    }
    if let Some(max_retries) = cli.max_retries {
        overrides.max_retries = Some(max_retries);
    }
    if let Some(timeout) = cli.request_timeout {
        overrides.request_timeout = Some(timeout);
    }
    if let Some(options) = cli.provider_options.as_ref() {
        overrides.provider_options = Some(options.clone());
    }
    if let Some(api_key_env) = cli.api_key_env.as_deref() {
        overrides.api_key_env = Some(api_key_env.to_owned());
    }
    if let Some(dump_dir) = cli.debug_dump_dir.as_ref() {
        overrides.debug_dump_dir = Some(dump_dir.clone());
    }
    if let Some(interval) = cli.rate_limit_interval {
        overrides.rate_limit_interval = Some(interval);
    }
    if let Some(backoff) = cli.retry_backoff {
        overrides.retry_backoff = Some(backoff);
    }
    if let Some(ceiling) = cli.retry_after_ceiling {
        overrides.retry_after_ceiling = Some(ceiling);
    }
}

/// Build a [`RetryPolicy`] from merged [`NornSettings`] and `-c`
/// overrides (NC-004 R4).
///
/// Starts from [`RetryPolicy::default`] (preserving the workspace
/// retryable-error set), folds in settings `retry.*` when present, then
/// overlays `-c retry_max` / `-c retry_base_delay`. `backoff_multiplier`
/// has no `-c` surface and is settings-only.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `retry.base_delay` is present
/// but fails to parse as a humantime duration.
pub fn retry_policy_from_settings_and_overrides(
    settings: &NornSettings,
    overrides: &ConfigOverrides,
) -> Result<RetryPolicy, BuildError> {
    let mut policy = RetryPolicy::default();
    if let Some(retry) = settings.retry.as_ref() {
        if let Some(max) = retry.max_retries {
            policy.max_retries = max;
        }
        if let Some(base) = retry.base_delay.as_deref() {
            policy.initial_backoff = parse_settings_duration("retry.base_delay", base)?;
        }
        if let Some(mult) = retry.backoff_multiplier {
            policy.backoff_multiplier = mult;
        }
    }
    if let Some(max) = overrides.retry_max {
        policy.max_retries = max;
    }
    if let Some(delay) = overrides.retry_base_delay {
        policy.initial_backoff = delay;
    }
    Ok(policy)
}

/// Apply settings-level reasoning hints to a [`Profile`] when the profile
/// itself does not specify them (NC-004 R6).
///
/// Profile-level reasoning is model-specific and wins over a global
/// settings default, so this function only fills the field when the
/// profile already has [`None`]. CLI `--reasoning-effort` runs after
/// this helper via [`apply_cli_profile_overrides`] and always wins.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `agent.reasoning_effort` or
/// `agent.reasoning_summary` is not one of the documented enum values
/// (`none`/`low`/`medium`/`high`/`xhigh` and `auto`/`concise`/`detailed`).
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

fn parse_settings_duration(field: &str, value: &str) -> Result<Duration, BuildError> {
    humantime::parse_duration(value).map_err(|err| {
        BuildError::Argument(format!(
            "invalid duration for {field}: '{value}': {err} (examples: 30s, 2m, 1h, 100ms)",
        ))
    })
}

fn parse_settings_conversation_state(raw: &str) -> Result<ConversationStateMode, BuildError> {
    match raw {
        "auto" => Ok(ConversationStateMode::Auto),
        "provider_threaded" => Ok(ConversationStateMode::ProviderThreaded),
        "manual" | "manual_replay" => Ok(ConversationStateMode::ManualReplay),
        _ => Err(BuildError::Argument(format!(
            "invalid value for agent.conversation_state: expected auto, manual, or provider_threaded, got '{raw}'",
        ))),
    }
}

fn parse_reasoning_effort(raw: &str) -> Result<ReasoningEffort, BuildError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value::<ReasoningEffort>(value).map_err(|err| {
        BuildError::Argument(format!(
            "invalid value for agent.reasoning_effort: '{raw}' ({err}); expected one of none, low, medium, high, xhigh",
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

/// Switch the process working directory when `--working-dir` is set.
///
/// Called once at startup before agent execution, per DESIGN.md NC3.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when the working directory does not
/// exist or cannot be entered (permissions, NFS error, etc.).
pub fn apply_working_dir(cli: &Cli) -> Result<(), BuildError> {
    if let Some(dir) = cli.working_dir.as_deref() {
        std::env::set_current_dir(dir).map_err(|err| {
            BuildError::Argument(format!(
                "failed to set working directory {}: {err}",
                dir.display(),
            ))
        })?;
    }
    Ok(())
}

/// Translate the local CLI enum into the runtime's [`ReasoningEffort`].
///
/// The CLI enum is `Copy` for ergonomic flag handling; the norn enum is
/// not — so we map by value rather than `From` to avoid pulling `Copy`
/// requirements over the dependency edge.
fn convert_reasoning_effort(value: CliReasoningEffort) -> ReasoningEffort {
    match value {
        CliReasoningEffort::None => ReasoningEffort::None,
        CliReasoningEffort::Low => ReasoningEffort::Low,
        CliReasoningEffort::Medium => ReasoningEffort::Medium,
        CliReasoningEffort::High => ReasoningEffort::High,
        CliReasoningEffort::XHigh => ReasoningEffort::XHigh,
    }
}

fn convert_service_tier(value: CliServiceTier) -> ServiceTier {
    match value {
        CliServiceTier::Fast => ServiceTier::Fast,
    }
}

/// Parse a comma-separated string, trimming whitespace and dropping
/// empty entries. Used for both `--allowed-tools` and
/// `--disallowed-tools`.
fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Default [`AgentLoopConfig`] used by the CLI when no overrides apply.
///
/// Currently identical to [`AgentLoopConfig::default`]; exposed as a
/// dedicated helper so that future briefs can centralise CLI-only
/// defaults here without touching every call site.
#[must_use]
pub fn default_agent_loop_config() -> AgentLoopConfig {
    AgentLoopConfig::default()
}

/// Compute the step timeout from the CLI overrides + `-c` overrides
/// without mutation. Useful for tests and tooling that needs to know the
/// effective step timeout before calling [`apply_loop_config_overrides`]
/// / [`apply_config_overrides_to_loop`].
#[must_use]
pub fn effective_step_timeout(cli: &Cli, overrides: &ConfigOverrides) -> Option<Duration> {
    if let Some(timeout_str) = cli.timeout.as_deref()
        && let Ok(duration) = parse_duration(timeout_str)
    {
        return Some(duration);
    }
    overrides.timeout
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn model_flag_overrides_profile_model() {
        let cli = cli_from(&["norn", "-m", "gpt-5.5"]);
        let mut profile = Profile {
            model: "gpt-old".to_owned(),
            ..Profile::default()
        };
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(profile.model, "gpt-5.5");
    }

    #[test]
    fn system_prompt_replaces_existing_instructions() {
        let cli = cli_from(&["norn", "-S", "Be concise"]);
        let mut profile = Profile {
            system_instructions: vec!["old".to_owned(), "more old".to_owned()],
            ..Profile::default()
        };
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(profile.system_instructions, vec!["Be concise"]);
    }

    #[test]
    fn append_system_prompt_adds_to_existing() {
        let cli = cli_from(&["norn", "--append-system-prompt", "Also be clear"]);
        let mut profile = Profile {
            system_instructions: vec!["Be concise".to_owned()],
            ..Profile::default()
        };
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(
            profile.system_instructions,
            vec!["Be concise", "Also be clear"],
        );
    }

    #[test]
    fn system_prompt_and_append_combine_in_order() {
        let cli = cli_from(&[
            "norn",
            "-S",
            "Be concise",
            "--append-system-prompt",
            "Also be clear",
        ]);
        let mut profile = Profile::default();
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(
            profile.system_instructions,
            vec!["Be concise", "Also be clear"],
        );
    }

    #[test]
    fn allowed_tools_replaces_profile_tools_with_csv() {
        let cli = cli_from(&["norn", "--allowed-tools", "read,edit"]);
        let mut profile = Profile {
            tools: Some(vec!["bash".to_owned()]),
            ..Profile::default()
        };
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(
            profile.tools,
            Some(vec!["read".to_owned(), "edit".to_owned()]),
        );
    }

    #[test]
    fn allowed_tools_trims_whitespace_and_skips_empty() {
        let cli = cli_from(&["norn", "--allowed-tools", " read , , edit "]);
        let mut profile = Profile::default();
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(
            profile.tools,
            Some(vec!["read".to_owned(), "edit".to_owned()]),
        );
    }

    #[test]
    fn allowed_tools_glob_pattern_is_hard_error() {
        let cli = cli_from(&["norn", "--allowed-tools", "bash*"]);
        let mut profile = Profile::default();
        let err = apply_cli_profile_overrides(&cli, &mut profile).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("--allowed-tools"), "reason: {reason}");
                assert!(reason.contains("bash*"), "reason: {reason}");
                assert!(reason.contains("exact"), "reason: {reason}");
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
        // The profile must not have been partially gated.
        assert!(profile.tools.is_none());
    }

    #[test]
    fn disallowed_tools_glob_pattern_is_hard_error() {
        for pattern in ["bash*", "to?l", "tool[ab]", "tool{ab}"] {
            let cli = cli_from(&["norn", "--disallowed-tools", pattern]);
            let mut profile = Profile::default();
            let err = apply_cli_profile_overrides(&cli, &mut profile).unwrap_err();
            match err {
                BuildError::Argument(reason) => {
                    assert!(
                        reason.contains("--disallowed-tools"),
                        "pattern {pattern}: reason: {reason}",
                    );
                    assert!(
                        reason.contains(pattern),
                        "pattern {pattern}: reason: {reason}",
                    );
                }
                other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
            }
        }
    }

    #[test]
    fn exact_tool_names_pass_pattern_rejection() {
        let cli = cli_from(&[
            "norn",
            "--allowed-tools",
            "read,write_file,lsp-bridge",
            "--disallowed-tools",
            "bash",
        ]);
        let mut profile = Profile::default();
        let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(
            applied.allowed_tools,
            vec!["read", "write_file", "lsp-bridge"],
        );
        assert_eq!(applied.disallowed_tools, vec!["bash"]);
    }

    #[test]
    fn allowed_tools_flag_absent_leaves_applied_allowed_empty() {
        let cli = cli_from(&["norn"]);
        let mut profile = Profile {
            tools: Some(vec!["from-profile".to_owned()]),
            ..Profile::default()
        };
        let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert!(applied.allowed_tools.is_empty());
        // Profile-declared tools stay untouched by the flag plumbing.
        assert_eq!(profile.tools, Some(vec!["from-profile".to_owned()]));
    }

    #[test]
    fn disallowed_tools_returned_in_applied_overrides() {
        let cli = cli_from(&["norn", "--disallowed-tools", "write,edit"]);
        let mut profile = Profile::default();
        let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(
            applied.disallowed_tools,
            vec!["write".to_owned(), "edit".to_owned()],
        );
        // Profile.tools must NOT have been touched.
        assert!(profile.tools.is_none());
    }

    #[test]
    fn reasoning_effort_high_maps_to_runtime_high() {
        let cli = cli_from(&["norn", "--reasoning-effort", "high"]);
        let mut profile = Profile::default();
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn reasoning_effort_low_and_medium_map_correctly() {
        let cli_low = cli_from(&["norn", "--reasoning-effort", "low"]);
        let cli_medium = cli_from(&["norn", "--reasoning-effort", "medium"]);
        let mut profile = Profile::default();
        apply_cli_profile_overrides(&cli_low, &mut profile).unwrap();
        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Low));
        apply_cli_profile_overrides(&cli_medium, &mut profile).unwrap();
        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Medium));
    }

    #[test]
    fn service_tier_flags_map_to_runtime_fast() {
        let cli = cli_from(&["norn", "--service-tier", "fast"]);
        let mut profile = Profile::default();
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(profile.service_tier, Some(ServiceTier::Fast));

        let cli = cli_from(&["norn", "--fast"]);
        let mut profile = Profile::default();
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(profile.service_tier, Some(ServiceTier::Fast));
    }

    #[test]
    fn no_flags_leaves_profile_untouched() {
        let cli = cli_from(&["norn"]);
        let mut profile = Profile {
            model: "kept".to_owned(),
            system_instructions: vec!["kept".to_owned()],
            tools: Some(vec!["kept".to_owned()]),
            ..Profile::default()
        };
        let snapshot = profile.clone();
        apply_cli_profile_overrides(&cli, &mut profile).unwrap();
        assert_eq!(profile.model, snapshot.model);
        assert_eq!(profile.system_instructions, snapshot.system_instructions);
        assert_eq!(profile.tools, snapshot.tools);
    }

    #[test]
    fn max_turns_flag_sets_iteration_cap() {
        let cli = cli_from(&["norn", "--max-turns", "10"]);
        let mut config = default_agent_loop_config();
        apply_loop_config_overrides(&cli, &mut config).unwrap();
        assert_eq!(config.max_iterations, Some(10));
    }

    #[test]
    fn timeout_flag_30s_sets_step_timeout() {
        let cli = cli_from(&["norn", "--timeout", "30s"]);
        let mut config = default_agent_loop_config();
        apply_loop_config_overrides(&cli, &mut config).unwrap();
        assert_eq!(config.step_timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn timeout_flag_2m_sets_step_timeout() {
        let cli = cli_from(&["norn", "--timeout", "2m"]);
        let mut config = default_agent_loop_config();
        apply_loop_config_overrides(&cli, &mut config).unwrap();
        assert_eq!(config.step_timeout, Some(Duration::from_mins(2)));
    }

    #[test]
    fn invalid_timeout_flag_returns_argument_error() {
        let cli = cli_from(&["norn", "--timeout", "garbage"]);
        let mut config = default_agent_loop_config();
        let err = apply_loop_config_overrides(&cli, &mut config).unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn config_overrides_always_overwrite_then_cli_flag_wins() {
        // New pipeline: settings → -c → CLI --flag. apply_config_overrides_to_loop
        // unconditionally writes every supplied -c value; the explicit
        // CLI --flag runs LAST in build_runtime and wins.
        let cli = cli_from(&["norn", "--max-turns", "3"]);
        let mut config = default_agent_loop_config();

        let overrides = ConfigOverrides::parse(&[
            "max_turns=99".to_owned(),
            "schema_budget=7".to_owned(),
            "context_window=12345".to_owned(),
            "compact_threshold=0.5".to_owned(),
            "compact_keep_turns=4".to_owned(),
        ])
        .unwrap();
        // -c first (settings would precede it in the real pipeline; here we
        // start from the bare default).
        apply_config_overrides_to_loop(&overrides, &mut config);
        // CLI --flag last — overwrites the -c value.
        apply_loop_config_overrides(&cli, &mut config).unwrap();

        assert_eq!(config.max_iterations, Some(3), "CLI --max-turns wins");
        // Fields without a CLI sibling stay at their -c value.
        assert_eq!(config.schema_attempt_budget, 7);
        assert_eq!(config.context_window_limit, Some(12345));
        assert!((config.auto_compact_threshold_pct.unwrap() - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.auto_compact_keep_recent_turns, 4);
    }

    #[test]
    fn settings_to_agent_config_fills_every_field() {
        use norn::config::{AgentSettings, NornSettings};
        let mut config = default_agent_loop_config();
        let settings = NornSettings {
            agent: Some(AgentSettings {
                max_turns: Some(11),
                step_timeout: Some("45s".to_owned()),
                schema_budget: Some(7),
                context_window: Some(200_000),
                compact_threshold: Some(0.6),
                compact_keep_turns: Some(8),
                conversation_state: Some("provider_threaded".to_owned()),
                server_compaction_threshold_tokens: Some(180_000),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        apply_settings_to_agent_config(&settings, &mut config).unwrap();
        assert_eq!(config.max_iterations, Some(11));
        assert_eq!(config.step_timeout, Some(Duration::from_secs(45)));
        assert_eq!(config.schema_attempt_budget, 7);
        assert_eq!(config.context_window_limit, Some(200_000));
        assert!((config.auto_compact_threshold_pct.unwrap() - 0.6).abs() < f64::EPSILON);
        assert_eq!(config.auto_compact_keep_recent_turns, 8);
        assert_eq!(
            config.conversation_state,
            ConversationStateMode::ProviderThreaded
        );
        assert_eq!(config.server_compaction_threshold_tokens, Some(180_000));
    }

    #[test]
    fn settings_to_agent_config_rejects_bad_duration() {
        use norn::config::{AgentSettings, NornSettings};
        let mut config = default_agent_loop_config();
        let settings = NornSettings {
            agent: Some(AgentSettings {
                step_timeout: Some("not-a-duration".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = apply_settings_to_agent_config(&settings, &mut config).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("agent.step_timeout"), "reason: {reason}");
                assert!(reason.contains("not-a-duration"), "reason: {reason}");
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn provider_overrides_from_settings_maps_every_field() {
        use norn::config::{NornSettings, ProviderSettings};
        let settings = NornSettings {
            provider: Some(ProviderSettings {
                base_url: Some("https://api.example.com".to_owned()),
                timeout: Some("12s".to_owned()),
                max_retries: Some(4),
                options: Some(serde_json::json!({"k":"v"})),
                api_key_env: Some("LOCAL_AI_KEY".to_owned()),
                debug_dump_dir: Some("/tmp/dump".to_owned()),
                rate_limit: Some(120),
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                runner_path: Some("/usr/local/bin/claude".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let overrides = provider_overrides_from_settings(&settings).unwrap();
        assert_eq!(
            overrides.base_url.as_deref(),
            Some("https://api.example.com")
        );
        assert_eq!(overrides.request_timeout, Some(Duration::from_secs(12)));
        assert_eq!(overrides.max_retries, Some(4));
        assert_eq!(
            overrides
                .provider_options
                .as_ref()
                .and_then(|v| v.get("k"))
                .and_then(serde_json::Value::as_str),
            Some("v"),
        );
        assert_eq!(overrides.api_key_env.as_deref(), Some("LOCAL_AI_KEY"));
        assert_eq!(overrides.debug_dump_dir, Some(PathBuf::from("/tmp/dump")));
        assert_eq!(overrides.rate_limit, Some(120));
        assert_eq!(overrides.rate_limit_interval, Some(Duration::from_secs(90)));
        assert_eq!(overrides.retry_backoff, Some(Duration::from_millis(500)));
        assert_eq!(overrides.retry_after_ceiling, Some(Duration::from_mins(2)));
        assert_eq!(
            overrides.runner_path,
            Some(PathBuf::from("/usr/local/bin/claude")),
        );
    }

    #[test]
    fn provider_overrides_from_settings_rejects_bad_rate_retry_durations() {
        use norn::config::{NornSettings, ProviderSettings};
        for (field, provider) in [
            (
                "provider.rate_limit_interval",
                ProviderSettings {
                    rate_limit_interval: Some("nope".to_owned()),
                    ..ProviderSettings::default()
                },
            ),
            (
                "provider.retry_backoff",
                ProviderSettings {
                    retry_backoff: Some("nope".to_owned()),
                    ..ProviderSettings::default()
                },
            ),
            (
                "provider.retry_after_ceiling",
                ProviderSettings {
                    retry_after_ceiling: Some("nope".to_owned()),
                    ..ProviderSettings::default()
                },
            ),
        ] {
            let settings = NornSettings {
                provider: Some(provider),
                ..NornSettings::default()
            };
            let err = provider_overrides_from_settings(&settings).unwrap_err();
            match err {
                BuildError::Argument(reason) => {
                    assert!(reason.contains(field), "reason must name {field}: {reason}");
                }
                other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
            }
        }
    }

    #[test]
    fn cli_rate_retry_overrides_beat_settings_values() {
        // Precedence: `-c` flag beats settings, settings beat the library
        // default — same chain as timeout / max_retries.
        use norn::config::{NornSettings, ProviderSettings};
        let settings = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let mut overrides = provider_overrides_from_settings(&settings).unwrap();
        let cli = ConfigOverrides {
            rate_limit_interval: Some(Duration::from_secs(30)),
            retry_backoff: Some(Duration::from_secs(3)),
            retry_after_ceiling: Some(Duration::from_mins(10)),
            ..ConfigOverrides::default()
        };
        overlay_cli_provider_overrides(&mut overrides, &cli);
        assert_eq!(overrides.rate_limit_interval, Some(Duration::from_secs(30)));
        assert_eq!(overrides.retry_backoff, Some(Duration::from_secs(3)));
        assert_eq!(overrides.retry_after_ceiling, Some(Duration::from_mins(10)),);
    }

    #[test]
    fn settings_rate_retry_values_survive_when_cli_unset() {
        use norn::config::{NornSettings, ProviderSettings};
        let settings = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let mut overrides = provider_overrides_from_settings(&settings).unwrap();
        overlay_cli_provider_overrides(&mut overrides, &ConfigOverrides::default());
        assert_eq!(overrides.rate_limit_interval, Some(Duration::from_secs(90)));
        assert_eq!(overrides.retry_backoff, Some(Duration::from_millis(500)));
        assert_eq!(overrides.retry_after_ceiling, Some(Duration::from_mins(2)));
    }

    #[test]
    fn provider_overrides_from_settings_rate_limit_absent_stays_none() {
        use norn::config::{NornSettings, ProviderSettings};
        let settings = NornSettings {
            provider: Some(ProviderSettings::default()),
            ..NornSettings::default()
        };
        let overrides = provider_overrides_from_settings(&settings).unwrap();
        assert_eq!(overrides.rate_limit, None);
    }

    #[test]
    fn overlay_cli_provider_overrides_overwrites_when_present() {
        let mut base = ProviderConfigOverrides {
            base_url: Some("https://from-settings".to_owned()),
            max_retries: Some(1),
            request_timeout: Some(Duration::from_secs(5)),
            provider_options: Some(serde_json::json!({"k":"settings"})),
            api_key_env: Some("SETTINGS_KEY".to_owned()),
            debug_dump_dir: Some(PathBuf::from("/tmp/from-settings")),
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: None,
            retry_backoff: None,
            retry_after_ceiling: None,
            runner_path: None,
        };
        let cli = ConfigOverrides {
            base_url: Some("https://from-cli".to_owned()),
            max_retries: Some(9),
            request_timeout: Some(Duration::from_secs(50)),
            provider_options: Some(serde_json::json!({"k":"cli"})),
            api_key_env: Some("CLI_KEY".to_owned()),
            debug_dump_dir: Some(PathBuf::from("/tmp/from-cli")),
            rate_limit_interval: Some(Duration::from_secs(30)),
            retry_backoff: Some(Duration::from_millis(500)),
            retry_after_ceiling: Some(Duration::from_mins(2)),
            ..ConfigOverrides::default()
        };
        overlay_cli_provider_overrides(&mut base, &cli);
        assert_eq!(base.base_url.as_deref(), Some("https://from-cli"));
        assert_eq!(base.max_retries, Some(9));
        assert_eq!(base.request_timeout, Some(Duration::from_secs(50)));
        assert_eq!(
            base.provider_options
                .as_ref()
                .and_then(|v| v.get("k"))
                .and_then(serde_json::Value::as_str),
            Some("cli"),
        );
        assert_eq!(base.api_key_env.as_deref(), Some("CLI_KEY"));
        assert_eq!(base.debug_dump_dir, Some(PathBuf::from("/tmp/from-cli")));
        assert_eq!(base.rate_limit_interval, Some(Duration::from_secs(30)));
        assert_eq!(base.retry_backoff, Some(Duration::from_millis(500)));
        assert_eq!(base.retry_after_ceiling, Some(Duration::from_mins(2)));
    }

    #[test]
    fn overlay_cli_provider_overrides_preserves_settings_when_cli_unset() {
        let mut base = ProviderConfigOverrides {
            base_url: Some("https://from-settings".to_owned()),
            max_retries: Some(1),
            ..ProviderConfigOverrides::default()
        };
        let cli = ConfigOverrides::default();
        overlay_cli_provider_overrides(&mut base, &cli);
        assert_eq!(base.base_url.as_deref(), Some("https://from-settings"));
        assert_eq!(base.max_retries, Some(1));
    }

    #[test]
    fn retry_policy_combines_settings_then_cli() {
        use norn::config::{NornSettings, RetrySettings};
        let settings = NornSettings {
            retry: Some(RetrySettings {
                max_retries: Some(5),
                base_delay: Some("3s".to_owned()),
                backoff_multiplier: Some(1.5),
            }),
            ..NornSettings::default()
        };
        let cli = ConfigOverrides {
            retry_max: Some(9),
            ..ConfigOverrides::default()
        };
        let policy = retry_policy_from_settings_and_overrides(&settings, &cli).unwrap();
        assert_eq!(policy.max_retries, 9, "CLI -c retry_max wins");
        assert_eq!(policy.initial_backoff, Duration::from_secs(3));
        assert!((policy.backoff_multiplier - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn retry_policy_settings_only_when_no_cli() {
        use norn::config::{NornSettings, RetrySettings};
        let settings = NornSettings {
            retry: Some(RetrySettings {
                max_retries: Some(7),
                base_delay: Some("250ms".to_owned()),
                backoff_multiplier: Some(3.0),
            }),
            ..NornSettings::default()
        };
        let cli = ConfigOverrides::default();
        let policy = retry_policy_from_settings_and_overrides(&settings, &cli).unwrap();
        assert_eq!(policy.max_retries, 7);
        assert_eq!(policy.initial_backoff, Duration::from_millis(250));
        assert!((policy.backoff_multiplier - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn retry_policy_default_when_no_inputs() {
        let policy = retry_policy_from_settings_and_overrides(
            &NornSettings::default(),
            &ConfigOverrides::default(),
        )
        .unwrap();
        assert_eq!(policy.max_retries, 2);
        assert_eq!(policy.initial_backoff, Duration::from_secs(1));
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn settings_reasoning_fills_profile_when_unset() {
        use norn::config::{AgentSettings, NornSettings};
        let mut profile = Profile::default();
        let settings = NornSettings {
            agent: Some(AgentSettings {
                reasoning_effort: Some("low".to_owned()),
                reasoning_summary: Some("detailed".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap();
        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(profile.reasoning_summary, Some(ReasoningSummary::Detailed));
    }

    #[test]
    fn settings_service_tier_fills_profile_when_unset() {
        use norn::config::{AgentSettings, NornSettings};
        let mut profile = Profile::default();
        let settings = NornSettings {
            agent: Some(AgentSettings {
                service_tier: Some("fast".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap();
        assert_eq!(profile.service_tier, Some(ServiceTier::Fast));
    }

    #[test]
    fn settings_reasoning_does_not_overwrite_profile() {
        use norn::config::{AgentSettings, NornSettings};
        let mut profile = Profile {
            reasoning_effort: Some(ReasoningEffort::High),
            ..Profile::default()
        };
        let settings = NornSettings {
            agent: Some(AgentSettings {
                reasoning_effort: Some("low".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap();
        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn settings_reasoning_rejects_bad_value() {
        use norn::config::{AgentSettings, NornSettings};
        let mut profile = Profile::default();
        let settings = NornSettings {
            agent: Some(AgentSettings {
                reasoning_effort: Some("turbo".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(
                    reason.contains("agent.reasoning_effort"),
                    "reason: {reason}"
                );
                assert!(reason.contains("turbo"), "reason: {reason}");
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }
}
