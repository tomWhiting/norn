use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;

use super::super::auth_root::NornAuthRoot;
use super::super::credential_decode::MalformedCredentialReason;
use super::super::credential_lock_timing::CredentialLockTiming;
use super::super::credential_transaction::{CredentialRevision, CredentialTransaction};
use super::super::storage::{AuthCredentialsStoreMode, auth_json_path, save_auth_dot_json};
use super::super::types::{AuthDotJson, CodexAuth, IdTokenInfo};
use super::*;

type TestResult = Result<(), Box<dyn Error>>;

fn fixture_jwt(claims: &serde_json::Value) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}

fn auth_document(
    account: &str,
    access_token: &str,
    refresh_token: &str,
) -> Result<AuthDotJson, serde_json::Error> {
    let id_token = IdTokenInfo::create_for_testing(account);
    serde_json::from_value(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token.raw_jwt,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account,
        }
    }))
}

fn auth_document_with_user(
    account: &str,
    user: &str,
    access_token: &str,
    refresh_token: &str,
) -> Result<AuthDotJson, serde_json::Error> {
    let id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account,
            "chatgpt_user_id": user
        }
    }));
    serde_json::from_value(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account,
        }
    }))
}

fn raw_auth_document(id_token: &str, account: Option<&str>) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": "manager-access-secret",
            "refresh_token": "manager-refresh-secret",
            "account_id": account,
        }
    }))
}

async fn manager_for(root: NornAuthRoot) -> Result<Arc<AuthManager>, AuthManagerBuildError> {
    AuthManager::shared_for_tests(
        root,
        AuthCredentialsStoreMode::File,
        "http://127.0.0.1:9".to_owned(),
    )
    .await
}

async fn ready_revision(manager: &AuthManager) -> Result<CredentialRevision, std::io::Error> {
    match manager.auth.lock().await.clone() {
        CachedAuthState::Ready {
            revision: Some(revision),
            ..
        } => Ok(revision),
        CachedAuthState::Missing
        | CachedAuthState::Ready { revision: None, .. }
        | CachedAuthState::PendingPersistence { .. }
        | CachedAuthState::Indeterminate { .. } => {
            Err(std::io::Error::other("manager has no file revision"))
        }
    }
}

async fn set_pending(
    manager: &AuthManager,
    proposed: AuthDotJson,
    expected_revision: CredentialRevision,
) {
    *manager.auth.lock().await = CachedAuthState::PendingPersistence {
        refreshed: Box::new(proposed),
        expected_revision: Some(expected_revision),
        error: RefreshTokenError::Undurable("simulated durability failure".to_owned()),
    };
}

async fn cached_access_token(manager: &AuthManager) -> Option<String> {
    match manager.auth.lock().await.clone() {
        CachedAuthState::Ready {
            auth: CodexAuth::ChatGpt(auth),
            ..
        } => auth
            .tokens
            .as_ref()
            .map(|tokens| tokens.access_token.clone()),
        CachedAuthState::Missing
        | CachedAuthState::Ready {
            auth: CodexAuth::ApiKey(_),
            ..
        }
        | CachedAuthState::PendingPersistence { .. }
        | CachedAuthState::Indeterminate { .. } => None,
    }
}

#[tokio::test]
async fn pending_persistence_replays_exact_proposed_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    let original = auth_document("account-a", "access-a", "refresh-a")?;
    save_auth_dot_json(&auth_root_path, &original)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager = manager_for(root.clone()).await?;
    let expected_revision = ready_revision(&manager).await?;
    let proposed = auth_document("account-a", "access-b", "refresh-b")?;

    let timing = CredentialLockTiming::new(Duration::from_secs(1), Duration::from_millis(1))?;
    let transaction = CredentialTransaction::acquire(&root, timing)?;
    let published_revision = transaction.save_if_revision(Some(&expected_revision), &proposed)?;
    drop(transaction);
    set_pending(&manager, proposed.clone(), expected_revision).await;

    manager.refresh_token_from_authority().await?;
    match manager.auth.lock().await.clone() {
        CachedAuthState::Ready {
            auth: CodexAuth::ChatGpt(auth),
            revision,
        } => {
            assert_eq!(*auth, proposed);
            assert_eq!(revision, Some(published_revision));
        }
        state => {
            return Err(std::io::Error::other(format!(
                "pending credential did not converge: {state:?}"
            ))
            .into());
        }
    }
    Ok(())
}

