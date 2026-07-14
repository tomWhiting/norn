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
//! - **Permission patterns** must parse under the SAME grammar the
//!   enforcement compiler uses
//!   (`crate::config::permissions::parse_permission_pattern`) — any
//!   pattern the matcher would treat as an inert tool-name literal is a
//!   typed error here.
//! - **Hook entries** must carry a non-empty `command` (the required
//!   `timeout` is enforced at the type level — see
//!   [`crate::config::types::HookEntry`]).
//! - **MCP server definitions** must specify at least one of `command` or
//!   `url`; an entry that has neither has no executable shape.
//! - **Numeric ranges** whose zero form is semantically a deadlock:
//!   `provider.rate_limit` (zero permits can never be acquired) and
//!   `agent.index_lock_deadline_ms` (a zero deadline can never acquire
//!   the session-index lock).
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
/// - A permission pattern is rejected by the shared enforcement grammar
///   (`crate::config::permissions::parse_permission_pattern`).
/// - A hook entry's `command` is empty or whitespace-only.
/// - An MCP server definition has neither `command` nor `url`.
pub fn validate_settings(settings: &NornSettings) -> Result<(), ConfigError> {
    validate_durations(settings)?;
    validate_numeric_ranges(settings)?;
    validate_provider_profiles(settings)?;
    validate_model_aliases(settings)?;
    validate_permissions(settings)?;
    validate_hooks(settings)?;
    validate_mcp_servers(settings)?;
    validate_variants(settings)?;
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
        && let Some(mode) = agent.conversation_state.as_deref()
        && !matches!(
            mode,
            "auto" | "manual" | "manual_replay" | "provider_threaded"
        )
    {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "invalid value for agent.conversation_state: {mode} (expected auto, manual, manual_replay, or provider_threaded)",
            ),
        });
    }
    // A zero index-lock deadline expires before the first non-blocking
    // acquisition attempt: a zero deadline can never acquire the lock, so
    // every session open/append would fail with a spurious timeout. Catch
    // it at config time.
    if let Some(agent) = settings.agent.as_ref()
        && agent.index_lock_deadline_ms == Some(0)
    {
        return Err(ConfigError::InvalidConfig {
            reason: "invalid value for agent.index_lock_deadline_ms: 0 (a zero deadline can \
                     never acquire the lock)"
                .to_string(),
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

/// Syntactic check for a Claude-Code-style permission pattern, delegated
/// to `crate::config::permissions::parse_permission_pattern` — the SAME
/// grammar the enforcement compiler uses, so no pattern can validate here
/// and then compile to an inert tool-name literal downstream. Wildcard
/// semantics and tool-name validity remain the matcher's responsibility.
fn check_permission_pattern(field: &str, pattern: &str) -> Result<(), ConfigError> {
    crate::config::permissions::parse_permission_pattern(pattern)
        .map(|_| ())
        .map_err(|err| ConfigError::InvalidConfig {
            reason: format!("invalid permission pattern in {field}: '{pattern}' {err}"),
        })
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// Reject hook entries whose `command` is empty or whitespace-only — a
/// hook without a command has no behaviour, and silently registering one
/// would hide an operator typo. The required `timeout` is enforced at the
/// type level ([`crate::config::types::HookEntry::timeout`] is a plain
/// `u64`), so only the command needs a semantic check here.
///
/// The [`crate::config::types::HookSettings`] value is destructured
/// exhaustively: adding an event slot without extending this walk is a
/// compile error, not a silently unvalidated slot.
fn validate_hooks(settings: &NornSettings) -> Result<(), ConfigError> {
    let Some(hooks) = settings.hooks.as_ref() else {
        return Ok(());
    };
    let crate::config::types::HookSettings {
        pre_tool,
        post_tool,
        post_tool_failure,
        pre_llm,
        post_llm,
        session_event,
        user_prompt,
        stop,
        subagent_start,
        subagent_stop,
        session_start,
        session_end,
        pre_compaction,
    } = hooks;
    let slots: &[(&str, &Option<Vec<crate::config::types::HookEntry>>)] = &[
        ("pre_tool", pre_tool),
        ("post_tool", post_tool),
        ("post_tool_failure", post_tool_failure),
        ("pre_llm", pre_llm),
        ("post_llm", post_llm),
        ("session_event", session_event),
        ("user_prompt", user_prompt),
        ("stop", stop),
        ("subagent_start", subagent_start),
        ("subagent_stop", subagent_stop),
        ("session_start", session_start),
        ("session_end", session_end),
        ("pre_compaction", pre_compaction),
    ];
    for (event_name, slot) in slots {
        let Some(entries) = slot.as_ref() else {
            continue;
        };
        for entry in entries {
            if entry.command.trim().is_empty() {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "hook entry for hooks.{event_name} has an empty command; a hook \
                         without a command has no behaviour",
                    ),
                });
            }
        }
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
        check_nonempty_clean("mcp_servers key", name)?;
        if !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        {
            return Err(ConfigError::InvalidConfig {
                reason: format!(
                    "invalid mcp server name '{name}': use only ASCII letters, digits, '-' or '_'",
                ),
            });
        }
        if def.max_inbound_message_bytes == Some(0) {
            return Err(ConfigError::InvalidConfig {
                reason: format!("mcp server '{name}' max_inbound_message_bytes must be positive",),
            });
        }
        if def.request_timeout_ms == Some(0) {
            return Err(ConfigError::InvalidConfig {
                reason: format!("mcp server '{name}' request_timeout_ms must be positive"),
            });
        }
        if def.enabled == Some(false) && def.command.is_none() && def.url.is_none() {
            continue;
        }
        if def.command.is_some() == def.url.is_some() {
            return Err(ConfigError::InvalidConfig {
                reason: format!("mcp server '{name}' must set exactly one of command or url"),
            });
        }
        match def.transport.as_deref() {
            None | Some("stdio") if def.command.is_some() => {
                let command = def.command.as_deref().unwrap_or_default();
                check_nonempty_clean(&format!("mcp_servers.{name}.command"), command)?;
                if def
                    .headers
                    .as_ref()
                    .is_some_and(|headers| !headers.is_empty())
                {
                    return Err(ConfigError::InvalidConfig {
                        reason: format!(
                            "mcp server '{name}' cannot set headers for stdio transport",
                        ),
                    });
                }
            }
            None | Some("http") if def.url.is_some() => {
                validate_mcp_http_server(name, def)?;
            }
            Some("sse") => {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "mcp server '{name}' selects unsupported transport 'sse'; use 'http'",
                    ),
                });
            }
            Some(transport) => {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "mcp server '{name}' has incompatible or unsupported transport '{transport}'",
                    ),
                });
            }
            None => {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("mcp server '{name}' transport could not be inferred"),
                });
            }
        }
    }
    Ok(())
}

