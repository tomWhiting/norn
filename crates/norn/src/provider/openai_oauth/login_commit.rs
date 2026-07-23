//! Shared durable publication boundary for interactive OAuth login.

use super::auth_root::NornAuthRoot;
use super::credential_lock_timing::CredentialLockTiming;
use super::credential_transaction::{
    CredentialRevision, CredentialTransaction, CredentialTransactionError,
};
use super::login_server::{LoginError, LoginStorageFailureKind};
use super::storage::AuthCredentialsStoreMode;
use super::types::AuthDotJson;

pub(super) fn inspect_login_revision(
    auth_root: &NornAuthRoot,
) -> Result<Option<CredentialRevision>, LoginError> {
    CredentialTransaction::inspect(auth_root)
        .map(|snapshot| snapshot.revision)
        .map_err(map_credential_transaction_error)
}

pub(super) async fn inspect_login_revision_async(
    auth_root: NornAuthRoot,
) -> Result<Option<CredentialRevision>, LoginError> {
    tokio::task::spawn_blocking(move || inspect_login_revision(&auth_root))
        .await
        .map_err(|error| LoginError::Storage {
            kind: LoginStorageFailureKind::Coordination,
            reason: format!("credential inspection task failed: {error}"),
        })?
}

pub(crate) async fn persist_prepared_login<V, F>(
    auth_root: NornAuthRoot,
    expected_revision: Option<CredentialRevision>,
    mode: AuthCredentialsStoreMode,
    credential_lock_timing: CredentialLockTiming,
    auth: AuthDotJson,
    validate: V,
    commit: F,
) -> Result<(), LoginError>
where
    V: FnOnce(&AuthDotJson) -> Result<(), LoginError> + Send + 'static,
    F: FnOnce() -> Result<(), LoginError> + Send + 'static,
{
    let acquired = tokio::task::spawn_blocking(move || {
        CredentialTransaction::acquire(&auth_root, credential_lock_timing)
            .map(|transaction| (transaction, auth, validate, commit))
    })
    .await
    .map_err(|error| LoginError::Storage {
        kind: LoginStorageFailureKind::Coordination,
        reason: format!("credential transaction task failed: {error}"),
    })?
    .map_err(map_credential_transaction_error)?;
    let (transaction, auth, validate, commit) = acquired;

    validate(&auth)?;
    // These synchronous operations run in one future poll, so cancellation
    // cannot split the durable save from caller-owned publication.
    match mode {
        AuthCredentialsStoreMode::File => transaction
            .save_if_revision(expected_revision.as_ref(), &auth)
            .map(drop)
            .map_err(map_credential_transaction_error),
    }
    .and_then(|()| commit())
}

pub(super) fn map_credential_transaction_error(error: CredentialTransactionError) -> LoginError {
    let reason = error.to_string();
    let kind = match error {
        CredentialTransactionError::DescriptorAdmission(error) => {
            return LoginError::DescriptorAdmission(error);
        }
        CredentialTransactionError::Conflict
        | CredentialTransactionError::VerificationConflict
        | CredentialTransactionError::RecoveryIncomplete(_) => LoginStorageFailureKind::Conflict,
        CredentialTransactionError::PublishedButUndurable { .. }
        | CredentialTransactionError::DeletedButUndurable(_) => LoginStorageFailureKind::Undurable,
        CredentialTransactionError::OpenRoot(_)
        | CredentialTransactionError::OpenLock(_)
        | CredentialTransactionError::LockTimeout { .. }
        | CredentialTransactionError::Lock(_)
        | CredentialTransactionError::Storage(_) => LoginStorageFailureKind::Coordination,
    };
    LoginError::Storage { kind, reason }
}

#[cfg(test)]
#[path = "login_commit_claim_tests.rs"]
mod claim_tests;