#[tokio::test]
async fn pending_persistence_rejects_noncanonical_same_account_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    let original = auth_document("account-a", "access-a", "refresh-a")?;
    save_auth_dot_json(&auth_root_path, &original)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager = manager_for(root).await?;
    let expected_revision = ready_revision(&manager).await?;
    let proposed = auth_document("account-a", "access-b", "refresh-b")?;
    let compact = serde_json::to_vec(&proposed)?;
    std::fs::write(auth_json_path(&auth_root_path), &compact)?;
    set_pending(&manager, proposed, expected_revision).await;

    let result = manager.refresh_token_from_authority().await;
    assert!(matches!(result, Err(RefreshTokenError::Conflict(_))));
    assert!(matches!(
        manager.auth.lock().await.clone(),
        CachedAuthState::PendingPersistence {
            error: RefreshTokenError::Conflict(_),
            ..
        }
    ));
    assert_eq!(std::fs::read(auth_json_path(&auth_root_path))?, compact);
    Ok(())
}

#[tokio::test]
async fn indeterminate_recovery_requires_a_new_refresh_lineage() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    let original = auth_document("account-a", "access-a", "refresh-a")?;
    save_auth_dot_json(&auth_root_path, &original)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let manager = manager_for(root).await?;
    let observed_revision = ready_revision(&manager).await?;
    let observed_lineage = RefreshLineage::from_auth(&original)
        .ok_or_else(|| std::io::Error::other("test credential has no refresh lineage"))?;
    let poison = RefreshTokenError::Indeterminate("rotation outcome is unknown".to_owned());
    *manager.auth.lock().await = CachedAuthState::Indeterminate {
        observed_revision: Some(observed_revision),
        observed_lineage,
        error: poison,
    };

    std::fs::write(
        auth_json_path(&auth_root_path),
        serde_json::to_vec(&original)?,
    )?;
    let unchanged = manager.refresh_token_from_authority().await;
    assert!(matches!(
        unchanged,
        Err(RefreshTokenError::Indeterminate(_))
    ));

    let replacement = auth_document("account-a", "access-b", "refresh-b")?;
    save_auth_dot_json(&auth_root_path, &replacement)?;
    manager.refresh_token_from_authority().await?;
    assert_eq!(
        cached_access_token(&manager).await.as_deref(),
        Some("access-b")
    );
    Ok(())
}

#[tokio::test]
async fn auth_never_returns_an_unusable_changed_lineage_recovery() -> TestResult {
    let expired_access = fixture_jwt(&serde_json::json!({"exp": 1}));
    for (index, (access, expected_transient)) in
        [(expired_access.as_str(), true), ("header.claims", false)]
            .into_iter()
            .enumerate()
    {
        let directory = tempfile::tempdir()?;
        let auth_root_path = directory.path().join(format!("auth-{index}"));
        let original = auth_document("account-a", "access-a", "refresh-a")?;
        save_auth_dot_json(&auth_root_path, &original)?;
        let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
        let manager = manager_for(root).await?;
        let observed_revision = ready_revision(&manager).await?;
        let observed_lineage = RefreshLineage::from_auth(&original)
            .ok_or_else(|| std::io::Error::other("test credential has no refresh lineage"))?;
        *manager.auth.lock().await = CachedAuthState::Indeterminate {
            observed_revision: Some(observed_revision),
            observed_lineage,
            error: RefreshTokenError::Indeterminate("rotation outcome is unknown".to_owned()),
        };

        let replacement = auth_document("account-a", access, "refresh-b")?;
        save_auth_dot_json(&auth_root_path, &replacement)?;
        let result = manager.auth().await;

        assert!(if expected_transient {
            matches!(result, Err(RefreshTokenError::Transient(_)))
        } else {
            matches!(result, Err(RefreshTokenError::Permanent(_)))
        });
    }
    Ok(())
}

