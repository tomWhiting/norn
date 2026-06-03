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
    validate_permissions(settings)?;
    validate_mcp_servers(settings)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------

fn validate_durations(settings: &NornSettings) -> Result<(), ConfigError> {
    if let Some(provider) = settings.provider.as_ref()
        && let Some(timeout) = provider.timeout.as_deref()
    {
        check_duration("provider.timeout", timeout)?;
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

fn check_duration(field: &str, value: &str) -> Result<(), ConfigError> {
    humantime::parse_duration(value)
        .map(|_| ())
        .map_err(|err| ConfigError::InvalidConfig {
            reason: format!("invalid duration for {field}: '{value}' ({err})"),
        })
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
        AgentSettings, McpServerSettings, PermissionSettings, ProviderSettings, RetrySettings,
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
