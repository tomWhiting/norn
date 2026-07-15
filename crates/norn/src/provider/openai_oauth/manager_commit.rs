//! Durable refresh dispatch and credential-commit protocol.

use super::{AccountIdentity, AuthManager, CachedAuthState, RefreshLineage, RefreshTokenError};
use crate::provider::openai_oauth::credential_recovery::{
    RecoveryJournalError, RefreshRecoveryOperation,
};
use crate::provider::openai_oauth::credential_revision::{CredentialRevision, serialize_auth};
use crate::provider::openai_oauth::credential_transaction::{
    CredentialSnapshot, CredentialTransaction, CredentialTransactionError,
};
use crate::provider::openai_oauth::refresh::{RefreshError, refresh_auth};
use crate::provider::openai_oauth::types::{AuthDotJson, CodexAuth};

impl AuthManager {
    pub(super) async fn refresh_and_persist(
        &self,
        transaction: &CredentialTransaction,
        current: Box<AuthDotJson>,
        expected_revision: Option<CredentialRevision>,
    ) -> Result<(), RefreshTokenError> {
        self.validate_refresh_owner(&current)?;
        let expected = expected_revision.as_ref().ok_or_else(|| {
            RefreshTokenError::Coordination(
                "file-backed OAuth credential has no durable revision".to_owned(),
            )
        })?;
        let mut operation = transaction
            .begin_refresh_recovery(expected, &current)
            .map_err(|error| map_pre_dispatch_recovery_error(&error))?;
        let observed_lineage = refresh_lineage(&current)?;
        *self.auth.lock().await = CachedAuthState::Indeterminate {
            observed_revision: expected_revision.clone(),
            observed_lineage: observed_lineage.clone(),
            error: refresh_incomplete_error(),
        };

        let refreshed = match refresh_auth(&current, &self.token_url, &self.client).await {
            Ok(refreshed) => Box::new(refreshed),
            Err(RefreshError::Indeterminate(message)) => {
                let error = RefreshTokenError::Indeterminate(message);
                *self.auth.lock().await = CachedAuthState::Indeterminate {
                    observed_revision: expected_revision,
                    observed_lineage,
                    error: error.clone(),
                };
                return Err(error);
            }
            Err(error) => {
                let refresh_error = map_refresh_error(error);
                *self.auth.lock().await = ready_file(current, expected_revision);
                if let Err(error) = transaction
                    .mark_refresh_resolved_no_rotation(&mut operation)
                    .and_then(|()| transaction.finish_refresh_recovery(&operation))
                {
                    return Err(map_no_rotation_cleanup_error(&error));
                }
                return Err(refresh_error);
            }
        };

        let (_, proposed_revision) =
            serialize_auth(&refreshed).map_err(map_staging_serialization_error)?;
        if let Err(error) =
            transaction.mark_refresh_commit_pending(&mut operation, &proposed_revision)
        {
            let refresh_error = map_post_dispatch_recovery_error(&error);
            return self
                .retain_pending(refreshed, expected_revision, refresh_error)
                .await;
        }
        self.commit_refreshed(transaction, refreshed, expected_revision, Some(operation))
            .await
    }

    pub(super) async fn persist_pending(
        &self,
        transaction: &CredentialTransaction,
        snapshot: &CredentialSnapshot,
        refreshed: Box<AuthDotJson>,
        expected_revision: Option<CredentialRevision>,
    ) -> Result<(), RefreshTokenError> {
        self.validate_refresh_owner(&refreshed)?;
        let Some(expected) = expected_revision.as_ref() else {
            let error = RefreshTokenError::Undurable(
                "pending OAuth credential has no expected durable revision".to_owned(),
            );
            return self
                .retain_pending(refreshed, expected_revision, error)
                .await;
        };
        let operation =
            match transaction.prepare_pending_refresh_recovery(snapshot, expected, &refreshed) {
                Ok(operation) => operation,
                Err(error) => {
                    let refresh_error = map_pending_recovery_error(&error);
                    return self
                        .retain_pending(refreshed, expected_revision, refresh_error)
                        .await;
                }
            };
        self.commit_refreshed(transaction, refreshed, expected_revision, operation)
            .await
    }

