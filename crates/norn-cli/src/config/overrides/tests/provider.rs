use std::path::PathBuf;
use std::time::Duration;

use super::super::*;
use crate::cli::BuildError;
use crate::config::{ConfigOverrides, ProviderConfigOverrides};

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
            auth: Some(norn::config::ProviderAuthMode::ApiKey),
            debug_dump_dir: Some("/tmp/dump".to_owned()),
            rate_limit: Some(120),
            rate_limit_interval: Some("90s".to_owned()),
            retry_backoff: Some("500ms".to_owned()),
            retry_after_ceiling: Some("2m".to_owned()),
            runner_path: Some("/usr/local/bin/claude".to_owned()),
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
    assert_eq!(overrides.auth, Some(norn::config::ProviderAuthMode::ApiKey));
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
fn provider_profile_oauth_clears_inherited_api_key_source() -> Result<(), BuildError> {
    use norn::config::{ProviderAuthMode, ProviderProfileSettings, ProviderSettings};

    let mut overrides = ProviderConfigOverrides {
        auth: Some(ProviderAuthMode::ApiKey),
        api_key_env: Some("SETTINGS_API_KEY".to_owned()),
        ..ProviderConfigOverrides::default()
    };
    let profile = ProviderProfileSettings {
        provider: ProviderSettings {
            auth: Some(ProviderAuthMode::OAuth),
            ..ProviderSettings::default()
        },
        ..ProviderProfileSettings::default()
    };

    overlay_provider_profile_overrides(&mut overrides, "oauth", &profile)?;

    assert_eq!(overrides.auth, Some(ProviderAuthMode::OAuth));
    assert_eq!(overrides.api_key_env, None);
    Ok(())
}

#[test]
fn provider_profile_auth_companion_obeys_same_layer_precedence() -> Result<(), BuildError> {
    use norn::config::{ProviderAuthMode, ProviderProfileSettings, ProviderSettings};

    for (mode, profile_env, expected_env) in [
        (ProviderAuthMode::ApiKey, None, "SETTINGS_API_KEY"),
        (
            ProviderAuthMode::OAuth,
            Some("PROFILE_API_KEY"),
            "PROFILE_API_KEY",
        ),
        (
            ProviderAuthMode::ApiKey,
            Some("PROFILE_API_KEY"),
            "PROFILE_API_KEY",
        ),
    ] {
        let mut overrides = ProviderConfigOverrides {
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SETTINGS_API_KEY".to_owned()),
            ..ProviderConfigOverrides::default()
        };
        let profile = ProviderProfileSettings {
            provider: ProviderSettings {
                auth: Some(mode),
                api_key_env: profile_env.map(str::to_owned),
                ..ProviderSettings::default()
            },
            ..ProviderProfileSettings::default()
        };

        overlay_provider_profile_overrides(&mut overrides, "selected", &profile)?;

        assert_eq!(overrides.auth, Some(mode));
        assert_eq!(overrides.api_key_env.as_deref(), Some(expected_env));
    }
    Ok(())
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
        auth: Some(norn::config::ProviderAuthMode::ApiKey),
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
fn cli_oauth_clears_inherited_api_key_source() {
    let mut base = ProviderConfigOverrides {
        auth: Some(norn::config::ProviderAuthMode::ApiKey),
        api_key_env: Some("SETTINGS_API_KEY".to_owned()),
        ..ProviderConfigOverrides::default()
    };
    let cli = ConfigOverrides {
        auth: Some(norn::config::ProviderAuthMode::OAuth),
        ..ConfigOverrides::default()
    };

    overlay_cli_provider_overrides(&mut base, &cli);

    assert_eq!(base.auth, Some(norn::config::ProviderAuthMode::OAuth));
    assert_eq!(base.api_key_env, None);
}

#[test]
fn cli_auth_companion_obeys_same_layer_precedence() {
    use norn::config::ProviderAuthMode;

    for (mode, cli_env, expected_env) in [
        (ProviderAuthMode::ApiKey, None, "SETTINGS_API_KEY"),
        (ProviderAuthMode::OAuth, Some("CLI_API_KEY"), "CLI_API_KEY"),
        (ProviderAuthMode::ApiKey, Some("CLI_API_KEY"), "CLI_API_KEY"),
    ] {
        let mut base = ProviderConfigOverrides {
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SETTINGS_API_KEY".to_owned()),
            ..ProviderConfigOverrides::default()
        };
        let cli = ConfigOverrides {
            auth: Some(mode),
            api_key_env: cli_env.map(str::to_owned),
            ..ConfigOverrides::default()
        };

        overlay_cli_provider_overrides(&mut base, &cli);

        assert_eq!(base.auth, Some(mode));
        assert_eq!(base.api_key_env.as_deref(), Some(expected_env));
    }
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
