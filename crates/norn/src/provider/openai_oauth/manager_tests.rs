use std::error::Error;
use std::sync::Arc;

use base64::Engine as _;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth_root::NornAuthRoot;
use super::super::storage::{AuthCredentialsStoreMode, save_auth_dot_json};
use super::super::types::{AuthDotJson, CodexAuth};
use super::*;

type TestResult = Result<(), Box<dyn Error>>;

/// Builds a file-owned manager whose refresh exchange targets the mock server.
async fn seeded_manager(
    server: &MockServer,
) -> Result<(tempfile::TempDir, Arc<AuthManager>), Box<dyn Error>> {
    file_manager_with_auth(server, seed_auth("seed-refresh")?).await
}

async fn file_manager_with_auth(
    server: &MockServer,
    auth: AuthDotJson,
) -> Result<(tempfile::TempDir, Arc<AuthManager>), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    save_auth_dot_json(&auth_root_path, &auth)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager =
        AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, server.uri()).await?;
    Ok((directory, manager))
}

fn static_manager_with_auth(
    server: &MockServer,
    auth: AuthDotJson,
) -> Result<Arc<AuthManager>, AuthManagerBuildError> {
    AuthManager::from_static_auth_with_token_url(CodexAuth::ChatGpt(Box::new(auth)), server.uri())
}

fn seed_auth(refresh_token: &str) -> Result<AuthDotJson, serde_json::Error> {
    serde_json::from_value(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token_fixture(),
            "access_token": "seed-access-token",
            "refresh_token": refresh_token,
            "account_id": "seed-account",
        }
    }))
}

fn id_token_fixture() -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "seed-account"
            }
        })
        .to_string(),
    );
    format!("{header}.{claims}.")
}

fn access_token_fixture(expiration: i64) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::json!({"exp": expiration}).to_string());
    format!("{header}.{claims}.")
}

fn refresh_response_body() -> serde_json::Value {
    serde_json::json!({
        "id_token": id_token_fixture(),
        "access_token": "new-access-token",
        "refresh_token": "rotated-refresh-token",
        "account_id": "seed-account",
    })
}

async fn access_token(manager: &Arc<AuthManager>) -> Result<String, RefreshTokenError> {
    let auth = manager
        .auth()
        .await?
        .ok_or_else(|| RefreshTokenError::Permanent("test credential missing".to_owned()))?;
    auth.get_token()
        .map(str::to_owned)
        .map_err(|error| RefreshTokenError::Permanent(error.to_string()))
}

async fn received_request_count(server: &MockServer) -> Result<usize, std::io::Error> {
    server
        .received_requests()
        .await
        .map(|requests| requests.len())
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))
}

#[tokio::test]
async fn unknown_expiry_does_not_trigger_an_age_based_refresh() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
        .mount(&server)
        .await;

    let (_directory, manager) = seeded_manager(&server).await?;
    assert_eq!(access_token(&manager).await?, "seed-access-token");
    assert_eq!(
        received_request_count(&server).await?,
        0,
        "opaque access tokens must not refresh from an arbitrary age heuristic"
    );

    manager.refresh_token_from_authority().await?;
    assert_eq!(received_request_count(&server).await?, 1);
    Ok(())
}

#[tokio::test]
async fn known_expiry_refreshes_before_the_token_is_served() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
        .mount(&server)
        .await;

    let mut auth = seed_auth("seed-refresh")?;
    if let Some(tokens) = auth.tokens.as_mut() {
        tokens.access_token = access_token_fixture(1);
    }
    let (_directory, manager) = file_manager_with_auth(&server, auth).await?;

    assert_eq!(access_token(&manager).await?, "new-access-token");
    assert_eq!(received_request_count(&server).await?, 1);
    Ok(())
}

#[tokio::test]
async fn expired_access_without_refresh_fails_before_network_io() -> TestResult {
    let server = MockServer::start().await;
    let mut auth = seed_auth("seed-refresh")?;
    if let Some(tokens) = auth.tokens.as_mut() {
        tokens.access_token = access_token_fixture(1);
        tokens.refresh_token.clear();
    }
    let manager = static_manager_with_auth(&server, auth)?;

    assert!(matches!(
        manager.auth().await,
        Err(RefreshTokenError::Permanent(_))
    ));
    assert_eq!(received_request_count(&server).await?, 0);
    Ok(())
}

