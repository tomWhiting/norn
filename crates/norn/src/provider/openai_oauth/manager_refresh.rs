//! File-transaction and single-flight refresh behavior for [`AuthManager`].

use std::sync::Arc;

use super::attempt::supervise_refresh_worker;
use super::{
    AccountIdentity, AuthManager, CachedAuthState, RefreshAttempt, RefreshLineage,
    RefreshTokenError,
};
use crate::provider::openai_oauth::credential_recovery::{
    RecoveryJournalError, RecoveryReconciliation,
};
use crate::provider::openai_oauth::credential_state::{
    LocalCredentialState, evaluate_chatgpt_credential,
};
use crate::provider::openai_oauth::credential_transaction::{
    CredentialDocument, CredentialSnapshot, CredentialTransaction, CredentialTransactionError,
};
use crate::provider::openai_oauth::types::{AuthDotJson, CodexAuth};

impl AuthManager {
    /// Return cached credentials after reconciling file-backed state and
    /// proactively refreshing a known-expired access token.
    pub async fn auth(self: &Arc<Self>) -> Result<Option<CodexAuth>, RefreshTokenError> {
        self.join_registered_attempt().await?;
        if self.auth_root.is_some() {
            self.synchronize_file_state().await?;
        }

        let current = self.auth.lock().await.clone();
        let needs_attempt = match &current {
            CachedAuthState::Missing
            | CachedAuthState::Ready {
                auth: CodexAuth::ApiKey(_),
                ..
            } => false,
            CachedAuthState::Ready {
                auth: CodexAuth::ChatGpt(auth),
                ..
            } => match evaluate_chatgpt_credential(auth, chrono::Utc::now()) {
                LocalCredentialState::RefreshCandidate { .. } => true,
                LocalCredentialState::LocallyValid { .. }
                | LocalCredentialState::Unknown { .. } => false,
                LocalCredentialState::Missing => {
                    return Err(RefreshTokenError::Permanent(
                        "OAuth credential is missing".to_owned(),
                    ));
                }
                LocalCredentialState::Malformed { .. } => {
                    return Err(RefreshTokenError::Permanent(
                        "OAuth credential is malformed".to_owned(),
                    ));
                }
                LocalCredentialState::AccessExpired { .. } => {
                    return Err(RefreshTokenError::Permanent(
                        "OAuth access token expired without a usable refresh token".to_owned(),
                    ));
                }
            },
            CachedAuthState::PendingPersistence { .. } | CachedAuthState::Indeterminate { .. } => {
                true
            }
        };
        if needs_attempt {
            self.refresh_token_from_authority().await?;
        }
        match self.auth.lock().await.clone() {
            CachedAuthState::Missing => Ok(None),
            CachedAuthState::Ready {
                auth: CodexAuth::ApiKey(key),
                ..
            } => Ok(Some(CodexAuth::ApiKey(key))),
            CachedAuthState::Ready {
                auth: CodexAuth::ChatGpt(auth),
                ..
            } => match evaluate_chatgpt_credential(&auth, chrono::Utc::now()) {
                LocalCredentialState::LocallyValid { .. }
                | LocalCredentialState::Unknown { .. } => Ok(Some(CodexAuth::ChatGpt(auth))),
                LocalCredentialState::RefreshCandidate { .. } => Err(RefreshTokenError::Transient(
                    "OAuth credential still requires refresh after reconciliation".to_owned(),
                )),
                LocalCredentialState::AccessExpired { .. } => Err(RefreshTokenError::Permanent(
                    "OAuth access token expired without a usable refresh token".to_owned(),
                )),
                LocalCredentialState::Malformed { .. } => Err(RefreshTokenError::Permanent(
                    "OAuth credential is malformed".to_owned(),
                )),
                LocalCredentialState::Missing => Err(RefreshTokenError::Permanent(
                    "OAuth credential is missing".to_owned(),
                )),
            },
            CachedAuthState::PendingPersistence { error, .. }
            | CachedAuthState::Indeterminate { error, .. } => Err(error),
        }
    }

    /// Refresh through one owned attempt shared by every local waiter.
    pub async fn refresh_token_from_authority(self: &Arc<Self>) -> Result<(), RefreshTokenError> {
        let (attempt, start) = {
            let mut registered = self.refresh_attempt.lock().await;
            if let Some(attempt) = registered.as_ref().filter(|attempt| !attempt.is_terminal()) {
                (Arc::clone(attempt), None)
            } else {
                let (attempt, completion) = RefreshAttempt::new();
                *registered = Some(Arc::clone(&attempt));
                (attempt, Some(completion))
            }
        };

        if let Some(completion) = start {
            let worker_manager = Arc::clone(self);
            let worker =
                tokio::spawn(async move { worker_manager.perform_refresh_attempt().await });
            let supervisor_manager = Arc::clone(self);
            let supervised_attempt = Arc::clone(&attempt);
            std::mem::drop(tokio::spawn(async move {
                supervise_refresh_worker(worker, completion).await;
                supervisor_manager
                    .clear_attempt_if_current(&supervised_attempt)
                    .await;
            }));
        }

        let result = attempt.wait().await;
        self.clear_attempt_if_current(&attempt).await;
        result
    }

