use std::error::Error;

use base64::Engine as _;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::code_exchange::auth_from_response_fixture;
use super::credential_transaction::CredentialTransaction;
use super::{
    AuthCredentialsStoreMode, AuthDotJson, AuthManager, NornAuthRoot, OAuthHttpOptions,
    RefreshTokenError,
};
use crate::provider::auth::{AuthProvider as _, OAuthAuthProvider};

type TestResult = Result<(), Box<dyn Error>>;

const FUTURE_FIXTURE_EXPIRATION: i64 = 4_102_444_800;

fn jwt(claims: &serde_json::Value) -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}

fn id_token(account_id: &str, user_id: &str) -> String {
    jwt(&serde_json::json!({
        "email": "fixture@example.invalid",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": user_id,
            "chatgpt_plan_type": "fixture-plan"
        }
    }))
}

fn flat_id_token(account_id: &str, user_id: &str) -> String {
    jwt(&serde_json::json!({
        "email": "legacy-fixture@example.invalid",
        "chatgpt_account_id": account_id,
        "chatgpt_user_id": user_id,
        "chatgpt_plan_type": "legacy-fixture-plan"
    }))
}

fn access_token(subject: &str) -> String {
    jwt(&serde_json::json!({
        "sub": subject,
        "exp": FUTURE_FIXTURE_EXPIRATION
    }))
}

fn login_response(
    account_id: &str,
    user_id: &str,
    access_token: &str,
    refresh_token: &str,
    response_account_id: Option<&str>,
) -> Result<Vec<u8>, serde_json::Error> {
    login_response_with_id_token(
        &id_token(account_id, user_id),
        access_token,
        refresh_token,
        response_account_id,
    )
}

fn login_response_with_id_token(
    id_token: &str,
    access_token: &str,
    refresh_token: &str,
    response_account_id: Option<&str>,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::json!({
        "id_token": id_token,
        "access_token": access_token,
        "refresh_token": refresh_token,
        "account_id": response_account_id,
        "token_type": "Bearer",
        "expires_in": 3600
    }))
}

fn auth_root(directory: &tempfile::TempDir) -> Result<NornAuthRoot, Box<dyn Error>> {
    NornAuthRoot::try_from(directory.path().join("auth")).map_err(Into::into)
}

fn persist(auth_root: &NornAuthRoot, auth: &AuthDotJson) -> Result<(), Box<dyn Error>> {
    let timing = OAuthHttpOptions::default().credential_lock_timing()?;
    let transaction = CredentialTransaction::acquire(auth_root, timing)?;
    let snapshot = transaction.snapshot()?;
    transaction.save_if_revision(snapshot.revision.as_ref(), auth)?;
    Ok(())
}

async fn assert_request_headers(
    manager: std::sync::Arc<AuthManager>,
    expected_access_token: &str,
    expected_account_id: &str,
) -> TestResult {
    let provider = OAuthAuthProvider::from_manager(manager);
    let request = provider
        .apply_auth(reqwest::Client::new().post("https://example.invalid/v1/responses"))
        .await?
        .build()?;
    let authorization = request
        .headers()
        .get(reqwest::header::AUTHORIZATION)
        .ok_or_else(|| std::io::Error::other("Authorization header is missing"))?
        .to_str()?;
    let account_id = request
        .headers()
        .get("chatgpt-account-id")
        .ok_or_else(|| std::io::Error::other("chatgpt-account-id header is missing"))?
        .to_str()?;

    assert_eq!(authorization, format!("Bearer {expected_access_token}"));
    assert_eq!(account_id, expected_account_id);
    Ok(())
}

#[tokio::test]
async fn namespaced_login_identity_survives_durable_reload_and_reaches_request_header() -> TestResult
{
    let directory = tempfile::tempdir()?;
    let auth_root = auth_root(&directory)?;
    let access_token = access_token("login-access-fixture");
    let response = login_response(
        "login-account-fixture",
        "login-user-fixture",
        &access_token,
        "login-refresh-fixture",
        None,
    )?;
    let auth = auth_from_response_fixture(&response)?;

    persist(&auth_root, &auth)?;
    let manager = AuthManager::shared_for_tests(
        auth_root,
        AuthCredentialsStoreMode::File,
        "http://127.0.0.1:9/unused".to_owned(),
    )
    .await?;

    assert_request_headers(manager, &access_token, "login-account-fixture").await
}

#[tokio::test]
async fn flat_login_identity_survives_durable_reload_and_reaches_request_headers() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root = auth_root(&directory)?;
    let access_token = access_token("flat-login-access-fixture");
    let response = login_response_with_id_token(
        &flat_id_token("flat-login-account-fixture", "flat-login-user-fixture"),
        &access_token,
        "flat-login-refresh-fixture",
        None,
    )?;
    let auth = auth_from_response_fixture(&response)?;

    persist(&auth_root, &auth)?;
    let manager = AuthManager::shared_for_tests(
        auth_root,
        AuthCredentialsStoreMode::File,
        "http://127.0.0.1:9/unused".to_owned(),
    )
    .await?;

    assert_request_headers(manager, &access_token, "flat-login-account-fixture").await
}