#[tokio::test]
async fn concurrent_refreshes_collapse_into_single_exchange() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(150))
                .set_body_json(refresh_response_body()),
        )
        .mount(&server)
        .await;

    let (_directory, manager) = seeded_manager(&server).await?;
    let (first, second) = tokio::join!(
        manager.refresh_token_from_authority(),
        manager.refresh_token_from_authority(),
    );
    assert!(first.is_ok(), "first refresh failed: {first:?}");
    assert!(second.is_ok(), "second refresh failed: {second:?}");
    assert_eq!(
        received_request_count(&server).await?,
        1,
        "concurrent refreshes must share one exchange"
    );
    assert_eq!(access_token(&manager).await?, "new-access-token");
    Ok(())
}

#[tokio::test]
async fn canceled_waiter_does_not_cancel_owned_refresh_attempt() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(150))
                .set_body_json(refresh_response_body()),
        )
        .mount(&server)
        .await;
    let (_directory, manager) = seeded_manager(&server).await?;
    let first_manager = Arc::clone(&manager);
    let first = tokio::spawn(async move { first_manager.refresh_token_from_authority().await });

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if manager.refresh_attempt.lock().await.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    first.abort();

    manager.refresh_token_from_authority().await?;
    assert_eq!(
        received_request_count(&server).await?,
        1,
        "cancellation must not restart exchange"
    );
    assert_eq!(access_token(&manager).await?, "new-access-token");
    Ok(())
}

#[tokio::test]
async fn concurrent_ambiguous_failure_is_shared_and_blocks_replay() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_delay(std::time::Duration::from_millis(100)))
        .mount(&server)
        .await;

    let (_directory, manager) = seeded_manager(&server).await?;
    let (first, overlapping) = tokio::join!(
        manager.refresh_token_from_authority(),
        manager.refresh_token_from_authority(),
    );
    assert!(
        matches!(first, Err(RefreshTokenError::Indeterminate(_))),
        "expected indeterminate failure from HTTP 500, got {first:?}"
    );
    assert!(
        matches!(overlapping, Err(RefreshTokenError::Indeterminate(_))),
        "overlapping waiter must receive the same failure: {overlapping:?}"
    );

    let later = manager.refresh_token_from_authority().await;
    assert!(matches!(later, Err(RefreshTokenError::Indeterminate(_))));
    assert_eq!(
        received_request_count(&server).await?,
        1,
        "an ambiguous response must not replay the prior refresh lineage"
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_request_timeout_is_shared_without_poisoning_later_attempts() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(408).set_delay(std::time::Duration::from_millis(100)))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
        .with_priority(2)
        .mount(&server)
        .await;

    let (_directory, manager) = seeded_manager(&server).await?;
    let (first, overlapping) = tokio::join!(
        manager.refresh_token_from_authority(),
        manager.refresh_token_from_authority(),
    );
    assert!(matches!(first, Err(RefreshTokenError::Transient(_))));
    assert!(matches!(overlapping, Err(RefreshTokenError::Transient(_))));

    manager.refresh_token_from_authority().await?;
    assert_eq!(access_token(&manager).await?, "new-access-token");
    assert_eq!(received_request_count(&server).await?, 2);
    Ok(())
}

