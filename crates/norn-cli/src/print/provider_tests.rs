use super::*;

#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn auth_error_maps_to_exit_code_three() {
    let err = ProviderBuildError::Auth("expired".to_owned());
    assert_eq!(err.exit_code(), ExitCode::AuthError);
}

#[test]
fn provider_error_maps_to_exit_code_one() {
    let err = ProviderBuildError::Provider("connection refused".to_owned());
    assert_eq!(err.exit_code(), ExitCode::AgentError);
}

#[test]
fn authentication_failed_provider_error_converts_to_auth_variant() {
    let err: ProviderBuildError = ProviderError::AuthenticationFailed {
        reason: "token expired".to_owned(),
    }
    .into();
    assert!(matches!(err, ProviderBuildError::Auth(_)));
    assert_eq!(err.exit_code(), ExitCode::AuthError);
}

#[test]
fn oauth_credential_failure_converts_to_auth_variant() {
    let err: ProviderBuildError = ProviderError::OAuthCredentialFailure {
        kind: norn::error::OAuthCredentialFailureKind::Conflict,
        reason: "credential changed".to_owned(),
    }
    .into();
    assert!(matches!(err, ProviderBuildError::Auth(_)));
    assert_eq!(err.exit_code(), ExitCode::AuthError);
}

#[test]
fn connection_failed_provider_error_converts_to_provider_variant() {
    let err: ProviderBuildError = ProviderError::ConnectionFailed {
        reason: "refused".to_owned(),
        kind: norn::error::TransientKind::ConnectionReset,
    }
    .into();
    assert!(matches!(err, ProviderBuildError::Provider(_)));
    assert_eq!(err.exit_code(), ExitCode::AgentError);
}

#[cfg(unix)]
#[test]
#[serial]
fn non_unicode_api_key_value_is_not_rendered() -> TestResult {
    const KEY: &str = "NORN_TEST_NON_UNICODE_API_KEY";
    let non_unicode = OsString::from_vec(b"SECRET_PAYLOAD_\xff_MUST_NOT_APPEAR".to_vec());
    let result = temp_env::with_var(KEY, Some(non_unicode), || {
        read_required_api_key(KEY, "test provider")
    });
    let Err(ProviderBuildError::Auth(rendered)) = result else {
        return Err(std::io::Error::other("non-Unicode API key was accepted").into());
    };

    assert!(rendered.contains("value is not valid Unicode"));
    assert!(!rendered.contains("SECRET_PAYLOAD"));
    assert!(!rendered.contains("MUST_NOT_APPEAR"));
    assert!(!rendered.contains('\u{fffd}'));
    Ok(())
}

#[test]
fn overrides_flow_through_to_provider_config_fields() {
    let overrides = ProviderConfigOverrides {
        base_url: Some("http://localhost:8080".to_owned()),
        request_timeout: Some(Duration::from_secs(30)),
        max_retries: Some(5),
        provider_options: Some(serde_json::json!({"key": "val"})),
        api_key_env: Some("LOCAL_AI_KEY".to_owned()),
        auth: None,
        debug_dump_dir: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: Some(Duration::from_secs(30)),
        retry_backoff: Some(Duration::from_millis(250)),
        retry_after_ceiling: Some(Duration::from_secs(90)),
        runner_path: None,
    };
    let config = ProviderConfig {
        auth_source: AuthSource::ApiKey {
            key: SecretString::new("local-test-key"),
        },
        timeout: overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
        max_retries: overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
        base_url: overrides.base_url,
        provider_options: overrides.provider_options,
        debug_dump_file: None,
        rate_limit: overrides.rate_limit,
        rate_limit_interval: overrides.rate_limit_interval,
        retry_backoff: overrides.retry_backoff,
        retry_after_ceiling: overrides.retry_after_ceiling,
    };
    assert_eq!(config.base_url, Some("http://localhost:8080".to_owned()));
    assert_eq!(config.timeout, Duration::from_secs(30));
    assert_eq!(config.max_retries, 5);
    assert!(config.provider_options.is_some());
    assert_eq!(config.rate_limit_interval, Some(Duration::from_secs(30)));
    assert_eq!(config.retry_backoff, Some(Duration::from_millis(250)));
    assert_eq!(config.retry_after_ceiling, Some(Duration::from_secs(90)));
}

