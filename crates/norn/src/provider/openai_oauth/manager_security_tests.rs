use super::*;

#[test]
fn manager_debug_redacts_cached_credentials() -> Result<(), AuthManagerBuildError> {
    let manager = AuthManager::from_static_auth(
        CodexAuth::from_api_key("manager-api-key-secret"),
        OAuthHttpOptions::default(),
    )?;

    let rendered = format!("{manager:?}");
    assert!(!rendered.contains("manager-api-key-secret"));
    assert!(rendered.contains("[REDACTED]"));
    Ok(())
}

#[test]
fn malformed_credential_display_retains_the_typed_reason() {
    let error = AuthManagerBuildError::MalformedCredential {
        reason: MalformedCredentialReason::UnsupportedAuthMode,
    };
    let rendered = error.to_string();

    assert!(rendered.contains("unsupported authentication mode"));
    assert!(!rendered.contains("malformed JSON"));
}

#[tokio::test]
async fn malformed_credential_storage_fails_manager_construction()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    std::fs::write(home.path().join("auth.json"), b"{malformed")?;
    let auth_root = NornAuthRoot::try_from(home.path())?;

    let result = AuthManager::shared_for_tests(
        auth_root,
        AuthCredentialsStoreMode::File,
        "http://127.0.0.1:9".to_owned(),
    )
    .await;

    assert!(matches!(
        result,
        Err(AuthManagerBuildError::MalformedCredential {
            reason: MalformedCredentialReason::InvalidJson,
        })
    ));
    Ok(())
}
