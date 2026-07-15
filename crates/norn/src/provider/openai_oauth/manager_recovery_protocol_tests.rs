use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth_root::{NornAuthRoot, resolve_norn_auth_root};
use super::super::credential_lock_timing::CredentialLockTiming;
use super::super::storage::{AUTH_JSON_FILE, AuthCredentialsStoreMode, save_auth_dot_json};
use super::super::types::{AuthDotJson, ChatGptTokens, CodexAuth, IdTokenInfo};
use super::*;
use crate::provider::openai_oauth::{AuthManager, AuthManagerBuildError, RefreshTokenError};

#[path = "credential_recovery_restart_support.rs"]
mod restart_support;

use restart_support::WithholdingAuthority;

type TestResult = Result<(), Box<dyn Error>>;

const RESTART_CHILD_MODE: &str = "NORN_OAUTH_RECOVERY_RESTART_CHILD";
const RESTART_CHILD_ROOT: &str = "NORN_OAUTH_RECOVERY_RESTART_ROOT";
const RESTART_CHILD_TOKEN_URL: &str = "NORN_OAUTH_RECOVERY_RESTART_TOKEN_URL";
const RESTART_TEST_NAME: &str = "provider::openai_oauth::credential_recovery::protocol_tests::ambiguous_outcome_survives_process_restart_without_replay";

fn auth_document(access: &str, refresh: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("recovery-account"),
        access_token: access.to_owned(),
        refresh_token: refresh.to_owned(),
        account_id: Some("recovery-account".to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

fn success_response() -> Value {
    serde_json::json!({
        "id_token": IdTokenInfo::create_for_testing("recovery-account").raw_jwt,
        "access_token": "rotated-access",
        "refresh_token": "rotated-refresh",
        "account_id": "recovery-account"
    })
}

fn marker_state(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value
        .pointer("/phase/state")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

async fn manager_for(
    root: NornAuthRoot,
    server: &MockServer,
) -> Result<Arc<AuthManager>, AuthManagerBuildError> {
    AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, server.uri()).await
}

async fn manager_for_url(
    root: NornAuthRoot,
    token_url: String,
) -> Result<Arc<AuthManager>, AuthManagerBuildError> {
    AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, token_url).await
}

async fn request_count(server: &MockServer) -> Result<usize, std::io::Error> {
    server
        .received_requests()
        .await
        .map(|requests| requests.len())
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))
}

fn seeded_root(directory: &tempfile::TempDir) -> Result<NornAuthRoot, Box<dyn Error>> {
    let path = directory.path().join("auth");
    save_auth_dot_json(&path, &auth_document("seed-access", "seed-refresh"))?;
    resolve_norn_auth_root(Some(path)).map_err(Into::into)
}

fn lock_timing() -> Result<CredentialLockTiming, Box<dyn Error>> {
    CredentialLockTiming::new(Duration::from_secs(1), Duration::from_millis(1)).map_err(Into::into)
}

fn required_revision(snapshot: &CredentialSnapshot) -> Result<CredentialRevision, std::io::Error> {
    snapshot
        .revision
        .clone()
        .ok_or_else(|| std::io::Error::other("recovery fixture revision is missing"))
}

async fn wait_until_dropped(manager: &std::sync::Weak<AuthManager>) -> Result<(), std::io::Error> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while manager.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|elapsed| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("retired OAuth manager was not released: {elapsed}"),
        )
    })
}

#[tokio::test]
async fn outcome_unknown_is_durable_before_http_dispatch_and_clears_after_commit() -> TestResult {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let auth_path = root.as_path().join(AUTH_JSON_FILE);
    let marker_path = root.as_path().join(JOURNAL_FILE);
    let original = std::fs::read(&auth_path)?;
    let saw_marker = Arc::new(AtomicBool::new(false));
    let saw_original = Arc::new(AtomicBool::new(false));
    let observer_marker = Arc::clone(&saw_marker);
    let observer_original = Arc::clone(&saw_original);
    let observed_marker_path = marker_path.clone();
    let observed_auth_path = auth_path.clone();
    let expected_auth_bytes = original.clone();
    let response = ResponseTemplate::new(200).set_body_json(success_response());
    Mock::given(method("POST"))
        .respond_with(move |_request: &wiremock::Request| {
            observer_marker.store(
                marker_state(&observed_marker_path).as_deref() == Some("outcome_unknown"),
                Ordering::SeqCst,
            );
            observer_original.store(
                std::fs::read(&observed_auth_path).is_ok_and(|raw| raw == expected_auth_bytes),
                Ordering::SeqCst,
            );
            response.clone()
        })
        .mount(&server)
        .await;

    let manager = manager_for(root, &server).await?;
    manager.refresh_token_from_authority().await?;

    assert!(saw_marker.load(Ordering::SeqCst));
    assert!(saw_original.load(Ordering::SeqCst));
    assert!(!marker_path.exists());
    assert_ne!(std::fs::read(auth_path)?, original);
    Ok(())
}