#[test]
fn default_overrides_use_brief_mandated_defaults() {
    let overrides = ProviderConfigOverrides::default();
    let timeout = overrides.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);
    let retries = overrides.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    assert_eq!(timeout, Duration::from_mins(2));
    assert_eq!(retries, 2);
}

#[tokio::test]
async fn openai_compatible_requires_base_url() -> TestResult {
    let overrides = ProviderConfigOverrides {
        api_key_env: Some("NORN_TEST_COMPAT_KEY_BASE_URL".to_owned()),
        ..ProviderConfigOverrides::default()
    };
    let result = temp_env::async_with_vars(
        [("NORN_TEST_COMPAT_KEY_BASE_URL", Some("test-key"))],
        build_provider(
            ProviderKind::OpenaiCompatible,
            &overrides,
            "local-model",
            None,
            false,
        ),
    )
    .await;
    let Err(ProviderBuildError::Provider(reason)) = result else {
        return Err(std::io::Error::other("missing base URL was not a provider error").into());
    };
    assert!(reason.contains("base_url"));
    Ok(())
}

#[tokio::test]
async fn openai_compatible_requires_api_key_env() -> TestResult {
    let overrides = ProviderConfigOverrides {
        base_url: Some("http://localhost:11434/v1".to_owned()),
        api_key_env: Some("NORN_TEST_COMPAT_KEY_MISSING".to_owned()),
        ..ProviderConfigOverrides::default()
    };
    let result = temp_env::async_with_vars(
        [("NORN_TEST_COMPAT_KEY_MISSING", None::<&str>)],
        build_provider(
            ProviderKind::OpenaiCompatible,
            &overrides,
            "local-model",
            None,
            false,
        ),
    )
    .await;
    let Err(ProviderBuildError::Auth(reason)) = result else {
        return Err(std::io::Error::other("missing API key was not an auth error").into());
    };
    assert!(reason.contains("NORN_TEST_COMPAT_KEY_MISSING"));
    Ok(())
}

#[tokio::test]
async fn openai_compatible_builds_with_api_key_env() -> TestResult {
    let overrides = ProviderConfigOverrides {
        base_url: Some("http://localhost:11434/v1".to_owned()),
        api_key_env: Some("NORN_TEST_COMPAT_KEY_PRESENT".to_owned()),
        ..ProviderConfigOverrides::default()
    };
    let built = temp_env::async_with_vars(
        [("NORN_TEST_COMPAT_KEY_PRESENT", Some("test-key"))],
        build_provider(
            ProviderKind::OpenaiCompatible,
            &overrides,
            "local-model",
            None,
            false,
        ),
    )
    .await?;
    let BuiltProvider::OpenAiCompatible(_) = built else {
        return Err(std::io::Error::other("expected OpenAiCompatible provider").into());
    };
    Ok(())
}

#[tokio::test]
async fn openai_responses_builds_with_api_key_env_when_selected() -> TestResult {
    let overrides = ProviderConfigOverrides {
        api_key_env: Some("NORN_TEST_OPENAI_KEY_PRESENT".to_owned()),
        ..ProviderConfigOverrides::default()
    };
    let built = temp_env::async_with_vars(
        [("NORN_TEST_OPENAI_KEY_PRESENT", Some("test-key"))],
        build_provider(ProviderKind::Openai, &overrides, "gpt-5.5", None, false),
    )
    .await?;
    let BuiltProvider::OpenAi(_) = built else {
        return Err(std::io::Error::other("expected OpenAI provider").into());
    };
    Ok(())
}

#[tokio::test]
async fn explicit_oauth_rejects_api_key_companion_before_environment_lookup() -> TestResult {
    let sentinel = "NORN_AUTH_MATRIX_ENV_MUST_NOT_BE_READ";
    let result = build_provider(
        ProviderKind::Openai,
        &ProviderConfigOverrides {
            auth: Some(norn::config::ProviderAuthMode::OAuth),
            api_key_env: Some(sentinel.to_owned()),
            ..ProviderConfigOverrides::default()
        },
        "gpt-5.5",
        None,
        false,
    )
    .await;
    let Err(ProviderBuildError::Provider(reason)) = result else {
        return Err(std::io::Error::other(
            "invalid OAuth companion was not rejected as configuration",
        )
        .into());
    };
    assert!(reason.contains("auth=oauth"));
    assert!(!reason.contains(sentinel));
    Ok(())
}

