//! Runtime configuration assembly for the Norn CLI (NC-004 R2 / R3 / R8).
//!
//! Lives at the layer between [`clap`] argument parsing and the
//! [`norn::r#loop::loop_context::LoopContext`] / `AgentLoopConfig` /
//! `ProviderConfig` triplet that [`norn::r#loop::runner::run_agent_step`]
//! consumes. The functions here:
//!
//! - Parse `KEY=VALUE` pairs and human-readable duration strings.
//! - Translate `-c key=value` overrides into the typed [`ConfigOverrides`]
//!   struct that downstream callers fold onto `AgentLoopConfig`,
//!   [`ProviderConfigOverrides`], and `RetryPolicy`.
//! - Hold the typed override surface for provider configuration that
//!   NC-003 will pick up when it constructs the [`norn::provider::request::ProviderConfig`].
//!
//! No I/O happens here — every helper is pure and side-effect free except
//! for the `tracing::warn!` emitted for unknown `-c` keys per the NC20
//! mapping table.

use std::path::PathBuf;
use std::time::Duration;

use norn::r#loop::config::ConversationStateMode;
use serde_json::Value;

use crate::cli::BuildError;

/// Parse a `KEY=VALUE` flag value, trimming whitespace around both halves.
///
/// Empty keys or missing `=` separators are rejected with a
/// [`BuildError::Argument`] describing the offending input.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `input` does not contain an `=`
/// or the trimmed key is empty.
pub fn parse_kv(input: &str) -> Result<(String, String), BuildError> {
    let Some((key, value)) = input.split_once('=') else {
        return Err(BuildError::Argument(format!(
            "expected KEY=VALUE, got '{input}'",
        )));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(BuildError::Argument(format!(
            "empty key in KEY=VALUE pair '{input}'",
        )));
    }
    Ok((key.to_owned(), value.trim().to_owned()))
}

/// Parse a human-readable duration string (e.g. `30s`, `2m`, `1h`,
/// `100ms`) via [`humantime::parse_duration`].
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when `input` does not parse as a
/// duration, embedding the original input for diagnostic context.
pub fn parse_duration(input: &str) -> Result<Duration, BuildError> {
    humantime::parse_duration(input).map_err(|err| {
        BuildError::Argument(format!(
            "invalid duration '{input}': {err} (examples: 30s, 2m, 1h, 100ms)",
        ))
    })
}

/// Typed `-c key=value` overrides drawn from the NC20 mapping table.
///
/// Each field corresponds to a row in DESIGN.md NC20 and stays `Option`
/// so callers can fold only the values the user actually set. The struct
/// is consumed by [`crate::runtime::build_runtime`] (R8) and split across
/// `AgentLoopConfig`, [`ProviderConfigOverrides`], and `RetryPolicy`.
#[derive(Debug, Default, Clone)]
pub struct ConfigOverrides {
    // -- AgentLoopConfig fields ------------------------------------------
    /// `-c timeout=<duration>` → [`AgentLoopConfig::step_timeout`].
    pub timeout: Option<Duration>,
    /// `-c max_turns=<u32>` → [`AgentLoopConfig::max_iterations`].
    pub max_turns: Option<u32>,
    /// `-c schema_budget=<u32>` → [`AgentLoopConfig::schema_attempt_budget`].
    pub schema_budget: Option<u32>,
    /// `-c context_window=<u64>` → [`AgentLoopConfig::context_window_limit`].
    pub context_window: Option<u64>,
    /// `-c compact_threshold=<f64>` → [`AgentLoopConfig::auto_compact_threshold_pct`].
    pub compact_threshold: Option<f64>,
    /// `-c compact_keep_turns=<usize>` → [`AgentLoopConfig::auto_compact_keep_recent_turns`].
    pub compact_keep_turns: Option<usize>,
    /// `-c conversation_state=<auto|manual|provider_threaded>` → conversation policy.
    pub conversation_state: Option<ConversationStateMode>,
    /// `-c server_compaction_threshold_tokens=<u64>` → provider compaction threshold.
    pub server_compaction_threshold_tokens: Option<u64>,

