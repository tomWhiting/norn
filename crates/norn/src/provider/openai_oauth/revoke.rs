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
    /// Credential removal committed, but exact catalog retirement did not.
    #[error("local OAuth credential was removed but account catalog retirement was not durable")]
    CatalogRetirement,
    /// Credential and catalog removal committed, but the empty slot remained.
    #[error("local OAuth credential was removed but account slot cleanup failed")]
    SlotCleanup,
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

pub(crate) struct PreparedLogout {
    local: Result<DeleteAuthOutcome, LocalLogoutError>,
    pending_remote: PendingRemoteRevoke,
    remote_permitted: bool,
}

impl PreparedLogout {
    pub(crate) fn coordination_failure() -> Self {
        Self {
            local: Err(LocalLogoutError::Coordination),
            pending_remote: PendingRemoteRevoke::LoadFailed(LogoutError::LocalRemovalIncomplete),
            remote_permitted: false,
        }
    }

    pub(crate) fn local_succeeded(&self) -> bool {
        self.local.is_ok()
    }

    pub(crate) fn catalog_retirement_failed(&mut self) {
        self.local = Err(LocalLogoutError::CatalogRetirement);
        self.remote_permitted = false;
    }

    pub(crate) fn slot_cleanup_failed(&mut self) {
        self.local = Err(LocalLogoutError::SlotCleanup);
    }
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
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(%error, "local OAuth logout task failed");
            PreparedLogout {
                local: Err(LocalLogoutError::Coordination),
                pending_remote: PendingRemoteRevoke::LoadFailed(
                    LogoutError::LocalRemovalIncomplete,
                ),
                remote_permitted: false,
            }
        }
    };
    complete_prepared_with_revoker(prepared, revoke).await
}

pub(crate) fn prepare_local_logout(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    timing: CredentialLockTiming,
) -> PreparedLogout {
    match mode {
        AuthCredentialsStoreMode::File => {}
    }
    let transaction = match CredentialTransaction::acquire(auth_root, timing) {
        Ok(transaction) => transaction,
        Err(error) => {
            return PreparedLogout {
                local: Err(map_local_logout_error(&error)),
                pending_remote: PendingRemoteRevoke::LoadFailed(
                    LogoutError::LocalRemovalIncomplete,
                ),
                remote_permitted: false,
            };
        }
    };
    let snapshot = match transaction.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return PreparedLogout {
                local: Err(map_local_logout_error(&error)),
                pending_remote: PendingRemoteRevoke::LoadFailed(
                    LogoutError::LocalRemovalIncomplete,
                ),
                remote_permitted: false,
            };
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
    let recovery_revision = transaction.recovery_revision_for_logout();
    let local = transaction
        .delete_if_revision(snapshot.revision.as_ref())
        .map_err(|error| map_local_logout_error(&error));
    if local.is_ok() {
        match recovery_revision {
            Ok(expected) => {
                if let Err(error) = transaction.clear_recovery_after_logout(expected.as_ref()) {
                    tracing::warn!(%error, "local OAuth logout could not clear recovery state");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "local OAuth logout could not inspect recovery state");
            }
        }
    }
    let remote_permitted = local.is_ok();
    PreparedLogout {
        local,
        pending_remote,
        remote_permitted,
    }
}

pub(crate) async fn complete_prepared_logout(
    prepared: PreparedLogout,
    http: OAuthHttpOptions,
) -> LogoutReport {
    complete_prepared_with_revoker(prepared, move |refresh_token| async move {
        revoke_refresh_token(&refresh_token, http).await
    })
    .await
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

#[cfg(test)]
async fn complete_logout<F, Fut>(
    local: Result<DeleteAuthOutcome, LocalLogoutError>,
    pending_remote: PendingRemoteRevoke,
    revoke: F,
) -> LogoutReport
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<(), LogoutError>>,
{
    let remote_permitted = local.is_ok();
    complete_prepared_with_revoker(
        PreparedLogout {
            local,
            pending_remote,
            remote_permitted,
        },
        revoke,
    )
    .await
}

async fn complete_prepared_with_revoker<F, Fut>(prepared: PreparedLogout, revoke: F) -> LogoutReport
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<(), LogoutError>>,
{
    let PreparedLogout {
        local,
        pending_remote,
        remote_permitted,
    } = prepared;
    if !remote_permitted {
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