#[tokio::test]
async fn explicit_api_key_without_source_rejects_before_oauth_storage_lookup() -> TestResult {
    let result = build_provider(
        ProviderKind::Openai,
        &ProviderConfigOverrides {
            auth: Some(norn::config::ProviderAuthMode::ApiKey),
            ..ProviderConfigOverrides::default()
        },
        "gpt-5.5",
        None,
        true,
    )
    .await;
    let Err(ProviderBuildError::Provider(reason)) = result else {
        return Err(std::io::Error::other(
            "API-key mode without a source was not rejected as configuration",
        )
        .into());
    };
    assert!(reason.contains("auth=api_key"));
    assert!(reason.contains("api_key_env"));
    Ok(())
}

#[tokio::test]
async fn claude_runner_rejects_norn_auth_before_adapter_construction() -> TestResult {
    let result = build_provider(
        ProviderKind::ClaudeRunner,
        &ProviderConfigOverrides {
            auth: Some(norn::config::ProviderAuthMode::OAuth),
            ..ProviderConfigOverrides::default()
        },
        "sonnet",
        None,
        false,
    )
    .await;
    let Err(ProviderBuildError::Provider(reason)) = result else {
        return Err(
            std::io::Error::other("Claude Runner accepted a Norn-managed auth mode").into(),
        );
    };
    assert!(reason.contains("claude-runner"));
    assert!(reason.contains("provider.auth"));
    Ok(())
}

#[tokio::test]
async fn claude_runner_honors_settings_runner_path_override() -> TestResult {
    // Regression for the ignored `settings.provider.runner_path`:
    // the documented override must reach the constructed adapter.
    let overrides = ProviderConfigOverrides {
        runner_path: Some(PathBuf::from("/opt/tools/claude-custom")),
        ..ProviderConfigOverrides::default()
    };
    let built = build_provider(
        ProviderKind::ClaudeRunner,
        &overrides,
        "sonnet",
        None,
        false,
    )
    .await?;
    let BuiltProvider::ClaudeRunner(adapter) = built else {
        return Err(std::io::Error::other("expected Claude Runner provider").into());
    };
    assert_eq!(
        adapter.runner_path(),
        std::path::Path::new("/opt/tools/claude-custom"),
    );
    Ok(())
}

#[tokio::test]
async fn claude_runner_defaults_to_claude_when_runner_path_unset() -> TestResult {
    let overrides = ProviderConfigOverrides::default();
    let built = build_provider(
        ProviderKind::ClaudeRunner,
        &overrides,
        "sonnet",
        None,
        false,
    )
    .await?;
    let BuiltProvider::ClaudeRunner(adapter) = built else {
        return Err(std::io::Error::other("expected Claude Runner provider").into());
    };
    assert_eq!(
        adapter.runner_path(),
        std::path::Path::new(DEFAULT_RUNNER_PATH)
    );
    Ok(())
}

#[tokio::test]
async fn claude_runner_construction_is_synchronous_and_succeeds() -> TestResult {
    // ClaudeRunnerAdapter::new is infallible — verify build_provider
    // wraps it correctly and returns a usable &dyn Provider.
    let overrides = ProviderConfigOverrides::default();
    let built = build_provider(
        ProviderKind::ClaudeRunner,
        &overrides,
        "sonnet",
        None,
        false,
    )
    .await?;
    if !matches!(&built, BuiltProvider::ClaudeRunner(_)) {
        return Err(std::io::Error::other("expected Claude Runner provider").into());
    }
    // Borrowing as &dyn Provider must compile.
    let _: &dyn Provider = built.as_dyn();
    Ok(())
}

#[test]
fn resumed_oauth_requires_explicit_account_but_api_key_does_not() -> TestResult {
    let oauth = validate_account_request(&ResolvedProviderAuth::OAuth, None, true);
    assert!(matches!(oauth, Err(ProviderBuildError::Auth(_))));

    let api_key = validate_account_request(
        &ResolvedProviderAuth::ApiKeyEnv("KEY".to_owned()),
        None,
        true,
    )?;
    assert_eq!(api_key, None);
    Ok(())
}

#[test]
fn explicit_account_is_rejected_for_non_oauth_backends() {
    let result = validate_account_request(&ResolvedProviderAuth::None, Some("work"), false);
    assert!(matches!(result, Err(ProviderBuildError::Provider(_))));
}