    async fn synchronize_file_state(&self) -> Result<(), RefreshTokenError> {
        let transaction = self.acquire_transaction().await?;
        let snapshot = transaction
            .snapshot()
            .map_err(|error| map_transaction_coordination(&error))?;
        let mut current = self.auth.lock().await;
        let observed = current.clone();
        match observed {
            CachedAuthState::PendingPersistence { .. }
            | CachedAuthState::Indeterminate { .. }
            | CachedAuthState::Ready {
                auth: CodexAuth::ApiKey(_),
                ..
            } => Ok(()),
            CachedAuthState::Ready { revision, .. } if revision == snapshot.revision => Ok(()),
            CachedAuthState::Missing | CachedAuthState::Ready { .. } => {
                reconcile_changed_snapshot(&mut current, self.account_identity.as_ref(), snapshot)
            }
        }
    }

    pub(super) async fn perform_refresh_attempt(&self) -> Result<(), RefreshTokenError> {
        if self.auth_root.is_none() {
            return Err(RefreshTokenError::Permanent(
                "OAuth refresh requires a file-backed credential owner".to_owned(),
            ));
        }
        let transaction = self.acquire_transaction().await?;
        let snapshot = transaction
            .snapshot()
            .map_err(|error| map_transaction_coordination(&error))?;
        let current = self.auth.lock().await.clone();
        if let CachedAuthState::PendingPersistence {
            refreshed,
            expected_revision,
            ..
        } = current
        {
            return self
                .persist_pending(&transaction, &snapshot, refreshed, expected_revision)
                .await;
        }
        if self
            .reconcile_recovery_before_attempt(&transaction, &snapshot)
            .await?
        {
            return Ok(());
        }
        let current = self.auth.lock().await.clone();
        match current {
            CachedAuthState::PendingPersistence {
                refreshed,
                expected_revision,
                ..
            } => {
                self.persist_pending(&transaction, &snapshot, refreshed, expected_revision)
                    .await
            }
            CachedAuthState::Indeterminate {
                observed_revision,
                observed_lineage,
                error,
            } => {
                self.recover_indeterminate(snapshot, observed_revision, observed_lineage, error)
                    .await
            }
            CachedAuthState::Ready {
                auth: CodexAuth::ChatGpt(_),
                revision,
            } => {
                if snapshot.revision != revision {
                    let mut current = self.auth.lock().await;
                    return reconcile_changed_snapshot(
                        &mut current,
                        self.account_identity.as_ref(),
                        snapshot,
                    );
                }
                let CredentialDocument::Parsed(disk_auth) = snapshot.document else {
                    return Err(RefreshTokenError::Conflict(
                        "OAuth credential changed before refresh".to_owned(),
                    ));
                };
                self.refresh_and_persist(&transaction, disk_auth, revision)
                    .await
            }
            CachedAuthState::Missing => {
                if matches!(&snapshot.document, CredentialDocument::Parsed(_)) {
                    let mut current = self.auth.lock().await;
                    reconcile_changed_snapshot(
                        &mut current,
                        self.account_identity.as_ref(),
                        snapshot,
                    )
                } else {
                    Err(RefreshTokenError::Permanent(
                        "no refreshable OAuth credential".to_owned(),
                    ))
                }
            }
            CachedAuthState::Ready {
                auth: CodexAuth::ApiKey(_),
                ..
            } => Err(RefreshTokenError::Permanent(
                "no refreshable OAuth credential".to_owned(),
            )),
        }
    }

    async fn recover_indeterminate(
        &self,
        snapshot: CredentialSnapshot,
        observed_revision: Option<
            crate::provider::openai_oauth::credential_transaction::CredentialRevision,
        >,
        observed_lineage: RefreshLineage,
        error: RefreshTokenError,
    ) -> Result<(), RefreshTokenError> {
        if snapshot.revision == observed_revision {
            return Err(error);
        }
        let CredentialDocument::Parsed(auth) = snapshot.document else {
            return Err(RefreshTokenError::Conflict(
                "OAuth credential did not recover after an indeterminate rotation".to_owned(),
            ));
        };
        let pinned_identity = self.account_identity.as_ref().ok_or_else(|| {
            RefreshTokenError::Permanent("OAuth credential has no pinned account owner".to_owned())
        })?;
        if &account_identity(&auth)? != pinned_identity {
            return Err(RefreshTokenError::Conflict(
                "OAuth account identity changed after an indeterminate rotation".to_owned(),
            ));
        }
        if refresh_lineage(&auth)? == observed_lineage {
            return Err(error);
        }
        *self.auth.lock().await = ready_file(auth, snapshot.revision);
        Ok(())
    }

    async fn acquire_transaction(&self) -> Result<CredentialTransaction, RefreshTokenError> {
        let root = self.auth_root.clone().ok_or_else(|| {
            RefreshTokenError::Permanent("credential has no file-backed owner".to_owned())
        })?;
        let timing = self
            .http
            .credential_lock_timing()
            .map_err(|error| RefreshTokenError::Coordination(error.to_string()))?;
        tokio::task::spawn_blocking(move || CredentialTransaction::acquire(&root, timing))
            .await
            .map_err(|error| {
                RefreshTokenError::Coordination(format!(
                    "credential transaction task failed: {error}"
                ))
            })?
            .map_err(|error| map_transaction_coordination(&error))
    }