fn validate_mcp_http_server(
    name: &str,
    definition: &crate::config::McpServerSettings,
) -> Result<(), ConfigError> {
    if definition
        .args
        .as_ref()
        .is_some_and(|args| !args.is_empty())
        || definition.env.as_ref().is_some_and(|env| !env.is_empty())
    {
        return Err(ConfigError::InvalidConfig {
            reason: format!("mcp server '{name}' cannot set args or env for HTTP transport"),
        });
    }
    let url = definition.url.as_deref().unwrap_or_default();
    let parsed = url::Url::parse(url).map_err(|_parse_error| ConfigError::InvalidConfig {
        reason: format!("mcp server '{name}' has an invalid URL"),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ConfigError::InvalidConfig {
            reason: format!("mcp server '{name}' URL must use http or https"),
        });
    }
    if let Some(headers) = definition.headers.as_ref() {
        let mut normalized = std::collections::BTreeSet::new();
        for (header, value) in headers {
            let name =
                reqwest::header::HeaderName::from_bytes(header.as_bytes()).map_err(|_error| {
                    ConfigError::InvalidConfig {
                        reason: format!("mcp server '{name}' has an invalid HTTP header name"),
                    }
                })?;
            reqwest::header::HeaderValue::from_str(value).map_err(|_error| {
                ConfigError::InvalidConfig {
                    reason: format!("mcp server '{name}' has an invalid HTTP header value"),
                }
            })?;
            if !normalized.insert(name.as_str().to_owned()) {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "mcp server '{name}' repeats an HTTP header name with different casing",
                    ),
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent variants
// ---------------------------------------------------------------------------

/// Validate the `variants` section: each definition must carry a clean name,
/// at most one prompt source, a non-empty model (when present), non-empty
/// tool-name entries, and a recognised reasoning-effort name (when present).
///
/// This mirrors the catalog build's own guards
/// ([`crate::agent::variants::VariantCatalogError`]) so a defect is a typed
/// config error at the settings boundary rather than a startup failure at
/// assembly — the `prompt`/`prompt_file` conflict and the reasoning-effort name
/// set are checked in BOTH places on purpose (validation is the early,
/// cheap boundary; the catalog is the load-bearing authority that also reads
/// the prompt file). The reasoning-effort name set is derived by round-
/// tripping through [`crate::provider::request::ReasoningEffort`]'s own serde
/// form — the SAME authority the catalog and `runtime_init` parse against —
/// so no second name list can drift out of sync.
fn validate_variants(settings: &NornSettings) -> Result<(), ConfigError> {
    let Some(variants) = settings.variants.as_ref() else {
        return Ok(());
    };
    for (name, variant) in variants {
        check_nonempty_clean("variants key", name)?;
        if variant.prompt.is_some() && variant.prompt_file.is_some() {
            return Err(ConfigError::InvalidConfig {
                reason: format!(
                    "variant '{name}': prompt and prompt_file are mutually exclusive — set one",
                ),
            });
        }
        if let Some(model) = variant.model.as_deref() {
            check_nonempty_clean(&format!("variants.{name}.model"), model)?;
        }
        if let Some(tools) = variant.tools.as_ref() {
            for tool in tools {
                check_nonempty_clean(&format!("variants.{name}.tools"), tool)?;
            }
        }
        if let Some(effort) = variant.reasoning_effort.as_deref() {
            check_reasoning_effort(&format!("variants.{name}.reasoning_effort"), effort)?;
        }
    }
    Ok(())
}

/// Syntactic check for a reasoning-effort name, delegated to
/// [`crate::provider::request::ReasoningEffort`]'s serde form. The value is
/// lower-cased before the round-trip so validation accepts the same case-
/// insensitive spellings the catalog build and `runtime_init`'s parser do,
/// without hand-maintaining a second name list.
fn check_reasoning_effort(field: &str, value: &str) -> Result<(), ConfigError> {
    let lowered = value.to_lowercase();
    let parsed = serde_json::from_value::<crate::provider::request::ReasoningEffort>(
        serde_json::Value::String(lowered),
    );
    if parsed.is_ok() {
        return Ok(());
    }
    Err(ConfigError::InvalidConfig {
        reason: format!(
            "invalid value for {field}: '{value}' \
             (expected one of: none, low, medium, high, xhigh, max)",
        ),
    })
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
                enabled: None,
                transport: Some("stdio".to_owned()),
                command: Some("mcp-fs".to_owned()),
                args: None,
                url: None,
                env: None,
                headers: None,
                max_inbound_message_bytes: Some(1024),
                request_timeout_ms: Some(5000),
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
    fn zero_index_lock_deadline_caught() {
        // A zero deadline expires before the first acquisition attempt —
        // it can never take the lock, so every session open would fail
        // with a spurious timeout.
        let s = NornSettings {
            agent: Some(AgentSettings {
                index_lock_deadline_ms: Some(0),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("agent.index_lock_deadline_ms"),
            "reason missing field: {reason}"
        );
        assert!(
            reason.contains("a zero deadline can never acquire the lock"),
            "reason missing zero rejection: {reason}"
        );
    }

    #[test]
    fn nonzero_index_lock_deadline_passes() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                index_lock_deadline_ms: Some(10_000),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
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

    /// The review's exact reproduction: `"bash(rm *) "` used to pass
    /// validation and then compile to an inert tool-name literal in the
    /// enforcement parser. Validation now shares that parser, so the
    /// trailing space is a typed error at the settings boundary.
    #[test]
    fn trailing_space_after_closing_paren_caught() {
        let s = NornSettings {
            permissions: Some(PermissionSettings {
                deny: Some(vec!["bash(rm *) ".to_owned()]),
                ..PermissionSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("permissions.deny"), "{reason}");
        assert!(reason.contains("bash(rm *) "), "{reason}");
        assert!(
            reason.contains("whitespace"),
            "reason must explain the defect: {reason}"
        );
    }

    /// Empty argument patterns and trailing text after the closing paren
    /// compile to rules that can never match — both are now typed errors
    /// via the shared grammar.
    #[test]
    fn inert_pattern_shapes_caught() {
        for pattern in ["bash()", "bash(x)y", "(x)"] {
            let s = NornSettings {
                permissions: Some(PermissionSettings {
                    allow: Some(vec![pattern.to_owned()]),
                    ..PermissionSettings::default()
                }),
                ..NornSettings::default()
            };
            let err = validate_settings(&s).expect_err("inert pattern shape must be rejected");
            let ConfigError::InvalidConfig { reason } = err else {
                panic!("expected InvalidConfig variant");
            };
            assert!(reason.contains(pattern), "{reason}");
        }
    }

    #[test]
    fn hook_entry_with_empty_command_caught() {
        use crate::config::types::{HookEntry, HookSettings};
        for command in ["", "   "] {
            let s = NornSettings {
                hooks: Some(HookSettings {
                    stop: Some(vec![HookEntry {
                        matcher: None,
                        command: command.to_owned(),
                        timeout: 5,
                    }]),
                    ..HookSettings::default()
                }),
                ..NornSettings::default()
            };
            let err = validate_settings(&s).expect_err("empty command must be rejected");
            let ConfigError::InvalidConfig { reason } = err else {
                panic!("expected InvalidConfig variant");
            };
            assert!(reason.contains("hooks.stop"), "{reason}");
            assert!(reason.contains("empty command"), "{reason}");
        }
    }

    #[test]
    fn hook_entry_with_command_passes_validation() {
        use crate::config::types::{HookEntry, HookSettings};
        let s = NornSettings {
            hooks: Some(HookSettings {
                pre_tool: Some(vec![HookEntry {
                    matcher: Some("Write".to_owned()),
                    command: "lint.sh".to_owned(),
                    timeout: 5,
                }]),
                ..HookSettings::default()
            }),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    /// The rejection message must enumerate every accepted value —
    /// `manual_replay` was previously omitted even though it is accepted.
    #[test]
    fn conversation_state_error_lists_manual_replay() {
        let s = NornSettings {
            agent: Some(AgentSettings {
                conversation_state: Some("bogus".to_string()),
                ..AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("manual_replay"),
            "message must list manual_replay: {reason}"
        );
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
                enabled: None,
                transport: Some("stdio".to_owned()),
                command: None,
                args: None,
                url: None,
                env: None,
                headers: None,
                max_inbound_message_bytes: None,
                request_timeout_ms: None,
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
                enabled: None,
                transport: None,
                command: Some("/bin/server".to_owned()),
                args: None,
                url: None,
                env: None,
                headers: None,
                max_inbound_message_bytes: None,
                request_timeout_ms: None,
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
                enabled: None,
                transport: None,
                command: None,
                args: None,
                url: Some("https://example.com".to_owned()),
                env: None,
                headers: None,
                max_inbound_message_bytes: None,
                request_timeout_ms: None,
            },
        );
        let s = NornSettings {
            mcp_servers: Some(servers),
            ..NornSettings::default()
        };
        validate_settings(&s).unwrap();
    }

    /// The reserve knob accepts any non-negative reserve, the explicit
    /// `off` disable, and unset: all validate. A misconfigured reserve (e.g.
    /// at or above the window) is not a config error — the loop's
    /// `maybe_auto_compact` handles it at the trigger by warning and
    /// disabling, so validation must not reject it.
    #[test]
    fn auto_compact_reserve_tokens_accepts_any_value() {
        use crate::config::AutoCompactReserve;
        for reserve in [
            Some(AutoCompactReserve::Tokens(0)),
            Some(AutoCompactReserve::Tokens(30_000)),
            Some(AutoCompactReserve::Tokens(10_000_000)),
            Some(AutoCompactReserve::Off),
            None,
        ] {
            let s = NornSettings {
                agent: Some(AgentSettings {
                    auto_compact_reserve_tokens: reserve,
                    ..AgentSettings::default()
                }),
                ..NornSettings::default()
            };
            validate_settings(&s).unwrap();
        }
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

    // -----------------------------------------------------------------------
    // Agent variants
    // -----------------------------------------------------------------------

    fn variant_settings_map(
        name: &str,
        variant: crate::config::types::VariantSettings,
    ) -> NornSettings {
        let mut variants = BTreeMap::new();
        variants.insert(name.to_owned(), variant);
        NornSettings {
            variants: Some(variants),
            ..NornSettings::default()
        }
    }

    #[test]
    fn variant_fully_valid_passes() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map(
            "scout",
            VariantSettings {
                description: Some("scouting".to_owned()),
                prompt: Some("Scout the area.".to_owned()),
                tools: Some(vec!["read".to_owned(), "search".to_owned()]),
                model: Some("some-model".to_owned()),
                reasoning_effort: Some("XHigh".to_owned()),
                ..VariantSettings::default()
            },
        );
        validate_settings(&s).unwrap();
    }

    #[test]
    fn variant_prompt_file_only_passes() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map(
            "scout",
            VariantSettings {
                prompt_file: Some("scout.md".to_owned()),
                ..VariantSettings::default()
            },
        );
        // A prompt_file that does not exist is NOT a config-validate error —
        // the catalog build is the authority that reads it (fail-loud at
        // assembly). Validation only rejects the prompt/prompt_file conflict.
        validate_settings(&s).unwrap();
    }

    #[test]
    fn variant_empty_name_caught() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map("   ", VariantSettings::default());
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("variants key"), "{reason}");
        assert!(reason.to_lowercase().contains("empty"), "{reason}");
    }

    #[test]
    fn variant_control_char_name_caught() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map("sco\u{0007}ut", VariantSettings::default());
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.to_lowercase().contains("control"), "{reason}");
    }

    #[test]
    fn variant_prompt_and_prompt_file_conflict_caught() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map(
            "clash",
            VariantSettings {
                prompt: Some("inline".to_owned()),
                prompt_file: Some("also-a-file.md".to_owned()),
                ..VariantSettings::default()
            },
        );
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("clash"),
            "reason must name the variant: {reason}"
        );
        assert!(
            reason.contains("mutually exclusive"),
            "reason must explain the conflict: {reason}",
        );
    }

    #[test]
    fn variant_empty_model_caught() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map(
            "scout",
            VariantSettings {
                model: Some("   ".to_owned()),
                ..VariantSettings::default()
            },
        );
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("variants.scout.model"), "{reason}");
        assert!(reason.to_lowercase().contains("empty"), "{reason}");
    }

    #[test]
    fn variant_empty_tool_entry_caught() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map(
            "scout",
            VariantSettings {
                tools: Some(vec!["read".to_owned(), String::new()]),
                ..VariantSettings::default()
            },
        );
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(reason.contains("variants.scout.tools"), "{reason}");
        assert!(reason.to_lowercase().contains("empty"), "{reason}");
    }

    #[test]
    fn variant_unknown_reasoning_effort_caught() {
        use crate::config::types::VariantSettings;
        let s = variant_settings_map(
            "hasty",
            VariantSettings {
                reasoning_effort: Some("turbo".to_owned()),
                ..VariantSettings::default()
            },
        );
        let err = validate_settings(&s).unwrap_err();
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig variant");
        };
        assert!(
            reason.contains("variants.hasty.reasoning_effort"),
            "{reason}",
        );
        assert!(
            reason.contains("turbo"),
            "reason must name the value: {reason}"
        );
        assert!(
            reason.contains("none, low, medium, high, xhigh, max"),
            "reason must list the accepted set: {reason}",
        );
    }

    /// The accepted effort names are matched case-insensitively (the same
    /// authority the catalog and `runtime_init` parse against).
    #[test]
    fn variant_reasoning_effort_is_case_insensitive() {
        use crate::config::types::VariantSettings;
        for effort in ["none", "LOW", "Medium", "high", "xHigh", "MAX"] {
            let s = variant_settings_map(
                "scout",
                VariantSettings {
                    reasoning_effort: Some(effort.to_owned()),
                    ..VariantSettings::default()
                },
            );
            validate_settings(&s)
                .unwrap_or_else(|err| panic!("effort '{effort}' must validate: {err:?}"));
        }
    }
}