    async fn commit_refreshed(
        &self,
        transaction: &CredentialTransaction,
        refreshed: Box<AuthDotJson>,
        expected_revision: Option<CredentialRevision>,
        operation: Option<RefreshRecoveryOperation>,
    ) -> Result<(), RefreshTokenError> {
        match transaction.save_if_revision(expected_revision.as_ref(), &refreshed) {
            Ok(revision) => {
                if let Some(operation) = operation
                    && let Err(error) = transaction.finish_refresh_recovery(&operation)
                {
                    let refresh_error = map_post_commit_cleanup_error(&error);
                    return self
                        .retain_pending(refreshed, expected_revision, refresh_error)
                        .await;
                }
                *self.auth.lock().await = CachedAuthState::Ready {
                    auth: CodexAuth::ChatGpt(refreshed),
                    revision: Some(revision),
                };
                Ok(())
            }
            Err(error) => {
                let refresh_error = map_post_refresh_commit_error(&error);
                self.retain_pending(refreshed, expected_revision, refresh_error)
                    .await
            }
        }
    }

    async fn retain_pending(
        &self,
        refreshed: Box<AuthDotJson>,
        expected_revision: Option<CredentialRevision>,
        error: RefreshTokenError,
    ) -> Result<(), RefreshTokenError> {
        *self.auth.lock().await = CachedAuthState::PendingPersistence {
            refreshed,
            expected_revision,
            error: error.clone(),
        };
        Err(error)
    }

    fn validate_refresh_owner(&self, auth: &AuthDotJson) -> Result<(), RefreshTokenError> {
        let proposed_identity = account_identity(auth)?;
        if self.account_identity.as_ref() != Some(&proposed_identity) {
            return Err(RefreshTokenError::Conflict(
                "rotated OAuth credential no longer matches its manager".to_owned(),
            ));
        }
        Ok(())
    }
}

fn ready_file(auth: Box<AuthDotJson>, revision: Option<CredentialRevision>) -> CachedAuthState {
    CachedAuthState::Ready {
        auth: CodexAuth::ChatGpt(auth),
        revision,
    }
}

fn account_identity(auth: &AuthDotJson) -> Result<AccountIdentity, RefreshTokenError> {
    AccountIdentity::from_auth(auth).ok_or_else(|| {
        RefreshTokenError::Permanent("OAuth credential has no account identity".to_owned())
    })
}

fn refresh_lineage(auth: &AuthDotJson) -> Result<RefreshLineage, RefreshTokenError> {
    RefreshLineage::from_auth(auth).ok_or_else(|| {
        RefreshTokenError::Permanent("OAuth credential has no refresh-token lineage".to_owned())
    })
}

fn refresh_incomplete_error() -> RefreshTokenError {
    RefreshTokenError::Indeterminate(
        "OAuth refresh ended before its authority outcome was recorded".to_owned(),
    )
}

fn map_refresh_error(error: RefreshError) -> RefreshTokenError {
    match error {
        RefreshError::Transient(message) => RefreshTokenError::Transient(message),
        RefreshError::Permanent(message) => RefreshTokenError::Permanent(message),
        RefreshError::Indeterminate(message) => RefreshTokenError::Indeterminate(message),
    }
}

fn map_pre_dispatch_recovery_error(error: &RecoveryJournalError) -> RefreshTokenError {
    match error {
        RecoveryJournalError::Changed => RefreshTokenError::Conflict(
            "OAuth refresh recovery state changed before authority dispatch".to_owned(),
        ),
        RecoveryJournalError::Invalid => RefreshTokenError::Indeterminate(
            "OAuth refresh recovery is required before authority dispatch".to_owned(),
        ),
        RecoveryJournalError::EntropyUnavailable
        | RecoveryJournalError::Serialization(_)
        | RecoveryJournalError::Io(_)
        | RecoveryJournalError::PublishedButUndurable(_)
        | RecoveryJournalError::DeletedButUndurable(_) => RefreshTokenError::Coordination(
            "OAuth refresh recovery barrier could not be durably prepared".to_owned(),
        ),
    }
}