    // -- ProviderConfig fields -------------------------------------------
    /// `-c base_url=<string>` → [`ProviderConfig::base_url`].
    pub base_url: Option<String>,
    /// `-c max_retries=<u32>` → [`ProviderConfig::max_retries`].
    pub max_retries: Option<u32>,
    /// `-c request_timeout=<duration>` → [`ProviderConfig::timeout`].
    pub request_timeout: Option<Duration>,
    /// `-c provider_options=<json>` → [`ProviderConfig::provider_options`].
    pub provider_options: Option<Value>,
    /// `-c rate_limit_interval=<duration>` → `ProviderConfig::rate_limit_interval`
    /// (replenishment window for the provider rate limiter; `None` defers
    /// to the library's owner-approved 60s default).
    pub rate_limit_interval: Option<Duration>,
    /// `-c retry_backoff=<duration>` → `ProviderConfig::retry_backoff`
    /// (backoff for a 429 without a parseable `Retry-After`; `None`
    /// defers to the library's owner-approved 1s default).
    pub retry_backoff: Option<Duration>,
    /// `-c retry_after_ceiling=<duration>` → `ProviderConfig::retry_after_ceiling`
    /// (optional cap on honored `Retry-After` waits; `None` honors the
    /// header as-is per the library's deliberate no-ceiling default).
    pub retry_after_ceiling: Option<Duration>,

    // -- RetryPolicy fields ----------------------------------------------
    /// `-c retry_max=<u32>` → [`RetryPolicy::max_retries`].
    pub retry_max: Option<u32>,
    /// `-c retry_base_delay=<duration>` → [`RetryPolicy::initial_backoff`].
    pub retry_base_delay: Option<Duration>,

    // -- Per-tool override fields ----------------------------------------
    /// `-c write.max_code_lines=<usize>` → [`LengthLimit::default`] for the
    /// `Write` tool. Takes precedence over the profile-supplied value when
    /// set.
    pub write_max_code_lines: Option<usize>,

    /// `-c debug_api=<path>` or `--debug-api` → raw API dump directory.
    pub debug_dump_dir: Option<PathBuf>,
}

/// Provider-config override surface separated from [`norn::provider::request::ProviderConfig`]
/// because the latter requires a mandatory `auth_source` constructed by
/// NC-003. NC-004 collects these values and hands them off; NC-003 folds
/// them onto the constructed [`norn::provider::request::ProviderConfig`].
#[derive(Debug, Default, Clone)]
pub struct ProviderConfigOverrides {
    /// `-c base_url=<string>`.
    pub base_url: Option<String>,
    /// `-c max_retries=<u32>`.
    pub max_retries: Option<u32>,
    /// `-c request_timeout=<duration>`.
    pub request_timeout: Option<Duration>,
    /// `-c provider_options=<json>`.
    pub provider_options: Option<Value>,
    /// Base directory from `--debug-api` or `-c debug_api=<path>`.
    pub debug_dump_dir: Option<PathBuf>,
    /// Resolved JSONL file path (`{dir}/{session_id}.jsonl`), set by
    /// the REPL driver or print orchestrator after session creation.
    pub debug_dump_file: Option<PathBuf>,
    /// Permits-per-minute granted by the provider's rate limiter,
    /// sourced from `settings.provider.rate_limit` (NC-005 R4). `None`
    /// falls back to the provider-specific compiled default.
    pub rate_limit: Option<u32>,
    /// Replenishment window for the provider rate limiter, sourced from
    /// `settings.provider.rate_limit_interval` and overridden by
    /// `-c rate_limit_interval=<duration>`. `None` defers to the
    /// library's owner-approved 60-second default.
    pub rate_limit_interval: Option<Duration>,
    /// Backoff for a 429 response without a parseable `Retry-After`
    /// header, sourced from `settings.provider.retry_backoff` and
    /// overridden by `-c retry_backoff=<duration>`. `None` defers to the
    /// library's owner-approved 1-second default.
    pub retry_backoff: Option<Duration>,
    /// Optional cap on honored `Retry-After` waits, sourced from
    /// `settings.provider.retry_after_ceiling` and overridden by
    /// `-c retry_after_ceiling=<duration>`. `None` honors the header
    /// as-is.
    pub retry_after_ceiling: Option<Duration>,
    /// Claude Runner binary path, sourced from
    /// `settings.provider.runner_path`. `None` falls back to the
    /// documented default lookup of `"claude"` on `PATH` (see
    /// `print/provider.rs::build_provider`). Only the Claude-Runner
    /// backend reads it; there is deliberately no `-c` surface.
    pub runner_path: Option<PathBuf>,
}