#[tokio::test]
async fn namespaced_refresh_identity_survives_durable_reload_and_reaches_request_header()
-> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root = auth_root(&directory)?;
    let initial_access = access_token("initial-access-fixture");
    let initial = auth_from_response_fixture(&login_response(
        "refresh-account-fixture",
        "refresh-user-fixture",
        &initial_access,
        "initial-refresh-fixture",
        None,
    )?)?;
    persist(&auth_root, &initial)?;

    let refreshed_access = access_token("refreshed-access-fixture");
    let authority = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": id_token("refresh-account-fixture", "refresh-user-fixture"),
            "access_token": refreshed_access,
            "refresh_token": "rotated-refresh-fixture"
        })))
        .mount(&authority)
        .await;
    let manager = AuthManager::shared_for_tests(
        auth_root.clone(),
        AuthCredentialsStoreMode::File,
        authority.uri(),
    )
    .await?;

    manager.refresh_token_from_authority().await?;
    drop(manager);
    let reloaded = AuthManager::shared_for_tests(
        auth_root,
        AuthCredentialsStoreMode::File,
        format!("{}/unused-after-reload", authority.uri()),
    )
    .await?;

    assert_request_headers(
        reloaded,
        &access_token("refreshed-access-fixture"),
        "refresh-account-fixture",
    )
    .await?;
    let requests = authority
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))?;
    assert_eq!(requests.len(), 1);
    Ok(())
}

#[tokio::test]
async fn flat_refresh_identity_survives_durable_reload_and_reaches_request_headers() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root = auth_root(&directory)?;
    let initial_access = access_token("flat-initial-access-fixture");
    let initial = auth_from_response_fixture(&login_response_with_id_token(
        &flat_id_token("flat-refresh-account-fixture", "flat-refresh-user-fixture"),
        &initial_access,
        "flat-initial-refresh-fixture",
        None,
    )?)?;
    persist(&auth_root, &initial)?;

    let refreshed_access = access_token("flat-refreshed-access-fixture");
    let authority = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": flat_id_token(
                "flat-refresh-account-fixture",
                "flat-refresh-user-fixture"
            ),
            "access_token": refreshed_access,
            "refresh_token": "flat-rotated-refresh-fixture"
        })))
        .mount(&authority)
        .await;
    let manager = AuthManager::shared_for_tests(
        auth_root.clone(),
        AuthCredentialsStoreMode::File,
        authority.uri(),
    )
    .await?;

    manager.refresh_token_from_authority().await?;
    drop(manager);
    let reloaded = AuthManager::shared_for_tests(
        auth_root,
        AuthCredentialsStoreMode::File,
        format!("{}/unused-after-reload", authority.uri()),
    )
    .await?;

    assert_request_headers(
        reloaded,
        &access_token("flat-refreshed-access-fixture"),
        "flat-refresh-account-fixture",
    )
    .await?;
    let requests = authority
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))?;
    assert_eq!(requests.len(), 1);
    Ok(())
}

#[test]
fn conflicting_login_identity_is_rejected_without_disclosure() -> TestResult {
    let response = login_response(
        "claim-account-fixture",
        "claim-user-fixture",
        &access_token("conflicting-login-access"),
        "conflicting-login-refresh",
        Some("response-account-fixture"),
    )?;

    let result = auth_from_response_fixture(&response);
    let error = result.err().ok_or_else(|| {
        std::io::Error::other("conflicting login identity unexpectedly produced credentials")
    })?;
    let rendered = error.to_string();

    assert!(rendered.contains("conflicting account identity metadata"));
    assert!(!rendered.contains("claim-account-fixture"));
    assert!(!rendered.contains("response-account-fixture"));
    Ok(())
}

#[tokio::test]
async fn conflicting_refresh_identity_is_rejected_before_durable_replacement() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root = auth_root(&directory)?;
    let initial = auth_from_response_fixture(&login_response(
        "stable-account-fixture",
        "stable-user-fixture",
        &access_token("stable-access-fixture"),
        "stable-refresh-fixture",
        None,
    )?)?;
    persist(&auth_root, &initial)?;
    let auth_path = auth_root.as_path().join(super::AUTH_JSON_FILE);
    let before = std::fs::read(&auth_path)?;

    let authority = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": id_token("replacement-account-fixture", "stable-user-fixture"),
            "access_token": access_token("replacement-access-fixture"),
            "refresh_token": "replacement-refresh-fixture"
        })))
        .mount(&authority)
        .await;
    let manager =
        AuthManager::shared_for_tests(auth_root, AuthCredentialsStoreMode::File, authority.uri())
            .await?;

    let result = manager.refresh_token_from_authority().await;
    let Err(RefreshTokenError::Indeterminate(reason)) = result else {
        return Err(std::io::Error::other(format!(
            "conflicting refresh identity had unexpected result: {result:?}"
        ))
        .into());
    };
    let after = std::fs::read(auth_path)?;

    assert!(reason.contains("conflicting account identity metadata"));
    assert!(!reason.contains("stable-account-fixture"));
    assert!(!reason.contains("replacement-account-fixture"));
    assert_eq!(after, before);
    Ok(())
}
