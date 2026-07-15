use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::provider::openai_oauth::{AuthDotJson, ChatGptTokens, IdTokenInfo};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn auth_document_with_tokens(access_token: &str, refresh_token: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("account"),
        access_token: access_token.to_owned(),
        refresh_token: refresh_token.to_owned(),
        account_id: Some("account".to_owned()),
        additional_fields: std::collections::BTreeMap::new(),
    })
}

fn auth_document() -> AuthDotJson {
    auth_document_with_tokens("access-token", "refresh-token")
}

fn require_auth(value: Option<AuthDotJson>) -> Result<AuthDotJson, std::io::Error> {
    value.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "auth.json missing"))
}

fn auth_root(home: &tempfile::TempDir) -> Result<NornAuthRoot, super::super::NornAuthRootError> {
    NornAuthRoot::try_from(home.path())
}

#[tokio::test]
async fn invalid_lock_timing_fails_before_logout_credential_access() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cases = [
        OAuthHttpOptions {
            credential_lock_timeout: std::time::Duration::ZERO,
            ..OAuthHttpOptions::default()
        },
        OAuthHttpOptions {
            credential_lock_poll_interval: std::time::Duration::ZERO,
            ..OAuthHttpOptions::default()
        },
    ];

    for (index, http) in cases.into_iter().enumerate() {
        let root_path = directory.path().join(format!("invalid-{index}"));
        let root = NornAuthRoot::try_from(root_path.as_path())?;
        let report = logout_with_revoke(&root, AuthCredentialsStoreMode::File, http).await;

        assert!(matches!(report.local, Err(LocalLogoutError::Coordination)));
        assert!(matches!(
            report.remote,
            RemoteRevokeOutcome::Failed(LogoutError::LocalRemovalIncomplete)
        ));
        assert!(!root_path.exists());
    }
    Ok(())
}

#[tokio::test]
async fn revoke_failure_still_removes_local_credential() -> TestResult {
    let home = tempfile::tempdir()?;
    let root = auth_root(&home)?;
    super::super::storage::save_auth_dot_json(home.path(), &auth_document())?;

    let report = logout_with_revoker(
        &root,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default().credential_lock_timing()?,
        |refresh_token| async move {
            drop(refresh_token);
            Err(LogoutError::Revoke("authority unavailable".to_owned()))
        },
    )
    .await;

    let rendered = format!("{report:?}");
    assert!(!rendered.contains("refresh-token"));
    assert!(matches!(report.local, Ok(DeleteAuthOutcome::Removed)));
    let RemoteRevokeOutcome::Failed(LogoutError::Revoke(reason)) = report.remote else {
        return Err(std::io::Error::other("revoke failure classification was lost").into());
    };
    assert_eq!(reason, "authority unavailable");
    assert!(!super::super::storage::auth_json_path(home.path()).exists());
    Ok(())
}

