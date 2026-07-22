use std::time::Duration;

use norn::agent_loop::config::{AgentLoopConfig, ConversationStateMode};
use norn::agent_loop::retry::RetryPolicy;
use norn::config::NornSettings;

use crate::cli::{BuildError, Cli};
use crate::config::{ConfigOverrides, parse_duration};

/// Apply CLI flags that target [`AgentLoopConfig`].
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `--timeout` is not a duration.
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

/// Fold parsed `-c key=value` overrides onto an [`AgentLoopConfig`].
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
    if let Some(reserve) = overrides.auto_compact_reserve_tokens {
        config.auto_compact_reserve_tokens = reserve.reserve_tokens();
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

/// Apply merged settings to an [`AgentLoopConfig`] below explicit overrides.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when a configured duration is invalid.
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
    if let Some(reserve) = agent.auto_compact_reserve_tokens {
        config.auto_compact_reserve_tokens = reserve.reserve_tokens();
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

/// Build a retry policy from merged settings and explicit overrides.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `retry.base_delay` is invalid.
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

/// Compiled default for the session index-lock acquisition deadline.
///
/// Owner-delegated value (Tom, 2026-07-06, "go with your recommendations").
pub const DEFAULT_INDEX_LOCK_DEADLINE_MS: u64 = 10_000;

/// Resolve the effective session index-lock acquisition deadline.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when either source supplies zero.
pub fn resolve_index_lock_deadline(
    settings: &NornSettings,
    overrides: &ConfigOverrides,
) -> Result<Duration, BuildError> {
    let (source, value) = if let Some(ms) = overrides.index_lock_deadline_ms {
        ("-c index_lock_deadline_ms", ms)
    } else if let Some(ms) = settings
        .agent
        .as_ref()
        .and_then(|agent| agent.index_lock_deadline_ms)
    {
        ("agent.index_lock_deadline_ms", ms)
    } else {
        ("compiled default", DEFAULT_INDEX_LOCK_DEADLINE_MS)
    };
    if value == 0 {
        return Err(BuildError::Argument(format!(
            "invalid value for {source}: a zero deadline can never acquire the lock",
        )));
    }
    Ok(Duration::from_millis(value))
}

pub(super) fn parse_settings_duration(field: &str, value: &str) -> Result<Duration, BuildError> {
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

/// Default [`AgentLoopConfig`] used by the CLI when no overrides apply.
#[must_use]
pub fn default_agent_loop_config() -> AgentLoopConfig {
    AgentLoopConfig::default()
}

/// Compute the effective step timeout without mutating the loop config.
#[must_use]
pub fn effective_step_timeout(cli: &Cli, overrides: &ConfigOverrides) -> Option<Duration> {
    if let Some(timeout_str) = cli.timeout.as_deref()
        && let Ok(duration) = parse_duration(timeout_str)
    {
        return Some(duration);
    }
    overrides.timeout
}