impl ConfigOverrides {
    /// Parse every `-c key=value` pair, dispatching by key per the NC20
    /// mapping table. Unknown keys emit a `tracing::warn!` and are
    /// ignored; invalid values surface as [`BuildError::Argument`] naming
    /// the key, expected type, and actual value.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::Argument`] when any value fails to parse for
    /// its expected type, or when a `KEY=VALUE` pair is malformed per
    /// [`parse_kv`].
    pub fn parse(pairs: &[String]) -> Result<Self, BuildError> {
        let mut overrides = Self::default();
        for pair in pairs {
            let (key, value) = parse_kv(pair)?;
            overrides.apply_pair(&key, &value)?;
        }
        Ok(overrides)
    }

    /// Project the provider-only subset of overrides for NC-003 to fold
    /// onto [`norn::provider::request::ProviderConfig`].
    #[must_use]
    pub fn provider_overrides(&self) -> ProviderConfigOverrides {
        ProviderConfigOverrides {
            base_url: self.base_url.clone(),
            max_retries: self.max_retries,
            request_timeout: self.request_timeout,
            provider_options: self.provider_options.clone(),
            debug_dump_dir: self.debug_dump_dir.clone(),
            debug_dump_file: None,
            rate_limit: None,
            rate_limit_interval: self.rate_limit_interval,
            retry_backoff: self.retry_backoff,
            retry_after_ceiling: self.retry_after_ceiling,
            // `runner_path` is settings-only (no `-c` surface), so the
            // `-c`-derived projection never carries it.
            runner_path: None,
        }
    }

    fn apply_pair(&mut self, key: &str, value: &str) -> Result<(), BuildError> {
        match key {
            "timeout" => {
                self.timeout = Some(parse_duration(value)?);
            }
            "max_turns" => {
                self.max_turns = Some(parse_typed::<u32>(key, "u32", value)?);
            }
            "schema_budget" => {
                self.schema_budget = Some(parse_typed::<u32>(key, "u32", value)?);
            }
            "context_window" => {
                self.context_window = Some(parse_typed::<u64>(key, "u64", value)?);
            }
            "compact_threshold" => {
                self.compact_threshold = Some(parse_typed::<f64>(key, "f64", value)?);
            }
            "compact_keep_turns" => {
                self.compact_keep_turns = Some(parse_typed::<usize>(key, "usize", value)?);
            }
            "conversation_state" => {
                self.conversation_state = Some(parse_conversation_state(value)?);
            }
            "server_compaction_threshold_tokens" => {
                self.server_compaction_threshold_tokens =
                    Some(parse_typed::<u64>(key, "u64", value)?);
            }
            "base_url" => {
                self.base_url = Some(value.to_owned());
            }
            "max_retries" => {
                self.max_retries = Some(parse_typed::<u32>(key, "u32", value)?);
            }
            "request_timeout" => {
                self.request_timeout = Some(parse_duration(value)?);
            }
            "rate_limit_interval" => {
                self.rate_limit_interval = Some(parse_duration(value)?);
            }
            "retry_backoff" => {
                self.retry_backoff = Some(parse_duration(value)?);
            }
            "retry_after_ceiling" => {
                self.retry_after_ceiling = Some(parse_duration(value)?);
            }
            "retry_max" => {
                self.retry_max = Some(parse_typed::<u32>(key, "u32", value)?);
            }
            "retry_base_delay" => {
                self.retry_base_delay = Some(parse_duration(value)?);
            }
            "provider_options" => {
                let parsed: Value = serde_json::from_str(value).map_err(|err| {
                    BuildError::Argument(format!(
                        "invalid value for provider_options: expected JSON, got '{value}': {err}",
                    ))
                })?;
                self.provider_options = Some(parsed);
            }
            "write.max_code_lines" => {
                self.write_max_code_lines = Some(parse_typed::<usize>(key, "usize", value)?);
            }
            "debug_api" => {
                self.debug_dump_dir = Some(PathBuf::from(value));
            }
            unknown => {
                tracing::warn!(
                    key = unknown,
                    value,
                    "unknown -c override key; ignoring (see DESIGN.md NC20 for the supported keys)",
                );
            }
        }
        Ok(())
    }
}

