use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn oauth_principal_identity_includes_optional_user() {
    let first = CredentialIdentity::from_oauth_principal("account-a", Some("user-a"));
    let same = CredentialIdentity::from_oauth_principal("account-a", Some("user-a"));
    let other_user = CredentialIdentity::from_oauth_principal("account-a", Some("user-b"));
    let absent_user = CredentialIdentity::from_oauth_principal("account-a", None);
    let empty_user = CredentialIdentity::from_oauth_principal("account-a", Some(""));

    assert_eq!(first, same);
    assert_ne!(first, other_user);
    assert_ne!(first, absent_user);
    assert_ne!(absent_user, empty_user);
}

#[test]
fn credential_and_authority_changes_produce_distinct_identities() {
    let first = CredentialIdentity::from_api_key("first-key")
        .scoped_to_openai_backend("responses_api", "https://api.example/v1/responses");
    let same = CredentialIdentity::from_api_key("first-key")
        .scoped_to_openai_backend("responses_api", "https://api.example/v1/responses");
    let other_key = CredentialIdentity::from_api_key("second-key")
        .scoped_to_openai_backend("responses_api", "https://api.example/v1/responses");
    let other_endpoint = CredentialIdentity::from_api_key("first-key")
        .scoped_to_openai_backend("responses_api", "https://other.example/v1/responses");

    assert_eq!(first, same);
    assert_ne!(first, other_key);
    assert_ne!(first, other_endpoint);
}

#[test]
fn static_codex_identity_includes_account_and_access_token() {
    let first = CredentialIdentity::from_static_codex("token-a", Some("account-a"));
    let same = CredentialIdentity::from_static_codex("token-a", Some("account-a"));
    let other_token = CredentialIdentity::from_static_codex("token-b", Some("account-a"));
    let other_account = CredentialIdentity::from_static_codex("token-a", Some("account-b"));
    let token_only = CredentialIdentity::from_static_codex("token-a", None);

    assert_eq!(first, same);
    assert_ne!(first, other_token);
    assert_ne!(first, other_account);
    assert_ne!(first, token_only);
}

#[test]
fn provider_state_serde_is_fixed_width_and_debug_is_redacted() -> TestResult {
    let credential = CredentialIdentity::derive("auth-sentinel", b"credential-sentinel");
    let identity = ProviderStateIdentity::derive("embedder-sentinel", b"opaque-sentinel");
    let encoded = serde_json::to_value(identity)?;
    let values = encoded
        .as_array()
        .ok_or_else(|| std::io::Error::other("identity did not serialize as a fixed array"))?;
    assert_eq!(values.len(), 32);
    assert_eq!(
        serde_json::from_value::<ProviderStateIdentity>(encoded)?,
        identity
    );
    assert!(serde_json::from_str::<ProviderStateIdentity>("[0,1]").is_err());

    let rendered = format!("{identity:?}");
    assert_eq!(rendered, "ProviderStateIdentity([REDACTED])");
    assert!(!rendered.contains("embedder-sentinel"));
    assert!(!rendered.contains("opaque-sentinel"));
    let credential_rendered = format!("{credential:?}");
    assert_eq!(credential_rendered, "CredentialIdentity([REDACTED])");
    assert!(!credential_rendered.contains("auth-sentinel"));
    assert!(!credential_rendered.contains("credential-sentinel"));
    Ok(())
}
