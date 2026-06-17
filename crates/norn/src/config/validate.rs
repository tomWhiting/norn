//! Semantic validation for a merged [`NornSettings`].
//!
//! [`validate_settings`] is invoked *after* the loader has produced typed
//! settings and *after* [`crate::config::merge::merge_settings`] has folded
//! the layers together. It enforces three classes of check, none of which
//! serde can perform on its own:
//!
//! - **Duration fields** must parse as `humantime` durations
//!   (`provider.timeout`, `agent.step_timeout`,
//!   `agent.prompt_command_timeout`, `retry.base_delay`).
//! - **Permission patterns** must be syntactically well-formed: non-empty,
//!   balanced parentheses, no embedded control characters. The full
//!   matcher logic lives downstream (Boundary in brief NC-003).
//! - **MCP server definitions** must specify at least one of `command` or
//!   `url`; an entry that has neither has no executable shape.
//!
//! Unknown top-level keys are *not* surfaced here — they are warned about
//! in the loader (see [`crate::config::loader`]) before typed conversion
//! drops them. By the time `validate_settings` runs, the typed view has
//! already discarded any unrecognised keys.

use crate::config::types::NornSettings;
use crate::error::ConfigError;

/// Validate a merged [`NornSettings`] value.
///
/// Returns [`Ok`] when all checks pass. Returns [`ConfigError::InvalidConfig`]
/// naming the offending field and its value on first failure — single-error
/// reporting matches the existing patterns in `assembly.rs` and keeps the
/// CLI's error stream readable.
///
/// # Errors
///
/// - A duration string is not parseable by `humantime`.
/// - A permission pattern is empty, has unbalanced parentheses, or
///   contains control characters.
/// - An MCP server definition has neither `command` nor `url`.
pub fn validate_settings(settings: &NornSettings) -> Result<(), ConfigError> {
    validate_durations(settings)?;
    validate_numeric_ranges(settings)?;
    validate_provider_profiles(settings)?;
    validate_model_aliases(settings)?;
    validate_permissions(settings)?;
    validate_mcp_servers(settings)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------

fn validate_durations(settings: &NornSettings) -> Result<(), ConfigError> {
    if let Some(provider) = settings.provider.as_ref() {
        validate_provider_durations("provider", provider)?;
    }
    if let Some(agent) = settings.agent.as_ref() {
        if let Some(step) = agent.step_timeout.as_deref() {
            check_duration("agent.step_timeout", step)?;
        }
        if let Some(prompt) = agent.prompt_command_timeout.as_deref() {
            check_duration("agent.prompt_command_timeout", prompt)?;
        }
    }
    if let Some(retry) = settings.retry.as_ref()
        && let Some(base) = retry.base_delay.as_deref()
    {
        check_duration("retry.base_delay", base)?;
    }
    Ok(())
}

fn validate_provider_durations(
    prefix: &str,
    provider: &crate::config::types::ProviderSettings,
) -> Result<(), ConfigError> {
    if let Some(timeout) = provider.timeout.as_deref() {
        check_duration(&format!("{prefix}.timeout"), timeout)?;
    }
    // The three rate/retry knobs are durations that must also be
    // non-zero: a zero replenishment window or zero backoff degrades
    // to a busy loop, and a zero Retry-After ceiling would clamp
    // every server-requested wait to nothing — defeating the rate
    // limiter entirely.
    if let Some(interval) = provider.rate_limit_interval.as_deref() {
        check_nonzero_duration(&format!("{prefix}.rate_limit_interval"), interval)?;
    }
    if let Some(backoff) = provider.retry_backoff.as_deref() {
        check_nonzero_duration(&format!("{prefix}.retry_backoff"), backoff)?;
    }
    if let Some(ceiling) = provider.retry_after_ceiling.as_deref() {
        check_nonzero_duration(&format!("{prefix}.retry_after_ceiling"), ceiling)?;
    }
    Ok(())
}

fn check_duration(field: &str, value: &str) -> Result<(), ConfigError> {
    humantime::parse_duration(value)
        .map(|_| ())
        .map_err(|err| ConfigError::InvalidConfig {
            reason: format!("invalid duration for {field}: '{value}' ({err})"),
        })
}

/// Like [`check_duration`] but additionally rejects a parsed value of
/// zero. Used by fields whose zero form is semantically a deadlock or a
/// busy loop rather than a meaningful configuration.
fn check_nonzero_duration(field: &str, value: &str) -> Result<(), ConfigError> {
    let parsed = humantime::parse_duration(value).map_err(|err| ConfigError::InvalidConfig {
        reason: format!("invalid duration for {field}: '{value}' ({err})"),
    })?;
    if parsed.is_zero() {
        return Err(ConfigError::InvalidConfig {
            reason: format!("invalid value for {field}: '{value}' (must be greater than zero)"),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Numeric ranges
// ---------------------------------------------------------------------------

fn validate_numeric_ranges(settings: &NornSettings) -> Result<(), ConfigError> {
    if let Some(agent) = settings.agent.as_ref()
        && let Some(threshold) = agent.compact_threshold
        && !(0.0..=1.0).contains(&threshold)
    {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "invalid value for agent.compact_threshold: {threshold} (must be 0.0..=1.0)",
            ),
        });
    }
    if let Some(agent) = settings.agent.as_ref()
        && let Some(mode) = agent.conversation_state.as_deref()
        && !matches!(
            mode,
            "auto" | "manual" | "manual_replay" | "provider_threaded"
        )
    {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "invalid value for agent.conversation_state: {mode} (expected auto, manual, or provider_threaded)",
            ),
        });
    }
    // A zero rate limit constructs `Semaphore::new(0)`, whose `acquire()` loop
    // can never obtain a permit (replenishment adds zero) and deadlocks the
    // provider permanently. Catch it at config time rather than guarding the
    // semaphore constructor.
    if let Some(provider) = settings.provider.as_ref()
        && provider.rate_limit == Some(0)
    {
        return Err(ConfigError::InvalidConfig {
            reason: "provider.rate_limit must be greater than 0 (zero permits would deadlock the \
                     rate limiter)"
                .to_string(),
        });
    }
    if let Some(profiles) = settings.provider_profiles.as_ref() {
        for (name, profile) in profiles {
            if profile.provider.rate_limit == Some(0) {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "provider_profiles.{name}.rate_limit must be greater than 0 \
                         (zero permits would deadlock the rate limiter)",
                    ),
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider profiles
// ---------------------------------------------------------------------------

fn validate_provider_profiles(settings: &NornSettings) -> Result<(), ConfigError> {
    let Some(profiles) = settings.provider_profiles.as_ref() else {
        return Ok(());
    };
    for (name, profile) in profiles {
        crate::provider::ProviderProfileId::new(name).map_err(|err| {
            ConfigError::InvalidConfig {
                reason: format!("invalid provider profile id '{name}': {err}"),
            }
        })?;
        validate_provider_durations(&format!("provider_profiles.{name}"), &profile.provider)?;
        if let Some(api_shape) = profile.api_shape.as_deref() {
            check_nonempty_clean(&format!("provider_profiles.{name}.api_shape"), api_shape)?;
            api_shape
                .parse::<crate::provider::ApiShape>()
                .map_err(|err| ConfigError::InvalidConfig {
                    reason: format!("invalid value for provider_profiles.{name}.api_shape: {err}"),
                })?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Model aliases
// ---------------------------------------------------------------------------

fn validate_model_aliases(settings: &NornSettings) -> Result<(), ConfigError> {
    let Some(aliases) = settings.model_aliases.as_ref() else {
        return Ok(());
    };
    for (alias, target) in aliases {
        check_nonempty_clean("model_aliases key", alias)?;
        check_nonempty_clean(&format!("model_aliases.{alias}.model"), target.model())?;
        if let Some(profile) = target.provider_profile() {
            check_nonempty_clean(&format!("model_aliases.{alias}.provider_profile"), profile)?;
        }
        if let Some(api_shape) = target.api_shape() {
            check_nonempty_clean(&format!("model_aliases.{alias}.api_shape"), api_shape)?;
            api_shape
                .parse::<crate::provider::ApiShape>()
                .map_err(|err| ConfigError::InvalidConfig {
                    reason: format!("invalid value for model_aliases.{alias}.api_shape: {err}"),
                })?;
        }
    }
    Ok(())
}

fn check_nonempty_clean(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::InvalidConfig {
            reason: format!("invalid value for {field}: must not be empty"),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ConfigError::InvalidConfig {
            reason: format!("invalid value for {field}: must not contain control characters"),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Permission patterns
// ---------------------------------------------------------------------------

fn validate_permissions(settings: &NornSettings) -> Result<(), ConfigError> {
    let Some(perms) = settings.permissions.as_ref() else {
        return Ok(());
    };
    if let Some(allow) = perms.allow.as_ref() {
        for pattern in allow {
            check_permission_pattern("permissions.allow", pattern)?;
        }
    }
    if let Some(deny) = perms.deny.as_ref() {
        for pattern in deny {
            check_permission_pattern("permissions.deny", pattern)?;
        }
    }
    if let Some(ask) = perms.ask.as_ref() {
        for pattern in ask {
            check_permission_pattern("permissions.ask", pattern)?;
        }
    }
    Ok(())
}

/// Permissive syntactic check for a Claude-Code-style permission pattern.
///
/// Rejects only patterns that cannot be matched by any reasonable engine:
/// empty strings, unbalanced parentheses, and embedded control characters.
/// Anything beyond that — wildcard semantics, tool-name validity — is the
/// responsibility of the downstream matcher (Boundary: NC-003 SHALL NOT
/// implement matching).
fn check_permission_pattern(field: &str, pattern: &str) -> Result<(), ConfigError> {
    if pattern.is_empty() {
        return Err(ConfigError::InvalidConfig {
            reason: format!("invalid permission pattern in {field}: empty string"),
        });
    }
    if pattern.chars().any(char::is_control) {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "invalid permission pattern in {field}: '{pattern}' contains control characters",
            ),
        });
    }
    let mut depth: i32 = 0;
    for ch in pattern.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return Err(ConfigError::InvalidConfig {
                        reason: format!(
                            "invalid permission pattern in {field}: '{pattern}' has unbalanced parentheses",
                        ),
                    });
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "invalid permission pattern in {field}: '{pattern}' has unbalanced parentheses",
            ),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP server shape
// ---------------------------------------------------------------------------

fn validate_mcp_servers(settings: &NornSettings) -> Result<(), ConfigError> {
    let Some(servers) = settings.mcp_servers.as_ref() else {
        return Ok(());
    };
    for (name, def) in servers {
        if def.command.is_none() && def.url.is_none() {
            return Err(ConfigError::InvalidConfig {
                reason: format!("mcp server '{name}' has neither command nor url"),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::uninlined_format_args
)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::types::{
        AgentSettings, McpServerSettings, ModelAliasSelection, ModelAliasSettings,
        PermissionSettings, ProviderProfileSettings, ProviderSettings, RetrySettings,
    };

    #[test]
    fn empty_settings_pass_validation() {
        let s = NornSettings::default();
        validate_settings(&s).unwrap();
    }

    #[test]
    fn fully_valid_settings_pass() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "fs".to_owned(),
            McpServerSettings {
                transport: Some("stdio".to_owned()),
                command: Some("mcp-fs".to_owned()),
                args: None,
                url: None,
                env: None,
                headers: None,
            },
        );
        let s = NornSettings {
            provider: Some(ProviderSettings {
                timeout: Some("30s".to_owned()),
                ..ProviderSettings::default()
            }),
            agent: Some(AgentSettings {
                step_timeout: Some("1m30s".to_owned()),
                prompt_command_timeout: Some("5s".to_owned()),
                ..AgentSettings::default()
            }),
            retry: Some(RetrySettings {
                base_delay: Some("250ms".to_owned()),
                ..RetrySettings::default()
            }),
            permissions: Some(PermissionSettings {
                allow: Some(vec!["read".to_owned(), "bash(ls *)".to_owned()]),
                deny: Some(vec!["bash(rm -rf*)".to_owned()]),
                ask: Some(vec!["bash(git push*)".to_owned()]),
            }),
            mcp_servers: Some(servers),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn invalid_provider_timeout_caught() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                timeout: Some("not-a-duration".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(
            reason.contains("provider.timeout"),
            "reason missing field: {reason}"
        );
        assert!(
            reason.contains("not-a-duration"),
            "reason missing value: {reason}"
        );
    }

    #[test]
    fn rate_and_retry_duration_knobs_valid_values_pass() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn invalid_rate_limit_interval_caught() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit_interval: Some("not-a-duration".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("provider.rate_limit_interval"),
            "reason missing field: {reason}"
        );
    }

    #[test]
    fn zero_rate_limit_interval_caught() {
        // A zero replenishment window means permits never accumulate over
        // a meaningful interval — reject it like rate_limit = 0.
        let s = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit_interval: Some("0s".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("provider.rate_limit_interval"),
            "reason missing field: {reason}"
        );
        assert!(
            reason.contains("greater than zero"),
            "reason missing zero rejection: {reason}"
        );
    }

    #[test]
    fn invalid_retry_backoff_caught() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                retry_backoff: Some("zzz".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("provider.retry_backoff"),
            "reason missing field: {reason}"
        );
    }

    #[test]
    fn zero_retry_backoff_caught() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                retry_backoff: Some("0ms".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("provider.retry_backoff"),
            "reason missing field: {reason}"
        );
        assert!(
            reason.contains("greater than zero"),
            "reason missing zero rejection: {reason}"
        );
    }

    #[test]
    fn zero_retry_after_ceiling_caught() {
        // A zero ceiling clamps every server-requested wait to nothing,
        // defeating the rate limiter entirely.
        let s = NornSettings {
            provider: Some(ProviderSettings {
                retry_after_ceiling: Some("0s".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("provider.retry_after_ceiling"),
            "reason missing field: {reason}"
        );
        assert!(
            reason.contains("greater than zero"),
            "reason missing zero rejection: {reason}"
        );
    }

    #[test]
    fn invalid_step_timeout_caught() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                step_timeout: Some("forever".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("agent.step_timeout"));
        assert!(reason.contains("forever"));
    }

    #[test]
    fn invalid_prompt_command_timeout_caught() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                prompt_command_timeout: Some("zzz".to_owned()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("agent.prompt_command_timeout"));
    }

    #[test]
    fn invalid_retry_base_delay_caught() {
        let s = NornSettings {
            retry: Some(RetrySettings {
                base_delay: Some("???".to_owned()),
                ..RetrySettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("retry.base_delay"));
        assert!(reason.contains("???"));
    }

    #[test]
    fn model_aliases_with_valid_api_shape_pass() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "55".to_owned(),
            ModelAliasSettings::Model("gpt-5.5".to_owned()),
        );
        aliases.insert(
            "local".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                provider_profile: Some("lmstudio".to_owned()),
                api_shape: Some("openai_chat_completions".to_owned()),
                model: "google/gemma-4-e4b".to_owned(),
            }),
        );
        let s = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn empty_model_alias_target_caught() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "blank".to_owned(),
            ModelAliasSettings::Model(" ".to_owned()),
        );
        let s = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("model_aliases.blank.model"));
        assert!(reason.contains("empty"));
    }

    #[test]
    fn invalid_model_alias_api_shape_caught() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "local".to_owned(),
            ModelAliasSettings::Selection(ModelAliasSelection {
                provider_profile: Some("lmstudio".to_owned()),
                api_shape: Some("not-a-shape".to_owned()),
                model: "google/gemma-4-e4b".to_owned(),
            }),
        );
        let s = NornSettings {
            model_aliases: Some(aliases),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("model_aliases.local.api_shape"));
        assert!(reason.contains("not-a-shape"));
    }

    #[test]
    fn provider_profile_with_valid_api_shape_passes() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "lmstudio".to_owned(),
            ProviderProfileSettings {
                api_shape: Some("openai_chat_completions".to_owned()),
                provider: ProviderSettings {
                    base_url: Some("http://localhost:1234/v1".to_owned()),
                    api_key_env: Some("NORN_OPENAI_COMPAT_API_KEY".to_owned()),
                    timeout: Some("30s".to_owned()),
                    ..ProviderSettings::default()
                },
            },
        );
        let s = NornSettings {
            provider_profiles: Some(profiles),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn provider_profile_invalid_api_shape_caught() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "lmstudio".to_owned(),
            ProviderProfileSettings {
                api_shape: Some("custom_magic".to_owned()),
                provider: ProviderSettings::default(),
            },
        );
        let s = NornSettings {
            provider_profiles: Some(profiles),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("provider_profiles.lmstudio.api_shape"));
        assert!(reason.contains("custom_magic"));
    }

    #[test]
    fn provider_profile_zero_rate_limit_caught() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "local".to_owned(),
            ProviderProfileSettings {
                api_shape: Some("openai_chat_completions".to_owned()),
                provider: ProviderSettings {
                    rate_limit: Some(0),
                    ..ProviderSettings::default()
                },
            },
        );
        let s = NornSettings {
            provider_profiles: Some(profiles),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("provider_profiles.local.rate_limit"));
    }

    #[test]
    fn empty_permission_pattern_caught() {
        let s = NornSettings {
            permissions: Some(PermissionSettings {
                deny: Some(vec![String::new()]),
                ..PermissionSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("permissions.deny"));
        assert!(reason.to_lowercase().contains("empty"));
    }

    #[test]
    fn unbalanced_parens_pattern_caught() {
        let s = NornSettings {
            permissions: Some(PermissionSettings {
                deny: Some(vec!["bash(rm -rf".to_owned()]),
                ..PermissionSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("bash(rm -rf"));
        assert!(reason.to_lowercase().contains("unbalanced"));
    }

    #[test]
    fn closing_before_opening_paren_caught() {
        let s = NornSettings {
            permissions: Some(PermissionSettings {
                allow: Some(vec!["bash)oops(".to_owned()]),
                ..PermissionSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.to_lowercase().contains("unbalanced"));
    }

    #[test]
    fn control_character_pattern_caught() {
        let s = NornSettings {
            permissions: Some(PermissionSettings {
                ask: Some(vec!["bash(\u{0007}x)".to_owned()]),
                ..PermissionSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("permissions.ask"));
        assert!(reason.to_lowercase().contains("control"));
    }

    #[test]
    fn mcp_server_missing_both_command_and_url_caught() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "broken".to_owned(),
            McpServerSettings {
                transport: Some("stdio".to_owned()),
                command: None,
                args: None,
                url: None,
                env: None,
                headers: None,
            },
        );
        let s = NornSettings {
            mcp_servers: Some(servers),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("broken"),
            "reason missing server name: {reason}"
        );
        assert!(reason.contains("command"));
        assert!(reason.contains("url"));
    }

    #[test]
    fn mcp_server_with_command_only_passes() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "stdio".to_owned(),
            McpServerSettings {
                transport: None,
                command: Some("/bin/server".to_owned()),
                args: None,
                url: None,
                env: None,
                headers: None,
            },
        );
        let s = NornSettings {
            mcp_servers: Some(servers),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn mcp_server_with_url_only_passes() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "remote".to_owned(),
            McpServerSettings {
                transport: None,
                command: None,
                args: None,
                url: Some("https://example.com".to_owned()),
                env: None,
                headers: None,
            },
        );
        let s = NornSettings {
            mcp_servers: Some(servers),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn compact_threshold_in_range_passes() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                compact_threshold: Some(0.95),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn compact_threshold_zero_passes() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                compact_threshold: Some(0.0),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn compact_threshold_one_passes() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                compact_threshold: Some(1.0),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn compact_threshold_above_one_caught() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                compact_threshold: Some(5.0),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("compact_threshold"),
            "reason missing field name: {reason}",
        );
    }

    #[test]
    fn compact_threshold_negative_caught() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                compact_threshold: Some(-0.1),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("compact_threshold"),
            "reason missing field name: {reason}",
        );
    }

    #[test]
    fn invalid_conversation_state_caught() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                conversation_state: Some("thread_magic".to_string()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("conversation_state"));
        assert!(reason.contains("thread_magic"));
    }

    #[test]
    fn rate_limit_zero_caught() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit: Some(0),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("provider.rate_limit"),
            "reason missing field name: {reason}",
        );
        assert!(
            reason.contains("greater than 0"),
            "reason missing the constraint text: {reason}",
        );
    }

    #[test]
    fn rate_limit_one_passes() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit: Some(1),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    #[test]
    fn rate_limit_none_passes() {
        let s = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit: None,
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }
}
