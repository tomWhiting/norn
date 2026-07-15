use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn bearer_header(request: &reqwest::Request) -> TestResultString<'_> {
    let value = request
        .headers()
        .get("Authorization")
        .ok_or_else(|| std::io::Error::other("Authorization header is missing"))?;
    Ok(value.to_str()?)
}

type TestResultString<'a> = Result<&'a str, Box<dyn std::error::Error>>;

#[test]
fn auth_source_default_is_oauth_with_no_auth_root_override() {
    assert!(matches!(
        AuthSource::default(),
        AuthSource::OAuth { auth_root: None }
    ));
}

#[test]
fn auth_source_oauth_default_constructor() {
    assert!(matches!(
        AuthSource::oauth_default(),
        AuthSource::OAuth { auth_root: None }
    ));
}

#[test]
fn login_config_default_is_browser_pkce() {
    let config = LoginConfig::default();
    assert!(config.auth_root.is_none());
    assert!(!config.device_code);
}

#[test]
fn auth_provider_is_object_safe() {
    let _provider: Arc<dyn AuthProvider> =
        Arc::new(ApiKeyAuthProvider::new(SecretString::new("k")));
}

#[tokio::test]
async fn api_key_auth_provider_on_unauthorized_returns_false() -> TestResult {
    let provider = ApiKeyAuthProvider::new(SecretString::new("test-key"));
    assert!(!provider.on_unauthorized().await?);
    Ok(())
}

#[tokio::test]
async fn api_key_auth_provider_sets_bearer_header() -> TestResult {
    let provider = ApiKeyAuthProvider::new(SecretString::new("test-key"));
    let client = reqwest::Client::new();
    let built = provider
        .apply_auth(client.get("http://example.invalid"))
        .await?;
    let request = built.build()?;

    assert_eq!(bearer_header(&request)?, "Bearer test-key");
    Ok(())
}

#[tokio::test]
async fn build_from_auth_source_api_key_returns_api_key_provider() -> TestResult {
    let source = AuthSource::ApiKey {
        key: SecretString::new("k"),
    };
    let provider = build_from_auth_source(&source).await?;
    assert!(!provider.on_unauthorized().await?);
    Ok(())
}

#[tokio::test]
async fn mock_auth_provider_applies_token_sequence() -> TestResult {
    let provider =
        MockAuthProvider::with_token_sequence(vec!["stale".to_owned(), "fresh".to_owned()]);
    let client = reqwest::Client::new();

    let first = provider
        .apply_auth(client.get("http://example.invalid"))
        .await?
        .build()?;
    assert_eq!(bearer_header(&first)?, "Bearer stale");

    let second = provider
        .apply_auth(client.get("http://example.invalid"))
        .await?
        .build()?;
    assert_eq!(bearer_header(&second)?, "Bearer fresh");
    assert_eq!(provider.apply_call_count(), 2);
    Ok(())
}

#[tokio::test]
async fn mock_auth_provider_consumes_on_unauthorized_sequence() -> TestResult {
    let provider =
        MockAuthProvider::single("t").with_unauthorized_responses(vec![Ok(true), Ok(false)]);
    assert!(provider.on_unauthorized().await?);
    assert!(!provider.on_unauthorized().await?);
    assert_eq!(provider.refresh_call_count(), 2);
    Ok(())
}

#[tokio::test]
async fn login_with_device_code_returns_config_error() -> TestResult {
    let result = login(LoginConfig {
        auth_root: Some(std::path::PathBuf::from("/tmp/norn-auth-test-nx")),
        device_code: true,
    })
    .await;
    let Err(NornError::Config(ConfigError::InvalidConfig { reason })) = result else {
        return Err(std::io::Error::other("device login did not return a config error").into());
    };
    assert!(reason.contains("device code"));
    Ok(())
}