    async fn reconcile_recovery_before_attempt(
        &self,
        transaction: &CredentialTransaction,
        snapshot: &CredentialSnapshot,
    ) -> Result<bool, RefreshTokenError> {
        let reconciliation = transaction
            .reconcile_refresh_recovery(snapshot)
            .map_err(|error| map_recovery_error(&error))?;
        match reconciliation {
            RecoveryReconciliation::Clean => return Ok(false),
            RecoveryReconciliation::CommitCompleted => {
                let CredentialDocument::Parsed(auth) = &snapshot.document else {
                    return Err(recovery_required_error());
                };
                let pinned_identity = self.account_identity.as_ref().ok_or_else(|| {
                    RefreshTokenError::Permanent(
                        "OAuth credential has no pinned account owner".to_owned(),
                    )
                })?;
                if &account_identity(auth)? != pinned_identity {
                    return Err(RefreshTokenError::Conflict(
                        "OAuth account identity changed after refresh commit".to_owned(),
                    ));
                }
                *self.auth.lock().await = ready_file(auth.clone(), snapshot.revision.clone());
                return Ok(true);
            }
            RecoveryReconciliation::RecoveryRequired => {}
        }
        let CredentialDocument::Parsed(auth) = &snapshot.document else {
            return Err(recovery_required_error());
        };
        let error = recovery_required_error();
        *self.auth.lock().await = CachedAuthState::Indeterminate {
            observed_revision: snapshot.revision.clone(),
            observed_lineage: refresh_lineage(auth)?,
            error: error.clone(),
        };
        Err(error)
    }
}

fn ready_file(
    auth: Box<AuthDotJson>,
    revision: Option<crate::provider::openai_oauth::credential_transaction::CredentialRevision>,
) -> CachedAuthState {
    CachedAuthState::Ready {
        auth: CodexAuth::ChatGpt(auth),
        revision,
    }
}

fn reconcile_changed_snapshot(
    current: &mut CachedAuthState,
    pinned_identity: Option<&AccountIdentity>,
    snapshot: CredentialSnapshot,
) -> Result<(), RefreshTokenError> {
    match snapshot.document {
        CredentialDocument::Missing => {
            *current = CachedAuthState::Missing;
            Ok(())
        }
        CredentialDocument::Malformed(_) => Err(RefreshTokenError::Conflict(
            "OAuth credential changed to a malformed document".to_owned(),
        )),
        CredentialDocument::Parsed(auth) => {
            let pinned_identity = pinned_identity.ok_or_else(|| {
                RefreshTokenError::Conflict(
                    "OAuth credential appeared after manager construction; construct a new provider"
                        .to_owned(),
                )
            })?;
            if &account_identity(&auth)? != pinned_identity {
                return Err(RefreshTokenError::Conflict(
                    "OAuth account identity changed while the provider was active".to_owned(),
                ));
            }
            *current = ready_file(auth, snapshot.revision);
            Ok(())
        }
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

fn map_transaction_coordination(error: &CredentialTransactionError) -> RefreshTokenError {
    match error {
        CredentialTransactionError::Conflict
        | CredentialTransactionError::VerificationConflict
        | CredentialTransactionError::RecoveryIncomplete(_) => {
            RefreshTokenError::Conflict(error.to_string())
        }
        CredentialTransactionError::PublishedButUndurable { .. }
        | CredentialTransactionError::DeletedButUndurable(_) => {
            RefreshTokenError::Undurable(error.to_string())
        }
        CredentialTransactionError::DescriptorAdmission(_)
        | CredentialTransactionError::OpenRoot(_)
        | CredentialTransactionError::OpenLock(_)
        | CredentialTransactionError::LockTimeout { .. }
        | CredentialTransactionError::Lock(_)
        | CredentialTransactionError::Storage(_) => {
            RefreshTokenError::Coordination(error.to_string())
        }
    }
}

fn recovery_required_error() -> RefreshTokenError {
    RefreshTokenError::Indeterminate(
        "OAuth refresh recovery is required; the prior refresh token was not replayed".to_owned(),
    )
}

fn map_recovery_error(error: &RecoveryJournalError) -> RefreshTokenError {
    match error {
        RecoveryJournalError::Invalid => recovery_required_error(),
        RecoveryJournalError::Changed => RefreshTokenError::Conflict(
            "OAuth refresh recovery state changed during reconciliation".to_owned(),
        ),
        RecoveryJournalError::EntropyUnavailable
        | RecoveryJournalError::Serialization(_)
        | RecoveryJournalError::Io(_)
        | RecoveryJournalError::PublishedButUndurable(_)
        | RecoveryJournalError::DeletedButUndurable(_) => RefreshTokenError::Coordination(
            "OAuth refresh recovery state could not be durably reconciled".to_owned(),
        ),
    }
}