#[tokio::test]
async fn sequential_refreshes_each_exchange() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
        .mount(&server)
        .await;

    let (_directory, manager) = seeded_manager(&server).await?;
    manager.refresh_token_from_authority().await?;
    manager.refresh_token_from_authority().await?;

    assert_eq!(
        received_request_count(&server).await?,
        2,
        "sequential refreshes must not deduplicate"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn persist_failure_is_shared_and_blocks_rotated_lineage() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir()?;
    let auth_root_path = dir.path().join("auth");
    std::fs::create_dir(&auth_root_path)?;
    save_auth_dot_json(&auth_root_path, &seed_auth("seed-refresh")?)?;
    let auth_root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager =
        AuthManager::shared_for_tests(auth_root, AuthCredentialsStoreMode::File, server.uri())
            .await?;

    std::fs::set_permissions(&auth_root_path, std::fs::Permissions::from_mode(0o500))?;
    let (first, second) = tokio::join!(
        manager.refresh_token_from_authority(),
        manager.refresh_token_from_authority(),
    );
    assert!(
        matches!(first, Err(RefreshTokenError::Undurable(_))),
        "first waiter must observe the persistence failure: {first:?}"
    );
    assert!(
        matches!(second, Err(RefreshTokenError::Undurable(_))),
        "second waiter must observe the persistence failure: {second:?}"
    );
    let cached_token = match manager.auth.lock().await.clone() {
        CachedAuthState::PendingPersistence { refreshed, .. } => refreshed
            .tokens
            .as_ref()
            .map(|tokens| tokens.access_token.clone()),
        CachedAuthState::Missing
        | CachedAuthState::Ready { .. }
        | CachedAuthState::Indeterminate { .. } => None,
    };
    assert_eq!(cached_token.as_deref(), Some("new-access-token"));
    assert!(matches!(
        manager.auth().await,
        Err(RefreshTokenError::Undurable(_))
    ));

    std::fs::set_permissions(&auth_root_path, std::fs::Permissions::from_mode(0o700))?;
    manager.refresh_token_from_authority().await?;
    assert_eq!(access_token(&manager).await?, "new-access-token");
    assert_eq!(
        received_request_count(&server).await?,
        1,
        "persistence retry must not rotate again"
    );
    Ok(())
}

#[tokio::test]
async fn file_manager_registry_shares_one_owner_and_reclaims_dead_entries() -> TestResult {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    save_auth_dot_json(&auth_root_path, &seed_auth("seed-refresh")?)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let first = AuthManager::shared_for_tests_with_options(
        root.clone(),
        AuthCredentialsStoreMode::File,
        server.uri(),
        OAuthHttpOptions::default(),
    )
    .await?;
    let same = AuthManager::shared_for_tests_with_options(
        root.clone(),
        AuthCredentialsStoreMode::File,
        server.uri(),
        OAuthHttpOptions::default(),
    )
    .await?;

    assert!(Arc::ptr_eq(&first, &same));
    let conflicting = OAuthHttpOptions {
        request_timeout: std::time::Duration::from_secs(11),
        ..OAuthHttpOptions::default()
    };
    assert!(matches!(
        AuthManager::shared_for_tests_with_options(
            root.clone(),
            AuthCredentialsStoreMode::File,
            server.uri(),
            conflicting,
        )
        .await,
        Err(AuthManagerBuildError::ConfigurationConflict)
    ));

    let old = Arc::downgrade(&first);
    drop(first);
    drop(same);
    assert!(old.upgrade().is_none());
    let replacement = AuthManager::shared_for_tests_with_options(
        root,
        AuthCredentialsStoreMode::File,
        server.uri(),
        conflicting,
    )
    .await?;
    assert!(old.upgrade().is_none());
    drop(replacement);
    Ok(())
}

#[tokio::test]
async fn static_auth_refuses_refresh_without_an_owner_sink() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response_body()))
        .mount(&server)
        .await;

    let manager = static_manager_with_auth(&server, seed_auth("seed-refresh")?)?;
    let result = manager.refresh_token_from_authority().await;
    let reason = match result {
        Err(RefreshTokenError::Permanent(reason)) => reason,
        other => {
            return Err(std::io::Error::other(format!(
                "ownerless refresh did not fail permanently: {other:?}"
            ))
            .into());
        }
    };
    assert!(reason.contains("file-backed credential owner"));
    assert_eq!(access_token(&manager).await?, "seed-access-token");
    assert_eq!(received_request_count(&server).await?, 0);
    Ok(())
}

#[tokio::test]
async fn static_auth_api_key_has_no_refresh_path() -> TestResult {
    let manager = AuthManager::from_static_auth(
        CodexAuth::from_api_key("vm-injected-key"),
        OAuthHttpOptions::default(),
    )?;
    assert_eq!(access_token(&manager).await?, "vm-injected-key");

    let result = manager.refresh_token_from_authority().await;
    assert!(
        matches!(result, Err(RefreshTokenError::Permanent(_))),
        "API-key credentials are not refreshable, got {result:?}"
    );
    Ok(())
}

const _: fn() = || {
    fn check<T: Send + Sync>() {}
    check::<AuthManager>();
    check::<Arc<AuthManager>>();
};
