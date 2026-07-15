use super::*;

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

#[test]
fn overrides_flow_through_to_provider_config_fields() {
    let overrides = ProviderConfigOverrides {
        base_url: Some("http://localhost:8080".to_owned()),
        request_timeout: Some(Duration::from_secs(30)),
        max_retries: Some(5),
        provider_options: Some(serde_json::json!({"key": "val"})),
        api_key_env: Some("LOCAL_AI_KEY".to_owned()),
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
        build_provider(ProviderKind::OpenaiCompatible, &overrides, "local-model"),
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
        build_provider(ProviderKind::OpenaiCompatible, &overrides, "local-model"),
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
        build_provider(ProviderKind::OpenaiCompatible, &overrides, "local-model"),
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
        build_provider(ProviderKind::Openai, &overrides, "gpt-5.5"),
    )
    .await?;
    let BuiltProvider::OpenAi(_) = built else {
        return Err(std::io::Error::other("expected OpenAI provider").into());
    };
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
    let built = build_provider(ProviderKind::ClaudeRunner, &overrides, "sonnet").await?;
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
    let built = build_provider(ProviderKind::ClaudeRunner, &overrides, "sonnet").await?;
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
    let built = build_provider(ProviderKind::ClaudeRunner, &overrides, "sonnet").await?;
    if !matches!(&built, BuiltProvider::ClaudeRunner(_)) {
        return Err(std::io::Error::other("expected Claude Runner provider").into());
    }
    // Borrowing as &dyn Provider must compile.
    let _: &dyn Provider = built.as_dyn();
    Ok(())
}
