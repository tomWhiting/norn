//! OAuth token revocation and logout.

use serde::Serialize;
use std::future::Future;

use super::CLIENT_ID;
use super::auth_root::NornAuthRoot;
use super::credential_lock_timing::{CredentialLockTiming, CredentialLockTimingError};
use super::credential_transaction::{
    CredentialDocument, CredentialTransaction, CredentialTransactionError,
};
use super::endpoints::REVOKE_URL;
use super::options::OAuthHttpOptions;
use super::storage::{AuthCredentialsStoreMode, DeleteAuthOutcome};

/// Errors from logout/revoke.
#[derive(Debug, thiserror::Error)]
pub enum LogoutError {
    /// HTTP client construction or request failed.
    #[error("token revoke failed: {0}")]
    Revoke(String),
    /// Remote revocation was not started because local removal did not commit.
    #[error("remote token revocation was not attempted because durable local removal failed")]
    LocalRemovalIncomplete,
    /// Credential bytes existed but could not be decoded for revocation.
    #[error("stored OAuth credential is malformed")]
    MalformedCredential,
}

/// Failure to complete the local half of logout.
#[derive(Debug, thiserror::Error)]
pub enum LocalLogoutError {
    /// The intended credential changed before local removal committed.
    #[error("local OAuth credential changed during logout and was not removed")]
    Conflict,
    /// Local removal occurred but directory durability was not confirmed.
    #[error("local OAuth credential removal occurred but durability was not confirmed")]
    Undurable,
    /// Locking, descriptor admission, or private filesystem I/O failed.
    #[error("local OAuth credential removal could not be coordinated")]
    Coordination,
}

/// Remote revocation result, reported separately from local credential removal.
#[derive(Debug)]
pub enum RemoteRevokeOutcome {
    /// No stored refresh token was available to revoke.
    NotApplicable,
    /// The authority accepted the refresh-token revocation request.
    Revoked,
    /// Loading the token, satisfying the local-first precondition, or
    /// contacting the revoke authority failed.
    Failed(LogoutError),
}

/// Complete logout result.
///
/// Local removal is always attempted before remote revocation, including when
/// credential loading fails.
#[must_use = "logout reports contain independent local-removal and remote-revocation outcomes"]
#[derive(Debug)]
pub struct LogoutReport {
    /// Durable local credential removal result.
    pub local: Result<DeleteAuthOutcome, LocalLogoutError>,
    /// Independent remote revocation result.
    pub remote: RemoteRevokeOutcome,
}

#[derive(Serialize)]
struct RevokeRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    client_id: &'a str,
}

enum PendingRemoteRevoke {
    NotApplicable,
    RefreshToken(String),
    LoadFailed(LogoutError),
}

/// Durably removes `auth.json` locally, then reports remote revocation
/// independently for the refresh token captured from that removed credential.
///
/// `http` supplies the whole-request deadline for the revoke exchange
/// (see [`OAuthHttpOptions::request_timeout`]).
///
/// The remote future is not constructed or awaited until local deletion and
/// directory synchronization succeed. Cancellation during remote revocation
/// therefore cannot leave the removed credential installed. A credential
/// written after remote revocation starts is not touched by this logout.
///
pub async fn logout_with_revoke(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    http: OAuthHttpOptions,
) -> LogoutReport {
    let timing = match http.credential_lock_timing() {
        Ok(timing) => timing,
        Err(error) => return invalid_lock_timing_report(error),
    };
    logout_with_revoker(auth_root, mode, timing, move |refresh_token| async move {
        revoke_refresh_token(&refresh_token, http).await
    })
    .await
}

async fn logout_with_revoker<F, Fut>(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    credential_lock_timing: CredentialLockTiming,
    revoke: F,
) -> LogoutReport
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<(), LogoutError>>,
{
    let root = auth_root.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_local_logout(&root, mode, credential_lock_timing)
    })
    .await;
    let (local, pending_remote) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(%error, "local OAuth logout task failed");
            (
                Err(LocalLogoutError::Coordination),
                PendingRemoteRevoke::LoadFailed(LogoutError::LocalRemovalIncomplete),
            )
        }
    };
    complete_logout(local, pending_remote, revoke).await
}