#[tokio::test]
async fn access_only_logout_does_not_attempt_remote_revoke() -> TestResult {
    let home = tempfile::tempdir()?;
    let root = auth_root(&home)?;
    let access_only = auth_document_with_tokens("access-token", "");
    super::super::storage::save_auth_dot_json(home.path(), &access_only)?;
    let revoke_started = Arc::new(AtomicBool::new(false));
    let revoke_started_in_callback = Arc::clone(&revoke_started);

    let report = logout_with_revoker(
        &root,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default().credential_lock_timing()?,
        move |refresh_token| async move {
            drop(refresh_token);
            revoke_started_in_callback.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .await;

    assert!(matches!(report.local, Ok(DeleteAuthOutcome::Removed)));
    assert!(matches!(report.remote, RemoteRevokeOutcome::NotApplicable));
    assert!(!revoke_started.load(Ordering::SeqCst));
    assert!(!super::super::storage::auth_json_path(home.path()).exists());
    Ok(())
}

#[tokio::test]
async fn malformed_credential_is_removed_when_revoke_cannot_start() -> TestResult {
    let home = tempfile::tempdir()?;
    let root = auth_root(&home)?;
    std::fs::write(
        super::super::storage::auth_json_path(home.path()),
        b"{malformed",
    )?;

    let report = logout_with_revoker(
        &root,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default().credential_lock_timing()?,
        |refresh_token: String| async move {
            drop(refresh_token);
            Ok(())
        },
    )
    .await;

    assert!(matches!(report.local, Ok(DeleteAuthOutcome::Removed)));
    let RemoteRevokeOutcome::Failed(LogoutError::MalformedCredential) = report.remote else {
        return Err(std::io::Error::other("storage failure classification was lost").into());
    };
    assert!(!super::super::storage::auth_json_path(home.path()).exists());
    Ok(())
}

#[tokio::test]
async fn cancellation_during_remote_revoke_cannot_restore_local_credential() -> TestResult {
    let home = tempfile::tempdir()?;
    let root = auth_root(&home)?;
    super::super::storage::save_auth_dot_json(home.path(), &auth_document())?;
    let credential_path = super::super::storage::auth_json_path(home.path());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();

    {
        let logout = logout_with_revoker(
            &root,
            AuthCredentialsStoreMode::File,
            OAuthHttpOptions::default().credential_lock_timing()?,
            move |refresh_token| async move {
                if refresh_token != "refresh-token" {
                    return Err(LogoutError::Revoke(
                        "logout captured the wrong refresh credential".to_owned(),
                    ));
                }
                entered_tx.send(()).map_err(|()| {
                    LogoutError::Revoke("logout cancellation observer was unavailable".to_owned())
                })?;
                std::future::pending::<Result<(), LogoutError>>().await
            },
        );
        tokio::pin!(logout);
        tokio::select! {
            entered = entered_rx => {
                entered.map_err(|error| std::io::Error::other(error.to_string()))?;
            }
            report = &mut logout => {
                return Err(std::io::Error::other(format!(
                    "remote revoke returned instead of remaining cancellable: {report:?}"
                )).into());
            }
        }
        assert!(
            !credential_path.exists(),
            "local removal must commit before the remote future is polled"
        );
    }

    assert!(
        !credential_path.exists(),
        "dropping the in-flight remote revoke must not reinstall credentials"
    );
    Ok(())
}

#[tokio::test]
async fn replacement_written_during_remote_revoke_survives_old_logout() -> TestResult {
    let home = tempfile::tempdir()?;
    let root = auth_root(&home)?;
    super::super::storage::save_auth_dot_json(home.path(), &auth_document())?;
    let replacement = auth_document_with_tokens("replacement-access", "replacement-refresh");
    let replacement_for_revoke = replacement.clone();
    let home_path = home.path().to_path_buf();

    let report = logout_with_revoker(
        &root,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default().credential_lock_timing()?,
        move |refresh_token| async move {
            if refresh_token != "refresh-token" {
                return Err(LogoutError::Revoke(
                    "logout captured the wrong refresh credential".to_owned(),
                ));
            }
            super::super::storage::save_auth_dot_json(&home_path, &replacement_for_revoke)
                .map_err(|error| LogoutError::Revoke(error.to_string()))?;
            Ok(())
        },
    )
    .await;

    assert!(matches!(report.local, Ok(DeleteAuthOutcome::Removed)));
    assert!(matches!(report.remote, RemoteRevokeOutcome::Revoked));
    let loaded = require_auth(super::super::storage::load_auth_dot_json(
        &root,
        AuthCredentialsStoreMode::File,
    )?)?;
    assert_eq!(loaded, replacement);
    Ok(())
}

#[tokio::test]
async fn local_removal_failure_never_starts_remote_revoke() -> TestResult {
    let revoke_started = Arc::new(AtomicBool::new(false));
    let revoke_started_in_callback = Arc::clone(&revoke_started);
    let local = Err(LocalLogoutError::Coordination);

    let report = complete_logout(
        local,
        PendingRemoteRevoke::RefreshToken("refresh-token".to_owned()),
        move |refresh_token| async move {
            drop(refresh_token);
            revoke_started_in_callback.store(true, Ordering::SeqCst);
            Err(LogoutError::Revoke(
                "unexpected remote revoke after failed local removal".to_owned(),
            ))
        },
    )
    .await;

    assert!(report.local.is_err());
    assert!(matches!(
        report.remote,
        RemoteRevokeOutcome::Failed(LogoutError::LocalRemovalIncomplete)
    ));
    assert!(!revoke_started.load(Ordering::SeqCst));
    Ok(())
}