#[tokio::test]
async fn definite_no_rotation_response_durably_cleans_journal() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {"code": "refresh_token_expired"}
        })))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let marker_path = root.as_path().join(JOURNAL_FILE);
    let manager = manager_for(root, &server).await?;

    let result = manager.refresh_token_from_authority().await;

    assert!(matches!(result, Err(RefreshTokenError::Permanent(_))));
    assert!(!marker_path.exists());
    assert_eq!(request_count(&server).await?, 1);
    Ok(())
}

#[tokio::test]
async fn ambiguous_authority_outcome_retains_barrier_and_never_replays() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let marker_path = root.as_path().join(JOURNAL_FILE);
    let manager = manager_for(root, &server).await?;

    let first = manager.refresh_token_from_authority().await;
    let second = manager.refresh_token_from_authority().await;

    assert!(matches!(first, Err(RefreshTokenError::Indeterminate(_))));
    assert!(matches!(second, Err(RefreshTokenError::Indeterminate(_))));
    assert_eq!(
        marker_state(&marker_path).as_deref(),
        Some("outcome_unknown")
    );
    assert_eq!(request_count(&server).await?, 1);
    Ok(())
}

#[tokio::test]
async fn ambiguous_outcome_blocks_a_reconstructed_manager_without_replay() -> TestResult {
    let mut authority = WithholdingAuthority::start().await?;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let manager = manager_for_url(root.clone(), authority.url().to_owned()).await?;
    let worker_manager = Arc::clone(&manager);
    let attempt = tokio::spawn(async move { worker_manager.refresh_token_from_authority().await });
    authority.wait_for_dispatch().await?;
    assert_eq!(
        marker_state(&root.as_path().join(JOURNAL_FILE)).as_deref(),
        Some("outcome_unknown")
    );
    authority.close_without_response();
    let first = tokio::time::timeout(Duration::from_secs(5), attempt)
        .await
        .map_err(|elapsed| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("interrupted refresh attempt did not finish: {elapsed}"),
            )
        })??;
    authority.finish().await?;
    let retired = Arc::downgrade(&manager);
    drop(manager);
    wait_until_dropped(&retired).await?;

    assert!(matches!(first, Err(RefreshTokenError::Indeterminate(_))));
    let trap = MockServer::start().await;
    let reconstructed = manager_for(root, &trap).await?;
    let second = reconstructed.refresh_token_from_authority().await;
    assert!(matches!(second, Err(RefreshTokenError::Indeterminate(_))));
    assert_eq!(request_count(&trap).await?, 0);
    Ok(())
}

#[tokio::test]
async fn ambiguous_outcome_survives_process_restart_without_replay() -> TestResult {
    if std::env::var_os(RESTART_CHILD_MODE).is_some() {
        return run_restart_child().await;
    }

    let mut authority = WithholdingAuthority::start().await?;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let mut child = spawn_restart_child(root.as_path(), authority.url())?;
    authority.wait_for_dispatch().await?;
    assert_eq!(
        marker_state(&root.as_path().join(JOURNAL_FILE)).as_deref(),
        Some("outcome_unknown")
    );
    child.kill().await?;
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|elapsed| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("terminated recovery child did not exit: {elapsed}"),
            )
        })??;
    assert!(!status.success());
    authority.finish().await?;

    let trap = MockServer::start().await;
    let reconstructed = manager_for(root, &trap).await?;
    let result = reconstructed.refresh_token_from_authority().await;
    assert!(matches!(result, Err(RefreshTokenError::Indeterminate(_))));
    assert_eq!(request_count(&trap).await?, 0);
    Ok(())
}