fn prepare_local_logout(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    timing: CredentialLockTiming,
) -> (
    Result<DeleteAuthOutcome, LocalLogoutError>,
    PendingRemoteRevoke,
) {
    match mode {
        AuthCredentialsStoreMode::File => {}
    }
    let transaction = match CredentialTransaction::acquire(auth_root, timing) {
        Ok(transaction) => transaction,
        Err(error) => {
            return (
                Err(map_local_logout_error(&error)),
                PendingRemoteRevoke::LoadFailed(LogoutError::LocalRemovalIncomplete),
            );
        }
    };
    let snapshot = match transaction.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                Err(map_local_logout_error(&error)),
                PendingRemoteRevoke::LoadFailed(LogoutError::LocalRemovalIncomplete),
            );
        }
    };
    let pending_remote = match snapshot.document {
        CredentialDocument::Missing => PendingRemoteRevoke::NotApplicable,
        CredentialDocument::Malformed(_) => {
            PendingRemoteRevoke::LoadFailed(LogoutError::MalformedCredential)
        }
        CredentialDocument::Parsed(auth) => {
            auth.tokens
                .map_or(PendingRemoteRevoke::NotApplicable, |tokens| {
                    if tokens.refresh_token.is_empty() {
                        PendingRemoteRevoke::NotApplicable
                    } else {
                        PendingRemoteRevoke::RefreshToken(tokens.refresh_token)
                    }
                })
        }
    };
    let local = transaction
        .delete_if_revision(snapshot.revision.as_ref())
        .map_err(|error| map_local_logout_error(&error));
    (local, pending_remote)
}

fn invalid_lock_timing_report(error: CredentialLockTimingError) -> LogoutReport {
    tracing::warn!(%error, "local OAuth logout timing configuration was rejected");
    LogoutReport {
        local: Err(LocalLogoutError::Coordination),
        remote: RemoteRevokeOutcome::Failed(LogoutError::LocalRemovalIncomplete),
    }
}

fn map_local_logout_error(error: &CredentialTransactionError) -> LocalLogoutError {
    match error {
        CredentialTransactionError::Conflict
        | CredentialTransactionError::VerificationConflict
        | CredentialTransactionError::RecoveryIncomplete(_) => LocalLogoutError::Conflict,
        CredentialTransactionError::DeletedButUndurable(_) => LocalLogoutError::Undurable,
        CredentialTransactionError::DescriptorAdmission(_)
        | CredentialTransactionError::OpenRoot(_)
        | CredentialTransactionError::OpenLock(_)
        | CredentialTransactionError::LockTimeout { .. }
        | CredentialTransactionError::Lock(_)
        | CredentialTransactionError::Storage(_)
        | CredentialTransactionError::PublishedButUndurable { .. } => {
            LocalLogoutError::Coordination
        }
    }
}

async fn complete_logout<F, Fut>(
    local: Result<DeleteAuthOutcome, LocalLogoutError>,
    pending_remote: PendingRemoteRevoke,
    revoke: F,
) -> LogoutReport
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<(), LogoutError>>,
{
    if local.is_err() {
        let remote = match pending_remote {
            PendingRemoteRevoke::NotApplicable => RemoteRevokeOutcome::NotApplicable,
            PendingRemoteRevoke::RefreshToken(refresh_token) => {
                drop(refresh_token);
                RemoteRevokeOutcome::Failed(LogoutError::LocalRemovalIncomplete)
            }
            PendingRemoteRevoke::LoadFailed(error) => RemoteRevokeOutcome::Failed(error),
        };
        return LogoutReport { local, remote };
    }

    let remote = match pending_remote {
        PendingRemoteRevoke::NotApplicable => RemoteRevokeOutcome::NotApplicable,
        PendingRemoteRevoke::RefreshToken(refresh_token) => match revoke(refresh_token).await {
            Ok(()) => RemoteRevokeOutcome::Revoked,
            Err(error) => RemoteRevokeOutcome::Failed(error),
        },
        PendingRemoteRevoke::LoadFailed(error) => RemoteRevokeOutcome::Failed(error),
    };
    LogoutReport { local, remote }
}

/// Revokes the refresh token at the compiled revoke endpoint.
///
/// The endpoint is deliberately not configurable (no environment
/// override): the request body carries the live refresh token, so an
/// environment-redirectable endpoint would be an exfiltration vector.
async fn revoke_refresh_token(
    refresh_token: &str,
    http: OAuthHttpOptions,
) -> Result<(), LogoutError> {
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| LogoutError::Revoke(error.to_string()))?;
    let _permit = governor
        .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
        .map_err(|error| LogoutError::Revoke(error.to_string()))?;
    let client = crate::provider::http_client::build_bounded_client(http.request_timeout)
        .map_err(|err| LogoutError::Revoke(err.to_string()))?;
    let response = client
        .post(REVOKE_URL)
        .json(&RevokeRequest {
            token: refresh_token,
            token_type_hint: "refresh_token",
            client_id: CLIENT_ID,
        })
        .send()
        .await
        .map_err(|err| LogoutError::Revoke(err.to_string()))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(LogoutError::Revoke(format!(
            "revoke endpoint returned {}",
            response.status()
        )))
    }
}

#[cfg(test)]
#[path = "revoke_tests.rs"]
mod tests;
