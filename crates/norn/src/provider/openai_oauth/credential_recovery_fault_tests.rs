use std::collections::BTreeMap;
use std::error::Error;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth_root::{NornAuthRoot, resolve_norn_auth_root};
use super::super::storage::{AUTH_JSON_FILE, AuthCredentialsStoreMode, save_auth_dot_json};
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::io::{RecoveryFaultPoint, arm_recovery_fault};
use super::*;
use crate::provider::openai_oauth::{AuthManager, RefreshTokenError};

type TestResult = Result<(), Box<dyn Error>>;

fn auth_document(access: &str, refresh: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("recovery-fault-account"),
        access_token: access.to_owned(),
        refresh_token: refresh.to_owned(),
        account_id: Some("recovery-fault-account".to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

fn success_response() -> serde_json::Value {
    serde_json::json!({
        "id_token": IdTokenInfo::create_for_testing("recovery-fault-account").raw_jwt,
        "access_token": "rotated-access",
        "refresh_token": "rotated-refresh",
        "account_id": "recovery-fault-account"
    })
}

fn seeded_root(directory: &tempfile::TempDir) -> Result<NornAuthRoot, Box<dyn Error>> {
    let path = directory.path().join("auth");
    save_auth_dot_json(&path, &auth_document("seed-access", "seed-refresh"))?;
    resolve_norn_auth_root(Some(path)).map_err(Into::into)
}

fn marker_state(root: &NornAuthRoot) -> Result<Option<String>, Box<dyn Error>> {
    let path = root.as_path().join(JOURNAL_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(value
        .pointer("/phase/state")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

fn stored_access(root: &NornAuthRoot) -> Result<String, Box<dyn Error>> {
    let stored: AuthDotJson =
        serde_json::from_slice(&std::fs::read(root.as_path().join(AUTH_JSON_FILE))?)?;
    stored
        .tokens
        .map(|tokens| tokens.access_token)
        .ok_or_else(|| std::io::Error::other("stored fault fixture has no tokens").into())
}

async fn request_count(server: &MockServer) -> Result<usize, std::io::Error> {
    server
        .received_requests()
        .await
        .map(|requests| requests.len())
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))
}

async fn success_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn marker_publication_faults_fail_before_dispatch_with_typed_state() -> TestResult {
    let cases = [
        (RecoveryFaultPoint::MarkerCreate, None),
        (RecoveryFaultPoint::MarkerWriteSync, None),
        (RecoveryFaultPoint::MarkerRename, None),
        (RecoveryFaultPoint::MarkerDirSync, Some("outcome_unknown")),
    ];

    for (point, expected_marker) in cases {
        let server = success_server().await;
        let directory = tempfile::tempdir()?;
        let root = seeded_root(&directory)?;
        let manager = AuthManager::shared_for_tests(
            root.clone(),
            AuthCredentialsStoreMode::File,
            server.uri(),
        )
        .await?;
        let fault = arm_recovery_fault(root.as_path(), point);

        let result = manager.refresh_token_from_authority().await;

        assert!(fault.was_triggered());
        assert!(matches!(&result, Err(RefreshTokenError::Coordination(_))));
        assert_eq!(request_count(&server).await?, 0);
        assert_eq!(stored_access(&root)?, "seed-access");
        assert_eq!(marker_state(&root)?.as_deref(), expected_marker);
        let rendered = format!("{result:?}");
        for secret in ["seed-access", "seed-refresh"] {
            assert!(!rendered.contains(secret));
        }

        if point == RecoveryFaultPoint::MarkerDirSync {
            let blocked = manager.refresh_token_from_authority().await;
            assert!(matches!(blocked, Err(RefreshTokenError::Indeterminate(_))));
            assert_eq!(request_count(&server).await?, 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn auth_publication_fault_retains_commit_and_converges_without_replay() -> TestResult {
    let server = success_server().await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let manager =
        AuthManager::shared_for_tests(root.clone(), AuthCredentialsStoreMode::File, server.uri())
            .await?;
    let fault = arm_recovery_fault(root.as_path(), RecoveryFaultPoint::AuthPublication);

    let first = manager.refresh_token_from_authority().await;

    assert!(fault.was_triggered());
    assert!(matches!(first, Err(RefreshTokenError::Undurable(_))));
    assert_eq!(stored_access(&root)?, "seed-access");
    assert_eq!(marker_state(&root)?.as_deref(), Some("commit_pending"));
    assert_eq!(request_count(&server).await?, 1);

    manager.refresh_token_from_authority().await?;

    assert_eq!(stored_access(&root)?, "rotated-access");
    assert_eq!(marker_state(&root)?, None);
    assert_eq!(request_count(&server).await?, 1);
    Ok(())
}

#[tokio::test]
async fn marker_deletion_faults_remain_typed_and_converge_without_replay() -> TestResult {
    let cases = [
        (RecoveryFaultPoint::MarkerDelete, Some("commit_pending")),
        (RecoveryFaultPoint::MarkerDeleteDirSync, None),
    ];

    for (point, expected_marker) in cases {
        let server = success_server().await;
        let directory = tempfile::tempdir()?;
        let root = seeded_root(&directory)?;
        let manager = AuthManager::shared_for_tests(
            root.clone(),
            AuthCredentialsStoreMode::File,
            server.uri(),
        )
        .await?;
        let fault = arm_recovery_fault(root.as_path(), point);

        let first = manager.refresh_token_from_authority().await;

        assert!(fault.was_triggered());
        assert!(matches!(first, Err(RefreshTokenError::Undurable(_))));
        assert_eq!(stored_access(&root)?, "rotated-access");
        assert_eq!(marker_state(&root)?.as_deref(), expected_marker);
        assert_eq!(request_count(&server).await?, 1);

        manager.refresh_token_from_authority().await?;

        assert_eq!(stored_access(&root)?, "rotated-access");
        assert_eq!(marker_state(&root)?, None);
        assert_eq!(request_count(&server).await?, 1);
    }
    Ok(())
}
