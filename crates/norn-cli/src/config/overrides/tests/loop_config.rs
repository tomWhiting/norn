use std::time::Duration;

use super::super::*;
use super::cli_from;
use crate::cli::BuildError;
use crate::config::ConfigOverrides;
use norn::agent_loop::config::ConversationStateMode;
use norn::config::NornSettings;

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
        "auto_compact_reserve_tokens=25000".to_owned(),
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
    assert_eq!(config.auto_compact_reserve_tokens, Some(25_000));
    assert_eq!(config.auto_compact_keep_recent_turns, 4);
}

#[test]
fn settings_to_agent_config_fills_every_field() {
    use norn::config::{AgentSettings, AutoCompactReserve, NornSettings};
    let mut config = default_agent_loop_config();
    let settings = NornSettings {
        agent: Some(AgentSettings {
            max_turns: Some(11),
            step_timeout: Some("45s".to_owned()),
            schema_budget: Some(7),
            context_window: Some(200_000),
            auto_compact_reserve_tokens: Some(AutoCompactReserve::Tokens(35_000)),
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
    assert_eq!(config.auto_compact_reserve_tokens, Some(35_000));
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
fn index_lock_deadline_defaults_when_unset() {
    let deadline =
        resolve_index_lock_deadline(&NornSettings::default(), &ConfigOverrides::default()).unwrap();
    assert_eq!(
        deadline,
        Duration::from_millis(DEFAULT_INDEX_LOCK_DEADLINE_MS),
    );
}

#[test]
fn index_lock_deadline_settings_beat_compiled_default() {
    use norn::config::{AgentSettings, NornSettings};
    let settings = NornSettings {
        agent: Some(AgentSettings {
            index_lock_deadline_ms: Some(5_000),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    let deadline = resolve_index_lock_deadline(&settings, &ConfigOverrides::default()).unwrap();
    assert_eq!(deadline, Duration::from_secs(5));
}

#[test]
fn index_lock_deadline_c_override_beats_settings() {
    use norn::config::{AgentSettings, NornSettings};
    let settings = NornSettings {
        agent: Some(AgentSettings {
            index_lock_deadline_ms: Some(5_000),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    let overrides = ConfigOverrides {
        index_lock_deadline_ms: Some(250),
        ..ConfigOverrides::default()
    };
    let deadline = resolve_index_lock_deadline(&settings, &overrides).unwrap();
    assert_eq!(deadline, Duration::from_millis(250));
}

#[test]
fn index_lock_deadline_zero_from_settings_is_typed_error() {
    use norn::config::{AgentSettings, NornSettings};
    let settings = NornSettings {
        agent: Some(AgentSettings {
            index_lock_deadline_ms: Some(0),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    let err = resolve_index_lock_deadline(&settings, &ConfigOverrides::default())
        .expect_err("a zero deadline must be rejected");
    match err {
        BuildError::Argument(reason) => {
            assert!(
                reason.contains("agent.index_lock_deadline_ms"),
                "reason names the settings key: {reason}",
            );
            assert!(
                reason.contains("a zero deadline can never acquire the lock"),
                "reason explains the rejection: {reason}",
            );
        }
        other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
    }
}

#[test]
fn index_lock_deadline_zero_from_c_override_is_typed_error() {
    // `ConfigOverrides::parse` already rejects zero; this covers a
    // programmatically constructed override sneaking past the parser.
    let overrides = ConfigOverrides {
        index_lock_deadline_ms: Some(0),
        ..ConfigOverrides::default()
    };
    let err = resolve_index_lock_deadline(&NornSettings::default(), &overrides)
        .expect_err("a zero deadline must be rejected");
    match err {
        BuildError::Argument(reason) => {
            assert!(
                reason.contains("-c index_lock_deadline_ms"),
                "reason names the -c key: {reason}",
            );
            assert!(
                reason.contains("a zero deadline can never acquire the lock"),
                "reason explains the rejection: {reason}",
            );
        }
        other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
    }
}
