use norn::config::{
    ProviderAuthBackend, ProviderAuthMode, resolve_provider_auth as resolve_library_auth,
};

use super::*;

#[test]
fn cli_adapter_matches_library_policy_for_every_input_shape() {
    let kinds = [
        (ProviderKind::Openai, ProviderAuthBackend::OpenAi),
        (
            ProviderKind::OpenaiCompatible,
            ProviderAuthBackend::OpenAiCompatible,
        ),
        (
            ProviderKind::ClaudeRunner,
            ProviderAuthBackend::ClaudeRunner,
        ),
    ];
    let modes = [
        None,
        Some(ProviderAuthMode::OAuth),
        Some(ProviderAuthMode::ApiKey),
    ];
    let environment_names = [None, Some("PROVIDER_API_KEY"), Some("   ")];

    for (kind, backend) in kinds {
        for auth in modes {
            for api_key_env in environment_names {
                let overrides = ProviderConfigOverrides {
                    auth,
                    api_key_env: api_key_env.map(str::to_owned),
                    ..ProviderConfigOverrides::default()
                };
                assert_eq!(
                    resolve_provider_auth(kind, &overrides),
                    resolve_library_auth(backend, auth, api_key_env),
                );
            }
        }
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