fn map_post_dispatch_recovery_error(error: &RecoveryJournalError) -> RefreshTokenError {
    match error {
        RecoveryJournalError::Changed | RecoveryJournalError::Invalid => {
            RefreshTokenError::Conflict(
                "OAuth refresh recovery state changed after authority dispatch".to_owned(),
            )
        }
        RecoveryJournalError::EntropyUnavailable
        | RecoveryJournalError::Serialization(_)
        | RecoveryJournalError::Io(_)
        | RecoveryJournalError::PublishedButUndurable(_)
        | RecoveryJournalError::DeletedButUndurable(_) => RefreshTokenError::Undurable(
            "rotated OAuth credential could not be durably staged".to_owned(),
        ),
    }
}

fn map_staging_serialization_error(_error: serde_json::Error) -> RefreshTokenError {
    RefreshTokenError::Indeterminate(
        "rotated OAuth credential could not be staged for durable storage".to_owned(),
    )
}

fn map_pending_recovery_error(error: &RecoveryJournalError) -> RefreshTokenError {
    match error {
        RecoveryJournalError::Changed => RefreshTokenError::Conflict(
            "OAuth refresh recovery state changed before credential commit".to_owned(),
        ),
        RecoveryJournalError::Invalid => RefreshTokenError::Conflict(
            "pending OAuth credential recovery state is invalid".to_owned(),
        ),
        RecoveryJournalError::EntropyUnavailable
        | RecoveryJournalError::Serialization(_)
        | RecoveryJournalError::Io(_)
        | RecoveryJournalError::PublishedButUndurable(_)
        | RecoveryJournalError::DeletedButUndurable(_) => RefreshTokenError::Undurable(
            "pending OAuth credential recovery could not be durably coordinated".to_owned(),
        ),
    }
}

fn map_no_rotation_cleanup_error(error: &RecoveryJournalError) -> RefreshTokenError {
    match error {
        RecoveryJournalError::Changed | RecoveryJournalError::Invalid => {
            RefreshTokenError::Conflict(
                "OAuth refresh recovery state changed during no-rotation cleanup".to_owned(),
            )
        }
        RecoveryJournalError::EntropyUnavailable
        | RecoveryJournalError::Serialization(_)
        | RecoveryJournalError::Io(_)
        | RecoveryJournalError::PublishedButUndurable(_)
        | RecoveryJournalError::DeletedButUndurable(_) => RefreshTokenError::Coordination(
            "OAuth refresh recovery cleanup was not durably confirmed".to_owned(),
        ),
    }
}

fn map_post_commit_cleanup_error(error: &RecoveryJournalError) -> RefreshTokenError {
    match error {
        RecoveryJournalError::Changed | RecoveryJournalError::Invalid => {
            RefreshTokenError::Conflict(
                "OAuth refresh recovery state changed after credential commit".to_owned(),
            )
        }
        RecoveryJournalError::EntropyUnavailable
        | RecoveryJournalError::Serialization(_)
        | RecoveryJournalError::Io(_)
        | RecoveryJournalError::PublishedButUndurable(_)
        | RecoveryJournalError::DeletedButUndurable(_) => RefreshTokenError::Undurable(
            "rotated OAuth credential was saved, but recovery cleanup was not durably confirmed"
                .to_owned(),
        ),
    }
}

fn map_post_refresh_commit_error(error: &CredentialTransactionError) -> RefreshTokenError {
    match error {
        CredentialTransactionError::Conflict
        | CredentialTransactionError::VerificationConflict
        | CredentialTransactionError::RecoveryIncomplete(_) => RefreshTokenError::Conflict(
            "rotated OAuth credential conflicted with another writer".to_owned(),
        ),
        CredentialTransactionError::DescriptorAdmission(_)
        | CredentialTransactionError::OpenRoot(_)
        | CredentialTransactionError::OpenLock(_)
        | CredentialTransactionError::LockTimeout { .. }
        | CredentialTransactionError::Lock(_)
        | CredentialTransactionError::Storage(_)
        | CredentialTransactionError::PublishedButUndurable { .. }
        | CredentialTransactionError::DeletedButUndurable(_) => RefreshTokenError::Undurable(
            format!("rotated OAuth credential was not durably accepted: {error}"),
        ),
    }
}