#[tokio::test]
async fn account_replacement_requires_a_new_registry_owner() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    let first_auth = auth_document("account-a", "access-a", "refresh-a")?;
    save_auth_dot_json(&auth_root_path, &first_auth)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let first = manager_for(root.clone()).await?;

    let second_auth = auth_document("account-b", "access-b", "refresh-b")?;
    save_auth_dot_json(&auth_root_path, &second_auth)?;
    assert!(matches!(
        first.auth().await,
        Err(RefreshTokenError::Conflict(_))
    ));

    std::fs::remove_file(auth_json_path(&auth_root_path))?;
    assert!(first.auth().await?.is_none());
    save_auth_dot_json(&auth_root_path, &second_auth)?;
    assert!(matches!(
        first.auth().await,
        Err(RefreshTokenError::Conflict(_))
    ));

    let second = manager_for(root).await?;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        cached_access_token(&second).await.as_deref(),
        Some("access-b")
    );
    assert!(matches!(
        first.auth().await,
        Err(RefreshTokenError::Conflict(_))
    ));
    Ok(())
}

#[tokio::test]
async fn same_workspace_user_replacement_requires_a_new_registry_owner() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    let first_auth = auth_document_with_user("workspace-a", "user-a", "access-a", "refresh-a")?;
    save_auth_dot_json(&auth_root_path, &first_auth)?;
    let root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let first = manager_for(root.clone()).await?;

    let replacement = auth_document_with_user("workspace-a", "user-b", "access-b", "refresh-b")?;
    save_auth_dot_json(&auth_root_path, &replacement)?;

    assert!(matches!(
        first.auth().await,
        Err(RefreshTokenError::Conflict(_))
    ));
    let second = manager_for(root).await?;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        cached_access_token(&second).await.as_deref(),
        Some("access-b")
    );
    Ok(())
}

#[tokio::test]
async fn manager_build_preserves_typed_malformed_reasons_before_network() -> TestResult {
    let valid_id_token = IdTokenInfo::create_for_testing("account-a");
    let cases = [
        (
            raw_auth_document("e30.e30.", None)?,
            MalformedCredentialReason::MissingAccountId,
        ),
        (
            raw_auth_document(&valid_id_token.raw_jwt, Some("account\nunsafe"))?,
            MalformedCredentialReason::InvalidAccountId,
        ),
        (
            raw_auth_document(&valid_id_token.raw_jwt, Some("account-b"))?,
            MalformedCredentialReason::ConflictingAccountIds,
        ),
        (
            raw_auth_document("malformed-id-token-secret", Some("account-a"))?,
            MalformedCredentialReason::MalformedIdTokenClaims,
        ),
    ];

    for (index, (raw, expected_reason)) in cases.into_iter().enumerate() {
        let directory = tempfile::tempdir()?;
        let auth_root_path = directory.path().join(format!("auth-{index}"));
        std::fs::create_dir(&auth_root_path)?;
        std::fs::write(auth_json_path(&auth_root_path), raw)?;
        let root = NornAuthRoot::try_from(auth_root_path.as_path())?;

        let result = manager_for(root).await;
        let reason = match result {
            Err(AuthManagerBuildError::MalformedCredential { reason }) => reason,
            other => {
                return Err(std::io::Error::other(format!(
                    "manager did not retain malformed reason: {other:?}"
                ))
                .into());
            }
        };
        assert_eq!(reason, expected_reason);
    }
    Ok(())
}

#[test]
fn refresh_lineage_debug_is_redacted() -> TestResult {
    let auth = auth_document("account-a", "access-a", "refresh-secret")?;
    let lineage = RefreshLineage::from_auth(&auth)
        .ok_or_else(|| std::io::Error::other("test credential has no refresh lineage"))?;
    let rendered = format!("{lineage:?}");
    assert_eq!(rendered, "RefreshLineage([REDACTED])");
    assert!(!rendered.contains("refresh-secret"));
    Ok(())
}