#[tokio::test]
async fn commit_pending_with_proposed_auth_converges_without_http() -> TestResult {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing()?)?;
    let current = auth_document("seed-access", "seed-refresh");
    let snapshot = transaction.snapshot()?;
    let revision = required_revision(&snapshot)?;
    let mut operation = transaction.begin_refresh_recovery(&revision, &current)?;
    let proposed = auth_document("rotated-access", "rotated-refresh");
    let (_, proposed_revision) = serialize_auth(&proposed)?;
    transaction.mark_refresh_commit_pending(&mut operation, &proposed_revision)?;
    transaction.save_if_revision(Some(&revision), &proposed)?;
    drop(transaction);

    let manager = manager_for(root.clone(), &server).await?;
    let auth = manager
        .auth()
        .await?
        .ok_or_else(|| std::io::Error::other("recovered manager returned no credential"))?;
    let CodexAuth::ChatGpt(auth) = auth else {
        return Err(std::io::Error::other("recovered manager returned an API key").into());
    };
    let tokens = auth
        .tokens
        .ok_or_else(|| std::io::Error::other("recovered credential has no tokens"))?;
    assert_eq!(tokens.access_token, "rotated-access");
    assert!(!root.as_path().join(JOURNAL_FILE).exists());
    assert_eq!(request_count(&server).await?, 0);
    Ok(())
}

#[tokio::test]
async fn commit_pending_with_prior_auth_remains_blocked_without_http() -> TestResult {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing()?)?;
    let current = auth_document("seed-access", "seed-refresh");
    let revision = required_revision(&transaction.snapshot()?)?;
    let mut operation = transaction.begin_refresh_recovery(&revision, &current)?;
    let proposed = auth_document("rotated-access", "rotated-refresh");
    let (_, proposed_revision) = serialize_auth(&proposed)?;
    transaction.mark_refresh_commit_pending(&mut operation, &proposed_revision)?;
    drop(transaction);

    let manager = manager_for(root.clone(), &server).await?;
    let result = manager.auth().await;

    assert!(matches!(result, Err(RefreshTokenError::Indeterminate(_))));
    assert_eq!(
        marker_state(&root.as_path().join(JOURNAL_FILE)).as_deref(),
        Some("commit_pending")
    );
    assert_eq!(request_count(&server).await?, 0);
    Ok(())
}

#[tokio::test]
async fn parsed_success_reaches_commit_pending_before_auth_publication() -> TestResult {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let root = seeded_root(&directory)?;
    let auth_path = root.as_path().join(AUTH_JSON_FILE);
    let marker_path = root.as_path().join(JOURNAL_FILE);
    let compact = serde_json::to_vec(&auth_document("seed-access", "seed-refresh"))?;
    let rewrote_auth = Arc::new(AtomicBool::new(false));
    let observer_rewrite = Arc::clone(&rewrote_auth);
    let observed_auth_path = auth_path.clone();
    let response = ResponseTemplate::new(200).set_body_json(success_response());
    Mock::given(method("POST"))
        .respond_with(move |_request: &wiremock::Request| {
            observer_rewrite.store(
                std::fs::write(&observed_auth_path, &compact).is_ok(),
                Ordering::SeqCst,
            );
            response.clone()
        })
        .mount(&server)
        .await;

    let manager = manager_for(root, &server).await?;
    let result = manager.refresh_token_from_authority().await;

    assert!(rewrote_auth.load(Ordering::SeqCst));
    assert!(matches!(result, Err(RefreshTokenError::Conflict(_))));
    assert_eq!(
        marker_state(&marker_path).as_deref(),
        Some("commit_pending")
    );
    let stored: AuthDotJson = serde_json::from_slice(&std::fs::read(auth_path)?)?;
    let tokens = stored
        .tokens
        .ok_or_else(|| std::io::Error::other("stored test credential has no tokens"))?;
    assert_eq!(tokens.access_token, "seed-access");
    assert_eq!(request_count(&server).await?, 1);
    Ok(())
}

async fn run_restart_child() -> TestResult {
    let root = std::env::var_os(RESTART_CHILD_ROOT)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| std::io::Error::other("missing recovery child root"))?;
    let token_url = required_child_string(RESTART_CHILD_TOKEN_URL)?;
    let auth_root = NornAuthRoot::try_from(root)?;
    let manager =
        AuthManager::shared_for_tests(auth_root, AuthCredentialsStoreMode::File, token_url).await?;
    manager.refresh_token_from_authority().await?;
    Err(std::io::Error::other("recovery child returned before termination").into())
}

fn spawn_restart_child(
    root: &Path,
    token_url: &str,
) -> Result<tokio::process::Child, std::io::Error> {
    Command::new(std::env::current_exe()?)
        .args(["--exact", RESTART_TEST_NAME, "--nocapture"])
        .env(RESTART_CHILD_MODE, "1")
        .env(RESTART_CHILD_ROOT, root)
        .env(RESTART_CHILD_TOKEN_URL, token_url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

fn required_child_string(name: &str) -> Result<String, std::io::Error> {
    std::env::var(name).map_err(|error| {
        std::io::Error::other(format!(
            "invalid child environment variable {name}: {error}"
        ))
    })
}
