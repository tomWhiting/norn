use norn::config::ProviderAuthMode;

use super::*;

struct MatrixCase {
    name: &'static str,
    kind: ProviderKind,
    auth: Option<ProviderAuthMode>,
    api_key_env: Option<&'static str>,
    expected: Result<ResolvedProviderAuth, ProviderAuthConfigError>,
}

fn resolve(case: &MatrixCase) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
    resolve_provider_auth(
        case.kind,
        &ProviderConfigOverrides {
            auth: case.auth,
            api_key_env: case.api_key_env.map(str::to_owned),
            ..ProviderConfigOverrides::default()
        },
    )
}

#[test]
fn provider_auth_matrix_is_exhaustive() {
    let cases = [
        MatrixCase {
            name: "openai omitted without env",
            kind: ProviderKind::Openai,
            auth: None,
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::OAuth),
        },
        MatrixCase {
            name: "openai omitted with env",
            kind: ProviderKind::Openai,
            auth: None,
            api_key_env: Some("OPENAI_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("OPENAI_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "openai oauth without env",
            kind: ProviderKind::Openai,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::OAuth),
        },
        MatrixCase {
            name: "openai oauth with env",
            kind: ProviderKind::Openai,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::OpenAiOAuthWithApiKeyEnv),
        },
        MatrixCase {
            name: "openai api key without env",
            kind: ProviderKind::Openai,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::OpenAiApiKeyWithoutEnv),
        },
        MatrixCase {
            name: "openai api key with env",
            kind: ProviderKind::Openai,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: Some("OPENAI_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("OPENAI_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "compatible omitted without env",
            kind: ProviderKind::OpenaiCompatible,
            auth: None,
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv(
                DEFAULT_OPENAI_COMPAT_API_KEY_ENV.to_owned(),
            )),
        },
        MatrixCase {
            name: "compatible omitted with env",
            kind: ProviderKind::OpenaiCompatible,
            auth: None,
            api_key_env: Some("LOCAL_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("LOCAL_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "compatible oauth without env",
            kind: ProviderKind::OpenaiCompatible,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
        },
        MatrixCase {
            name: "compatible oauth with env",
            kind: ProviderKind::OpenaiCompatible,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::OpenAiCompatibleOAuth),
        },
        MatrixCase {
            name: "compatible api key without env",
            kind: ProviderKind::OpenaiCompatible,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::OpenAiCompatibleApiKeyWithoutEnv),
        },
        MatrixCase {
            name: "compatible api key with env",
            kind: ProviderKind::OpenaiCompatible,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: Some("LOCAL_API_KEY"),
            expected: Ok(ResolvedProviderAuth::ApiKeyEnv("LOCAL_API_KEY".to_owned())),
        },
        MatrixCase {
            name: "claude omitted without env",
            kind: ProviderKind::ClaudeRunner,
            auth: None,
            api_key_env: None,
            expected: Ok(ResolvedProviderAuth::None),
        },
        MatrixCase {
            name: "claude omitted with env",
            kind: ProviderKind::ClaudeRunner,
            auth: None,
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::ClaudeRunnerApiKeyEnv),
        },
        MatrixCase {
            name: "claude oauth without env",
            kind: ProviderKind::ClaudeRunner,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
        MatrixCase {
            name: "claude oauth with env",
            kind: ProviderKind::ClaudeRunner,
            auth: Some(ProviderAuthMode::OAuth),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
        MatrixCase {
            name: "claude api key without env",
            kind: ProviderKind::ClaudeRunner,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: None,
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
        MatrixCase {
            name: "claude api key with env",
            kind: ProviderKind::ClaudeRunner,
            auth: Some(ProviderAuthMode::ApiKey),
            api_key_env: Some("SHOULD_NOT_BE_READ"),
            expected: Err(ProviderAuthConfigError::ClaudeRunnerAuth),
        },
    ];

    for case in &cases {
        assert_eq!(
            resolve(case),
            case.expected.clone(),
            "matrix case: {}",
            case.name,
        );
    }
}

#[test]
fn blank_api_key_environment_names_are_rejected_before_lookup() {
    for (kind, auth) in [
        (ProviderKind::Openai, None),
        (ProviderKind::Openai, Some(ProviderAuthMode::ApiKey)),
        (ProviderKind::OpenaiCompatible, None),
        (
            ProviderKind::OpenaiCompatible,
            Some(ProviderAuthMode::ApiKey),
        ),
    ] {
        let result = resolve_provider_auth(
            kind,
            &ProviderConfigOverrides {
                auth,
                api_key_env: Some("   ".to_owned()),
                ..ProviderConfigOverrides::default()
            },
        );
        assert_eq!(result, Err(ProviderAuthConfigError::EmptyApiKeyEnv));
    }
}

#[test]
fn provider_auth_wire_spellings_are_exact() -> Result<(), Box<dyn std::error::Error>> {
    for (encoded, expected) in [
        ("\"oauth\"", ProviderAuthMode::OAuth),
        ("\"api_key\"", ProviderAuthMode::ApiKey),
    ] {
        let decoded: ProviderAuthMode = serde_json::from_str(encoded)?;
        assert_eq!(decoded, expected);
        assert_eq!(serde_json::to_string(&decoded)?, encoded);
    }

    for rejected in ["env", "", "OAuth", "api-key", "unknown"] {
        let encoded = serde_json::to_string(rejected)?;
        let error = serde_json::from_str::<ProviderAuthMode>(&encoded);
        let Err(error) = error else {
            return Err(std::io::Error::other("invalid auth mode was accepted").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("expected exactly oauth or api_key"));
        if !rejected.is_empty() {
            assert!(!rendered.contains(rejected));
        }
    }
    Ok(())
}

#[test]
fn cli_auth_override_uses_the_same_exact_spellings() -> Result<(), Box<dyn std::error::Error>> {
    for (spelling, expected) in [
        ("oauth", ProviderAuthMode::OAuth),
        ("api_key", ProviderAuthMode::ApiKey),
    ] {
        let parsed = crate::config::ConfigOverrides::parse(&[format!("auth={spelling}")])?;
        assert_eq!(parsed.auth, Some(expected));
        assert_eq!(parsed.provider_overrides().auth, Some(expected));
    }
    for rejected in ["env", "", "OAuth", "api-key", "unknown"] {
        let result = crate::config::ConfigOverrides::parse(&[format!("auth={rejected}")]);
        let Err(error) = result else {
            return Err(std::io::Error::other("invalid CLI auth mode was accepted").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("expected exactly oauth or api_key"));
        if !rejected.is_empty() {
            assert!(!rendered.contains(rejected));
        }
    }
    Ok(())
}

#[test]
fn hostile_cli_auth_value_is_not_rendered() -> Result<(), Box<dyn std::error::Error>> {
    let sentinel = "AUTH_VALUE_MUST_NOT_APPEAR\u{1b}[31m";
    let result = crate::config::ConfigOverrides::parse(&[format!("auth={sentinel}")]);
    let Err(error) = result else {
        return Err(std::io::Error::other("hostile CLI auth mode was accepted").into());
    };
    let rendered = error.to_string();
    assert!(!rendered.contains("AUTH_VALUE_MUST_NOT_APPEAR"));
    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains("[31m"));
    Ok(())
}

#[test]
fn invalid_matrix_errors_do_not_disclose_companion_names() -> Result<(), Box<dyn std::error::Error>>
{
    let sentinel = "AUTH_ENV_NAME_MUST_NOT_APPEAR";
    for (kind, auth) in [
        (ProviderKind::Openai, ProviderAuthMode::OAuth),
        (ProviderKind::OpenaiCompatible, ProviderAuthMode::OAuth),
        (ProviderKind::ClaudeRunner, ProviderAuthMode::ApiKey),
    ] {
        let error = resolve_provider_auth(
            kind,
            &ProviderConfigOverrides {
                auth: Some(auth),
                api_key_env: Some(sentinel.to_owned()),
                ..ProviderConfigOverrides::default()
            },
        );
        let Err(error) = error else {
            return Err(std::io::Error::other(
                format!("invalid matrix was accepted for {kind:?}",),
            )
            .into());
        };
        assert!(!error.to_string().contains(sentinel));
    }
    Ok(())
}