#[tokio::test]
async fn login_rejects_relative_auth_root_before_starting_browser_flow() -> TestResult {
    let result = login(LoginConfig {
        auth_root: Some(PathBuf::from("relative-auth-root")),
        device_code: false,
    })
    .await;
    let Err(NornError::Config(ConfigError::InvalidConfig { reason })) = result else {
        return Err(std::io::Error::other(
            "relative login root did not fail at the typed auth boundary",
        )
        .into());
    };
    assert!(reason.contains("must be absolute"));
    Ok(())
}

#[test]
fn login_transport_failures_are_retryable_at_provider_boundary() -> TestResult {
    let failures = [
        LoginError::Bind,
        LoginError::Server("callback transport closed".to_owned()),
        LoginError::Canceled,
    ];

    for failure in failures {
        let NornError::Provider(ProviderError::ConnectionFailed { kind, .. }) =
            map_login_error(failure)
        else {
            return Err(std::io::Error::other(
                "login transport failure lost its retryable provider type",
            )
            .into());
        };
        assert_eq!(kind, TransientKind::ConnectionReset);
    }
    Ok(())
}

#[test]
fn login_browser_failure_is_a_configuration_error() -> TestResult {
    let mapped = map_login_error(LoginError::Browser("browser launcher unavailable"));
    let NornError::Config(ConfigError::InvalidConfig { reason }) = mapped else {
        return Err(std::io::Error::other(
            "structural browser failure was not a configuration error",
        )
        .into());
    };
    assert!(reason.contains("browser launcher unavailable"));
    Ok(())
}

#[test]
fn login_authority_failures_are_authentication_errors() -> TestResult {
    let failures = [
        LoginError::MissingCode,
        LoginError::AuthorizationFailed,
        LoginError::TokenExchange("authority rejected the code".to_owned()),
    ];

    for failure in failures {
        let NornError::Provider(ProviderError::AuthenticationFailed { .. }) =
            map_login_error(failure)
        else {
            return Err(std::io::Error::other(
                "login authority failure lost its authentication type",
            )
            .into());
        };
    }
    Ok(())
}

#[test]
fn login_storage_failures_preserve_lifecycle_classification() -> TestResult {
    let credential_failures = [
        (
            LoginStorageFailureKind::Conflict,
            OAuthCredentialFailureKind::Conflict,
        ),
        (
            LoginStorageFailureKind::Undurable,
            OAuthCredentialFailureKind::Undurable,
        ),
    ];

    for (source_kind, expected_kind) in credential_failures {
        let mapped = map_login_error(LoginError::Storage {
            kind: source_kind,
            reason: "structural storage marker".to_owned(),
        });
        let NornError::Provider(ProviderError::OAuthCredentialFailure { kind, reason }) = mapped
        else {
            return Err(std::io::Error::other(
                "credential lifecycle failure was flattened at the login boundary",
            )
            .into());
        };
        assert_eq!(kind, expected_kind);
        assert_eq!(reason, "structural storage marker");
    }

    let mapped = map_login_error(LoginError::Storage {
        kind: LoginStorageFailureKind::Coordination,
        reason: "coordination marker".to_owned(),
    });
    let NornError::Provider(ProviderError::ConnectionFailed { kind, reason }) = mapped else {
        return Err(std::io::Error::other(
            "storage coordination failure lost its retryable provider type",
        )
        .into());
    };
    assert_eq!(kind, TransientKind::ConnectionReset);
    assert_eq!(reason, "coordination marker");
    Ok(())
}

#[tokio::test]
async fn oauth_provider_sets_bearer_and_account_id_headers() -> TestResult {
    let auth = super::super::openai_oauth::CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let manager = super::super::openai_oauth::AuthManager::from_static_auth(
        auth,
        OAuthHttpOptions::default(),
    )?;
    let provider = OAuthAuthProvider::from_manager(manager);
    let client = reqwest::Client::new();
    let request = provider
        .apply_auth(client.get("http://example.invalid"))
        .await?
        .build()?;

    assert_eq!(bearer_header(&request)?, "Bearer Access Token");
    let account = request
        .headers()
        .get("chatgpt-account-id")
        .ok_or_else(|| std::io::Error::other("chatgpt-account-id header is missing"))?;
    assert_eq!(account.to_str()?, "account_id");
    Ok(())
}