fn parse_conversation_state(value: &str) -> Result<ConversationStateMode, BuildError> {
    match value {
        "auto" => Ok(ConversationStateMode::Auto),
        "manual" | "manual_replay" => Ok(ConversationStateMode::ManualReplay),
        "provider_threaded" => Ok(ConversationStateMode::ProviderThreaded),
        other => Err(BuildError::Argument(format!(
            "invalid value for conversation_state: expected auto, manual, or provider_threaded, got '{other}'",
        ))),
    }
}

fn parse_typed<T>(key: &str, expected: &str, value: &str) -> Result<T, BuildError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value.parse::<T>().map_err(|err| {
        BuildError::Argument(format!(
            "invalid value for {key}: expected {expected}, got '{value}' ({err})",
        ))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_kv_splits_on_first_equals() {
        let (key, value) = parse_kv("project=yggdrasil=alpha").unwrap();
        assert_eq!(key, "project");
        assert_eq!(value, "yggdrasil=alpha");
    }

    #[test]
    fn parse_kv_trims_whitespace() {
        let (key, value) = parse_kv("  env  =  staging  ").unwrap();
        assert_eq!(key, "env");
        assert_eq!(value, "staging");
    }

    #[test]
    fn parse_kv_rejects_missing_equals() {
        let err = parse_kv("noequals").unwrap_err();
        match err {
            BuildError::Argument(reason) => assert!(reason.contains("noequals")),
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn parse_kv_rejects_empty_key() {
        let err = parse_kv("=value").unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn parse_duration_accepts_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_accepts_minutes() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_mins(2));
    }

    #[test]
    fn parse_duration_accepts_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_hours(1));
    }

    #[test]
    fn parse_duration_accepts_milliseconds() {
        assert_eq!(parse_duration("100ms").unwrap(), Duration::from_millis(100),);
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        let err = parse_duration("not-a-duration").unwrap_err();
        match err {
            BuildError::Argument(reason) => assert!(reason.contains("not-a-duration")),
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn overrides_default_has_all_none() {
        let overrides = ConfigOverrides::default();
        assert!(overrides.timeout.is_none());
        assert!(overrides.max_turns.is_none());
        assert!(overrides.schema_budget.is_none());
        assert!(overrides.context_window.is_none());
        assert!(overrides.compact_threshold.is_none());
        assert!(overrides.compact_keep_turns.is_none());
        assert!(overrides.conversation_state.is_none());
        assert!(overrides.server_compaction_threshold_tokens.is_none());
        assert!(overrides.base_url.is_none());
        assert!(overrides.max_retries.is_none());
        assert!(overrides.request_timeout.is_none());
        assert!(overrides.provider_options.is_none());
        assert!(overrides.retry_max.is_none());
        assert!(overrides.retry_base_delay.is_none());
        assert!(overrides.write_max_code_lines.is_none());
        assert!(overrides.debug_dump_dir.is_none());
        assert!(overrides.rate_limit_interval.is_none());
        assert!(overrides.retry_backoff.is_none());
        assert!(overrides.retry_after_ceiling.is_none());
    }

    #[test]
    fn parse_rate_limit_interval_sets_value() {
        let overrides = ConfigOverrides::parse(&["rate_limit_interval=30s".to_owned()]).unwrap();
        assert_eq!(overrides.rate_limit_interval, Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_retry_backoff_sets_value() {
        let overrides = ConfigOverrides::parse(&["retry_backoff=250ms".to_owned()]).unwrap();
        assert_eq!(overrides.retry_backoff, Some(Duration::from_millis(250)));
    }

    #[test]
    fn parse_retry_after_ceiling_sets_value() {
        let overrides = ConfigOverrides::parse(&["retry_after_ceiling=90s".to_owned()]).unwrap();
        assert_eq!(overrides.retry_after_ceiling, Some(Duration::from_secs(90)));
    }

    #[test]
    fn parse_retry_after_ceiling_rejects_garbage() {
        let err =
            ConfigOverrides::parse(&["retry_after_ceiling=not-a-duration".to_owned()]).unwrap_err();
        assert!(matches!(err, BuildError::Argument(_)));
    }

    #[test]
    fn provider_rate_and_retry_overrides_flow_into_provider_overrides() {
        let overrides = ConfigOverrides::parse(&[
            "rate_limit_interval=45s".to_owned(),
            "retry_backoff=2s".to_owned(),
            "retry_after_ceiling=1m".to_owned(),
        ])
        .unwrap();
        let provider = overrides.provider_overrides();
        assert_eq!(provider.rate_limit_interval, Some(Duration::from_secs(45)));
        assert_eq!(provider.retry_backoff, Some(Duration::from_secs(2)));
        assert_eq!(provider.retry_after_ceiling, Some(Duration::from_mins(1)));
    }

    #[test]
    fn parse_timeout_sets_step_timeout() {
        let overrides = ConfigOverrides::parse(&["timeout=30s".to_owned()]).unwrap();
        assert_eq!(overrides.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_max_turns_sets_value() {
        let overrides = ConfigOverrides::parse(&["max_turns=5".to_owned()]).unwrap();
        assert_eq!(overrides.max_turns, Some(5));
    }

    #[test]
    fn parse_schema_budget_sets_value() {
        let overrides = ConfigOverrides::parse(&["schema_budget=10".to_owned()]).unwrap();
        assert_eq!(overrides.schema_budget, Some(10));
    }

    #[test]
    fn parse_context_window_sets_value() {
        let overrides = ConfigOverrides::parse(&["context_window=200000".to_owned()]).unwrap();
        assert_eq!(overrides.context_window, Some(200_000));
    }

    #[test]
    fn parse_compact_threshold_sets_value() {
        let overrides = ConfigOverrides::parse(&["compact_threshold=0.75".to_owned()]).unwrap();
        assert!((overrides.compact_threshold.unwrap() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_compact_keep_turns_sets_value() {
        let overrides = ConfigOverrides::parse(&["compact_keep_turns=5".to_owned()]).unwrap();
        assert_eq!(overrides.compact_keep_turns, Some(5));
    }

    #[test]
    fn parse_conversation_state_sets_value() {
        let overrides =
            ConfigOverrides::parse(&["conversation_state=provider_threaded".to_owned()]).unwrap();
        assert_eq!(
            overrides.conversation_state,
            Some(ConversationStateMode::ProviderThreaded),
        );
    }

    #[test]
    fn parse_conversation_state_auto_sets_value() {
        let overrides = ConfigOverrides::parse(&["conversation_state=auto".to_owned()]).unwrap();
        assert_eq!(
            overrides.conversation_state,
            Some(ConversationStateMode::Auto),
        );
    }

    #[test]
    fn parse_server_compaction_threshold_sets_value() {
        let overrides =
            ConfigOverrides::parse(&["server_compaction_threshold_tokens=200000".to_owned()])
                .unwrap();
        assert_eq!(overrides.server_compaction_threshold_tokens, Some(200_000),);
    }

    #[test]
    fn parse_base_url_sets_value() {
        let overrides =
            ConfigOverrides::parse(&["base_url=http://localhost:8080".to_owned()]).unwrap();
        assert_eq!(overrides.base_url.as_deref(), Some("http://localhost:8080"));
    }

    #[test]
    fn parse_max_retries_is_http_layer() {
        let overrides = ConfigOverrides::parse(&["max_retries=3".to_owned()]).unwrap();
        assert_eq!(overrides.max_retries, Some(3));
        assert_eq!(
            overrides.retry_max, None,
            "max_retries and retry_max are distinct layers",
        );
    }

    #[test]
    fn parse_request_timeout_sets_value() {
        let overrides = ConfigOverrides::parse(&["request_timeout=10s".to_owned()]).unwrap();
        assert_eq!(overrides.request_timeout, Some(Duration::from_secs(10)));
    }

    #[test]
    fn parse_retry_max_is_loop_layer() {
        let overrides = ConfigOverrides::parse(&["retry_max=4".to_owned()]).unwrap();
        assert_eq!(overrides.retry_max, Some(4));
        assert_eq!(
            overrides.max_retries, None,
            "retry_max and max_retries are distinct layers",
        );
    }

    #[test]
    fn parse_retry_base_delay_sets_value() {
        let overrides = ConfigOverrides::parse(&["retry_base_delay=2s".to_owned()]).unwrap();
        assert_eq!(overrides.retry_base_delay, Some(Duration::from_secs(2)));
    }

    #[test]
    fn parse_provider_options_accepts_inline_json() {
        let overrides =
            ConfigOverrides::parse(&[r#"provider_options={"k":"v"}"#.to_owned()]).unwrap();
        let parsed = overrides.provider_options.expect("provider_options set");
        assert_eq!(parsed.get("k").and_then(Value::as_str), Some("v"));
    }

    #[test]
    fn parse_provider_options_rejects_invalid_json() {
        let err = ConfigOverrides::parse(&["provider_options=not-json".to_owned()]).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("provider_options"));
                assert!(reason.contains("JSON"));
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_key_is_ignored() {
        // Should not error — just warn and skip.
        let overrides = ConfigOverrides::parse(&["unknown_key=value".to_owned()]).unwrap();
        assert!(overrides.timeout.is_none());
    }

    #[test]
    fn parse_invalid_max_turns_names_key_and_type_and_value() {
        let err = ConfigOverrides::parse(&["max_turns=abc".to_owned()]).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("max_turns"), "reason: {reason}");
                assert!(reason.contains("u32"), "reason: {reason}");
                assert!(reason.contains("abc"), "reason: {reason}");
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn provider_overrides_extracts_provider_only_fields() {
        let overrides = ConfigOverrides::parse(&[
            "base_url=http://x".to_owned(),
            "max_retries=2".to_owned(),
            "request_timeout=5s".to_owned(),
            "max_turns=10".to_owned(),
        ])
        .unwrap();
        let provider = overrides.provider_overrides();
        assert_eq!(provider.base_url.as_deref(), Some("http://x"));
        assert_eq!(provider.max_retries, Some(2));
        assert_eq!(provider.request_timeout, Some(Duration::from_secs(5)));
        // max_turns is NOT a provider field; it stays on the parent.
        assert_eq!(overrides.max_turns, Some(10));
    }

    #[test]
    fn parse_write_max_code_lines_sets_value() {
        let overrides = ConfigOverrides::parse(&["write.max_code_lines=800".to_owned()]).unwrap();
        assert_eq!(overrides.write_max_code_lines, Some(800));
    }

    #[test]
    fn parse_write_max_code_lines_rejects_non_numeric() {
        let err = ConfigOverrides::parse(&["write.max_code_lines=abc".to_owned()]).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(reason.contains("write.max_code_lines"), "reason: {reason}");
                assert!(reason.contains("usize"), "reason: {reason}");
                assert!(reason.contains("abc"), "reason: {reason}");
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_write_subkey_is_ignored() {
        // Unknown `write.*` keys must fall through to the generic
        // unknown-key warn arm, not surface as an error.
        let overrides = ConfigOverrides::parse(&["write.unknown_key=foo".to_owned()]).unwrap();
        assert!(overrides.write_max_code_lines.is_none());
    }

    #[test]
    fn parse_multiple_overrides_combine_correctly() {
        let overrides = ConfigOverrides::parse(&[
            "timeout=30s".to_owned(),
            "max_turns=5".to_owned(),
            "schema_budget=8".to_owned(),
        ])
        .unwrap();
        assert_eq!(overrides.timeout, Some(Duration::from_secs(30)));
        assert_eq!(overrides.max_turns, Some(5));
        assert_eq!(overrides.schema_budget, Some(8));
    }

    #[test]
    fn parse_debug_api_sets_dump_dir() {
        let overrides = ConfigOverrides::parse(&["debug_api=/tmp/norn-debug".to_owned()]).unwrap();
        assert_eq!(
            overrides.debug_dump_dir,
            Some(PathBuf::from("/tmp/norn-debug")),
        );
    }

    #[test]
    fn debug_api_flows_into_provider_overrides() {
        let overrides = ConfigOverrides::parse(&["debug_api=/tmp/dump".to_owned()]).unwrap();
        let provider = overrides.provider_overrides();
        assert_eq!(provider.debug_dump_dir, Some(PathBuf::from("/tmp/dump")));
    }
}
