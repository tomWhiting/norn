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

use norn::config::NornSettings;
use norn::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use norn::r#loop::retry::RetryPolicy;
use norn::profile::Profile;
use norn::provider::request::{ReasoningEffort, ReasoningSummary};

use crate::cli::BuildError;
use crate::cli::{Cli, ReasoningEffort as CliReasoningEffort};
use crate::config::{ConfigOverrides, ProviderConfigOverrides, parse_duration};

/// Side-channel outputs produced when applying CLI overrides that do not
/// fit on the [`Profile`] type itself.
#[derive(Debug, Default, Clone)]
pub struct AppliedOverrides {
    /// Tool patterns added by `--disallowed-tools`, applied after tool
    /// resolution per the brief's NC3 surface. Carried through to the
    /// runtime bundle so downstream filters (bash command refusal etc.)
    /// can consume them.
    pub disallowed_tools: Vec<String>,
}

/// Apply every `--*` CLI flag in NC3 that targets the [`Profile`].
///
/// `profile` is mutated in place. The return value collects the override
/// side-channels — currently only the `--disallowed-tools` list — that
/// have no home on the [`Profile`] type.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a CLI value fails validation —
/// for example an unparseable `--timeout` (handled in
/// [`apply_loop_config_overrides`], not here, but the function returns
/// the unified error type for symmetry).
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

    if let Some(allowed) = cli.allowed_tools.as_deref() {
        profile.tools = Some(split_csv(allowed));
    }

    let disallowed_tools = cli
        .disallowed_tools
        .as_deref()
        .map(split_csv)
        .unwrap_or_default();

    if let Some(effort) = cli.reasoning_effort {
        profile.reasoning_effort = Some(convert_reasoning_effort(effort));
    }

    Ok(AppliedOverrides { disallowed_tools })
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
/// `max_retries`, `options`, `debug_dump_dir`, and `rate_limit`.
/// `runner_path` is out of scope here — the Claude Runner adapter does
/// not flow through `ProviderConfigOverrides`.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `provider.timeout` is present
/// but fails to parse as a humantime duration.
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
    if let Some(dump_dir) = provider.debug_dump_dir.as_deref() {
        overrides.debug_dump_dir = Some(PathBuf::from(dump_dir));
    }
    if let Some(rate_limit) = provider.rate_limit {
        overrides.rate_limit = Some(rate_limit);
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
    if let Some(dump_dir) = cli.debug_dump_dir.as_ref() {
        overrides.debug_dump_dir = Some(dump_dir.clone());
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
                debug_dump_dir: Some("/tmp/dump".to_owned()),
                rate_limit: Some(120),
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
        assert_eq!(overrides.debug_dump_dir, Some(PathBuf::from("/tmp/dump")));
        assert_eq!(overrides.rate_limit, Some(120));
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
            debug_dump_dir: Some(PathBuf::from("/tmp/from-settings")),
            debug_dump_file: None,
            rate_limit: None,
        };
        let cli = ConfigOverrides {
            base_url: Some("https://from-cli".to_owned()),
            max_retries: Some(9),
            request_timeout: Some(Duration::from_secs(50)),
            provider_options: Some(serde_json::json!({"k":"cli"})),
            debug_dump_dir: Some(PathBuf::from("/tmp/from-cli")),
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
        assert_eq!(base.debug_dump_dir, Some(PathBuf::from("/tmp/from-cli")));
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