#[tokio::test]
async fn oauth_provider_build_retains_the_typed_credential_reason() -> TestResult {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join(super::super::openai_oauth::AUTH_JSON_FILE),
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "unsupported-mode",
            "tokens": {}
        }))?,
    )?;

    let result = OAuthAuthProvider::new(Some(home.path().to_path_buf())).await;
    let Err(ProviderError::AuthenticationFailed { reason }) = result else {
        return Err(std::io::Error::other(
            "unsupported stored auth mode did not reach the provider boundary",
        )
        .into());
    };
    assert!(reason.contains("unsupported authentication mode"));
    assert!(!reason.contains("malformed JSON"));
    Ok(())
}

#[test]
fn transient_refresh_failure_stays_retryable_at_provider_boundary() -> TestResult {
    let error = map_refresh_token_error(RefreshTokenError::Transient(
        "authority returned HTTP 500".to_owned(),
    ));
    let ProviderError::ConnectionFailed { reason, kind } = &error else {
        return Err(std::io::Error::other("transient refresh lost its retryable type").into());
    };

    assert_eq!(*kind, crate::error::TransientKind::ConnectionReset);
    assert!(reason.contains("transiently"));
    assert!(!reason.contains("no refresh available"));
    assert!(crate::r#loop::retry::RetryPolicy::default().classifies_as_retryable(&error));
    Ok(())
}

#[test]
fn credential_lifecycle_failures_preserve_their_provider_kind() -> TestResult {
    let cases = [
        (
            RefreshTokenError::Permanent("owner sink unavailable".to_owned()),
            OAuthCredentialFailureKind::Permanent,
        ),
        (
            RefreshTokenError::Undurable("directory sync failed".to_owned()),
            OAuthCredentialFailureKind::Undurable,
        ),
        (
            RefreshTokenError::Conflict("foreign writer won".to_owned()),
            OAuthCredentialFailureKind::Conflict,
        ),
        (
            RefreshTokenError::Indeterminate("lineage response malformed".to_owned()),
            OAuthCredentialFailureKind::Indeterminate,
        ),
    ];

    for (source, expected) in cases {
        let error = map_refresh_token_error(source);
        let ProviderError::OAuthCredentialFailure { kind, .. } = error else {
            return Err(std::io::Error::other("OAuth lifecycle failure was flattened").into());
        };
        assert_eq!(kind, expected);
    }
    Ok(())
}

#[tokio::test]
async fn ownerless_static_refresh_is_a_permanent_typed_failure() -> TestResult {
    let auth = super::super::openai_oauth::CodexAuth::from_api_key("vm-injected-key");
    let manager = super::super::openai_oauth::AuthManager::from_static_auth(
        auth,
        OAuthHttpOptions::default(),
    )?;
    let provider = OAuthAuthProvider::from_manager(manager);
    let result = provider.on_unauthorized().await;
    let Err(ProviderError::OAuthCredentialFailure {
        kind: OAuthCredentialFailureKind::Permanent,
        reason,
    }) = result
    else {
        return Err(std::io::Error::other(
            "ownerless static refresh was not a permanent credential failure",
        )
        .into());
    };

    assert!(reason.contains("file-backed credential owner"));
    Ok(())
}

#[tokio::test]
async fn oauth_provider_omits_account_id_when_absent() -> TestResult {
    let auth = super::super::openai_oauth::CodexAuth::from_api_key("test-api-key-value");
    let manager = super::super::openai_oauth::AuthManager::from_static_auth(
        auth,
        OAuthHttpOptions::default(),
    )?;
    let provider = OAuthAuthProvider::from_manager(manager);
    let client = reqwest::Client::new();
    let request = provider
        .apply_auth(client.get("http://example.invalid"))
        .await?
        .build()?;

    assert_eq!(bearer_header(&request)?, "Bearer test-api-key-value");
    assert!(request.headers().get("chatgpt-account-id").is_none());
    Ok(())
}
