use super::*;

struct MatrixCase {
    name: &'static str,
    backend: ProviderAuthBackend,
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&'static str>,
    expected: Result<ResolvedProviderAuth, ProviderAuthConfigError>,
}

#[test]
fn provider_auth_matrix_is_exhaustive() {
    let cases = [
        MatrixCase {
            name: "openai omitted without env",
            backend: ProviderAuthBackend::OpenAi,
            auth: None,
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::OAuth),
        },
        MatrixCase {
            name: "openai omitted with env",
            backend: ProviderAuthBackend::OpenAi,
            auth: None,
            api_key_env: Some("OPENAI_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("OPENAI_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "openai oauth without env",
            backend: ProviderAuthBackend::OpenAi,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::OAuth),
        },
        MatrixCase {
            name: "openai oauth with env",
            backend: ProviderAuthBackend::OpenAi,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::OpenAiOAuthWithApiKeyEnv),
        },
        MatrixCase {
            name: "openai api key without env",
            backend: ProviderAuthBackend::OpenAi,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::OpenAiApiKeyWithoutEnv),
        },
        MatrixCase {
            name: "openai api key with env",
            backend: ProviderAuthBackend::OpenAi,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: Some("OPENAI_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("OPENAI_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "compatible omitted without env",
            backend: ProviderAuthBackend::OpenAiCompatible,
            auth: None,
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv(
                DEFAULT_OPENAI_COMPAT_API_KEY_ENV.to_owned(),
            )),
        },
        MatrixCase {
            name: "compatible omitted with env",
            backend: ProviderAuthBackend::OpenAiCompatible,
            auth: None,
            api_key_env: Some("LOCAL_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("LOCAL_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "compatible oauth without env",
            backend: ProviderAuthBackend::OpenAiCompatible,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
        },
        MatrixCase {
            name: "compatible oauth with env",
            backend: ProviderAuthBackend::OpenAiCompatible,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
        },
        MatrixCase {
            name: "compatible api key without env",
            backend: ProviderAuthBackend::OpenAiCompatible,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::OpenAiCompatibleApiKeyWithoutEnv),
        },
        MatrixCase {
            name: "compatible api key with env",
            backend: ProviderAuthBackend::OpenAiCompatible,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: Some("LOCAL_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("LOCAL_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "claude omitted without env",
            backend: ProviderAuthBackend::ClaudeRunner,
            auth: None,
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::None),
        },
        MatrixCase {
            name: "claude omitted with env",
            backend: ProviderAuthBackend::ClaudeRunner,
            auth: None,
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::ClaudeRunnerApiKeyEnv),
        },
        MatrixCase {
            name: "claude oauth without env",
            backend: ProviderAuthBackend::ClaudeRunner,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
        MatrixCase {
            name: "claude oauth with env",
            backend: ProviderAuthBackend::ClaudeRunner,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
        MatrixCase {
            name: "claude api key without env",
            backend: ProviderAuthBackend::ClaudeRunner,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
        MatrixCase {
            name: "claude api key with env",
            backend: ProviderAuthBackend::ClaudeRunner,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
    ];

    for case in &cases {
        assert_eq!(
            resolve_provider_auth(case.backend, case.auth, case.api_key_env),
            case.expected.clone(),
            "matrix case: {}",
            case.name,
        );
    }
}

#[test]
fn blank_api_key_environment_names_are_rejected() {
    for (backend, auth) in [
        (ProviderAuthBackend::OpenAi, None),
        (ProviderAuthBackend::OpenAi, Some(ProviderAuthMode::ApiKey)),
        (ProviderAuthBackend::OpenAiCompatible, None),
        (
            ProviderAuthBackend::OpenAiCompatible,
            Some(ProviderAuthMode::ApiKey),
        ),
    ] {
        assert_eq!(
            resolve_provider_auth(backend, auth, Some("   ")),
            Err(ProviderAuthConfigError::EmptyApiKeyEnv),
        );
    }
}

#[test]
fn invalid_matrix_errors_do_not_disclose_companion_names() -> Result<(), std::io::Error> {
    let sentinel = "AUTH_ENV_NAME_MUST_NOT_APPEAR";
    for (backend, auth) in [
        (ProviderAuthBackend::OpenAi, ProviderAuthMode::OAuth),
        (
            ProviderAuthBackend::OpenAiCompatible,
            ProviderAuthMode::OAuth,
        ),
        (ProviderAuthBackend::ClaudeRunner, ProviderAuthMode::ApiKey),
    ] {
        let result = resolve_provider_auth(backend, Some(auth), Some(sentinel));
        let Err(error) = result else {
            return Err(std::io::Error::other("invalid auth matrix was accepted"));
        };
        assert!(!error.to_string().contains(sentinel));
    }
    Ok(())
}
