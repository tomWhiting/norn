//! Restart-safe journal for ambiguous OAuth refresh rotation.

use std::io::{ErrorKind, Read as _};
use std::path::Path;

use rand::TryRngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::credential_revision::{CredentialRevision, revision, serialize_auth};
use super::credential_transaction::{
    CredentialDocument, CredentialSnapshot, CredentialTransaction,
};
use super::types::AuthDotJson;

#[path = "credential_recovery_io.rs"]
pub(super) mod io;

const JOURNAL_FILE: &str = ".norn-auth-refresh-recovery.json";
const JOURNAL_VERSION: u8 = 1;
const LINEAGE_PROOF_DOMAIN: &[u8] = b"norn:oauth:refresh-recovery:lineage:v1";
const MARKER_INTEGRITY_DOMAIN: &[u8] = b"norn:oauth:refresh-recovery:marker:v1\0";

/// Result of reconciling a retained journal against durable credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryReconciliation {
    /// No unresolved refresh outcome remains.
    Clean,
    /// The exact proposed credential revision was already durably published.
    CommitCompleted,
    /// The same durable refresh-token lineage may already have been consumed.
    RecoveryRequired,
}

/// One in-process handle to a durably journaled refresh operation.
pub(crate) struct RefreshRecoveryOperation {
    marker: RefreshRecoveryMarker,
}

impl std::fmt::Debug for RefreshRecoveryOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RefreshRecoveryOperation([REDACTED])")
    }
}

/// Failure to durably inspect or update the recovery journal.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecoveryJournalError {
    /// The operating system did not provide nonce and salt material.
    #[error("OAuth refresh recovery could not obtain operating-system randomness")]
    EntropyUnavailable,
    /// The retained journal was malformed, unsupported, or inconsistent.
    #[error("OAuth refresh recovery journal is invalid")]
    Invalid,
    /// A lock-ignoring writer changed the journal during the operation.
    #[error("OAuth refresh recovery journal changed during the operation")]
    Changed,
    /// Journal serialization failed without exposing its contents.
    #[error("OAuth refresh recovery journal serialization failed")]
    Serialization(#[source] serde_json::Error),
    /// Private journal storage failed.
    #[error("OAuth refresh recovery journal I/O failed: {0}")]
    Io(#[source] std::io::Error),
    /// New journal bytes were visible but directory durability was unconfirmed.
    #[error("OAuth refresh recovery journal publication durability was not confirmed: {0}")]
    PublishedButUndurable(#[source] std::io::Error),
    /// Journal deletion occurred but directory durability was unconfirmed.
    #[error("OAuth refresh recovery journal deletion durability was not confirmed: {0}")]
    DeletedButUndurable(#[source] std::io::Error),
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
struct RefreshRecoveryMarker {
    version: u8,
    operation_nonce: [u8; 32],
    prior_revision: [u8; 32],
    lineage_salt: [u8; 32],
    lineage_proof: [u8; 32],
    phase: RecoveryPhase,
    integrity_proof: [u8; 32],
}

impl std::fmt::Debug for RefreshRecoveryMarker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RefreshRecoveryMarker([REDACTED])")
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum RecoveryPhase {
    OutcomeUnknown,
    CommitPending { proposed_revision: [u8; 32] },
    ResolvedNoRotation,
}

impl CredentialTransaction {
    /// Persist the no-replay barrier before authority dispatch.
    pub(crate) fn begin_refresh_recovery(
        &self,
        expected_revision: &CredentialRevision,
        auth: &AuthDotJson,
    ) -> Result<RefreshRecoveryOperation, RecoveryJournalError> {
        let mut material = [[0_u8; 32]; 2];
        for bytes in &mut material {
            if rand::rngs::OsRng.try_fill_bytes(bytes).is_err() {
                return Err(RecoveryJournalError::EntropyUnavailable);
            }
        }
        self.begin_refresh_recovery_with_material(expected_revision, auth, material)
    }

    fn begin_refresh_recovery_with_material(
        &self,
        expected_revision: &CredentialRevision,
        auth: &AuthDotJson,
        material: [[u8; 32]; 2],
    ) -> Result<RefreshRecoveryOperation, RecoveryJournalError> {
        let refresh_token = refresh_token(auth)?;
        let mut marker = RefreshRecoveryMarker {
            version: JOURNAL_VERSION,
            operation_nonce: material[0],
            prior_revision: expected_revision.0,
            lineage_salt: material[1],
            lineage_proof: lineage_proof(&material[1], refresh_token),
            phase: RecoveryPhase::OutcomeUnknown,
            integrity_proof: [0; 32],
        };
        marker.integrity_proof = marker_integrity(&marker);
        self.replace_marker(None, &marker)?;
        Ok(RefreshRecoveryOperation { marker })
    }

    /// Record a parsed authority success before publishing credential bytes.
    pub(crate) fn mark_refresh_commit_pending(
        &self,
        operation: &mut RefreshRecoveryOperation,
        proposed_revision: &CredentialRevision,
    ) -> Result<(), RecoveryJournalError> {
        let mut proposed = operation.marker.clone();
        proposed.phase = RecoveryPhase::CommitPending {
            proposed_revision: proposed_revision.0,
        };
        proposed.integrity_proof = marker_integrity(&proposed);
        self.replace_marker(Some(&operation.marker), &proposed)?;
        operation.marker = proposed;
        Ok(())
    }

    /// Record a response that proves no credential rotation occurred.
    pub(crate) fn mark_refresh_resolved_no_rotation(
        &self,
        operation: &mut RefreshRecoveryOperation,
    ) -> Result<(), RecoveryJournalError> {
        let mut proposed = operation.marker.clone();
        proposed.phase = RecoveryPhase::ResolvedNoRotation;
        proposed.integrity_proof = marker_integrity(&proposed);
        self.replace_marker(Some(&operation.marker), &proposed)?;
        operation.marker = proposed;
        Ok(())
    }

    /// Durably remove the exact completed operation marker.
    pub(crate) fn finish_refresh_recovery(
        &self,
        operation: &RefreshRecoveryOperation,
    ) -> Result<(), RecoveryJournalError> {
        self.clear_marker(&operation.marker)
    }

    /// Reconcile retained state without ever replaying a refresh request.
    pub(crate) fn reconcile_refresh_recovery(
        &self,
        snapshot: &CredentialSnapshot,
    ) -> Result<RecoveryReconciliation, RecoveryJournalError> {
        let Some(marker) = self.load_marker()? else {
            return Ok(RecoveryReconciliation::Clean);
        };
        validate_marker(&marker)?;

        if matches!(&marker.phase, RecoveryPhase::ResolvedNoRotation) {
            self.clear_marker(&marker)?;
            return Ok(RecoveryReconciliation::Clean);
        }
        if let RecoveryPhase::CommitPending { proposed_revision } = &marker.phase
            && snapshot_revision(snapshot) == Some(*proposed_revision)
        {
            self.clear_marker(&marker)?;
            return Ok(RecoveryReconciliation::CommitCompleted);
        }
        let CredentialDocument::Parsed(auth) = &snapshot.document else {
            return Err(RecoveryJournalError::Invalid);
        };
        if marker_matches_lineage(&marker, auth)? {
            return Ok(RecoveryReconciliation::RecoveryRequired);
        }
        if snapshot_revision(snapshot) == Some(marker.prior_revision) {
            return Err(RecoveryJournalError::Invalid);
        }
        self.clear_marker(&marker)?;
        Ok(RecoveryReconciliation::Clean)
    }

    /// Reconstruct the commit stage for a known in-memory success response.
    pub(crate) fn prepare_pending_refresh_recovery(
        &self,
        snapshot: &CredentialSnapshot,
        expected_revision: &CredentialRevision,
        refreshed: &AuthDotJson,
    ) -> Result<Option<RefreshRecoveryOperation>, RecoveryJournalError> {
        let (_, proposed_revision) =
            serialize_auth(refreshed).map_err(RecoveryJournalError::Serialization)?;
        let marker = self.load_marker()?;
        match marker {
            None => {
                self.prepare_unjournaled_pending(snapshot, expected_revision, &proposed_revision)
            }
            Some(marker) => self.prepare_journaled_pending(
                snapshot,
                expected_revision,
                &proposed_revision,
                marker,
            ),
        }
    }

    fn prepare_unjournaled_pending(
        &self,
        snapshot: &CredentialSnapshot,
        expected_revision: &CredentialRevision,
        proposed_revision: &CredentialRevision,
    ) -> Result<Option<RefreshRecoveryOperation>, RecoveryJournalError> {
        if snapshot.revision.as_ref() == Some(proposed_revision) {
            return Ok(None);
        }
        if snapshot.revision.as_ref() != Some(expected_revision) {
            return Err(RecoveryJournalError::Changed);
        }
        let CredentialDocument::Parsed(current) = &snapshot.document else {
            return Err(RecoveryJournalError::Invalid);
        };
        let mut operation = self.begin_refresh_recovery(expected_revision, current)?;
        self.mark_refresh_commit_pending(&mut operation, proposed_revision)?;
        Ok(Some(operation))
    }

    fn prepare_journaled_pending(
        &self,
        snapshot: &CredentialSnapshot,
        expected_revision: &CredentialRevision,
        proposed_revision: &CredentialRevision,
        marker: RefreshRecoveryMarker,
    ) -> Result<Option<RefreshRecoveryOperation>, RecoveryJournalError> {
        validate_marker(&marker)?;
        if marker.prior_revision != expected_revision.0 {
            return Err(RecoveryJournalError::Changed);
        }
        match marker.phase.clone() {
            RecoveryPhase::ResolvedNoRotation => Err(RecoveryJournalError::Changed),
            RecoveryPhase::OutcomeUnknown => {
                if snapshot.revision.as_ref() != Some(expected_revision) {
                    return Err(RecoveryJournalError::Changed);
                }
                let CredentialDocument::Parsed(current) = &snapshot.document else {
                    return Err(RecoveryJournalError::Invalid);
                };
                if !marker_matches_lineage(&marker, current)? {
                    return Err(RecoveryJournalError::Invalid);
                }
                let mut operation = RefreshRecoveryOperation { marker };
                self.mark_refresh_commit_pending(&mut operation, proposed_revision)?;
                Ok(Some(operation))
            }
            RecoveryPhase::CommitPending {
                proposed_revision: recorded,
            } => {
                if recorded != proposed_revision.0 {
                    return Err(RecoveryJournalError::Changed);
                }
                let current = snapshot_revision(snapshot);
                if current != Some(expected_revision.0) && current != Some(proposed_revision.0) {
                    return Err(RecoveryJournalError::Changed);
                }
                if current == Some(expected_revision.0) {
                    let CredentialDocument::Parsed(auth) = &snapshot.document else {
                        return Err(RecoveryJournalError::Invalid);
                    };
                    if !marker_matches_lineage(&marker, auth)? {
                        return Err(RecoveryJournalError::Invalid);
                    }
                }
                Ok(Some(RefreshRecoveryOperation { marker }))
            }
        }
    }

    fn load_marker(&self) -> Result<Option<RefreshRecoveryMarker>, RecoveryJournalError> {
        let Some(raw) = self.load_marker_raw()? else {
            return Ok(None);
        };
        serde_json::from_slice(&raw).map(Some).map_err(|error| {
            tracing::debug!(%error, "OAuth refresh recovery journal did not decode");
            RecoveryJournalError::Invalid
        })
    }

    fn load_marker_raw(&self) -> Result<Option<Vec<u8>>, RecoveryJournalError> {
        let mut file = match self.root.open_read(Path::new(JOURNAL_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(RecoveryJournalError::Io(error)),
        };
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)
            .map_err(RecoveryJournalError::Io)?;
        Ok(Some(raw))
    }

    pub(crate) fn recovery_revision_for_logout(
        &self,
    ) -> Result<Option<CredentialRevision>, RecoveryJournalError> {
        self.load_marker_raw()
            .map(|raw| raw.as_deref().map(revision))
    }

    pub(crate) fn clear_recovery_after_logout(
        &self,
        expected: Option<&CredentialRevision>,
    ) -> Result<(), RecoveryJournalError> {
        let current = self.recovery_revision_for_logout()?;
        if current.is_none() {
            return Ok(());
        }
        if current.as_ref() != expected {
            return Err(RecoveryJournalError::Changed);
        }
        self.remove_marker_file()
    }
}

fn validate_marker(marker: &RefreshRecoveryMarker) -> Result<(), RecoveryJournalError> {
    if marker.version != JOURNAL_VERSION || marker.integrity_proof != marker_integrity(marker) {
        return Err(RecoveryJournalError::Invalid);
    }
    Ok(())
}

fn marker_integrity(marker: &RefreshRecoveryMarker) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(MARKER_INTEGRITY_DOMAIN);
    digest.update([1, 1, marker.version]);
    digest.update([2, 32]);
    digest.update(marker.operation_nonce);
    digest.update([3, 32]);
    digest.update(marker.prior_revision);
    digest.update([4, 32]);
    digest.update(marker.lineage_salt);
    digest.update([5, 32]);
    digest.update(marker.lineage_proof);
    match &marker.phase {
        RecoveryPhase::OutcomeUnknown => digest.update([6, 1, 1]),
        RecoveryPhase::CommitPending { proposed_revision } => {
            digest.update([6, 33, 2]);
            digest.update(proposed_revision);
        }
        RecoveryPhase::ResolvedNoRotation => digest.update([6, 1, 3]),
    }
    digest.finalize().into()
}

fn snapshot_revision(snapshot: &CredentialSnapshot) -> Option<[u8; 32]> {
    snapshot.revision.as_ref().map(|revision| revision.0)
}

fn refresh_token(auth: &AuthDotJson) -> Result<&str, RecoveryJournalError> {
    auth.tokens
        .as_ref()
        .map(|tokens| tokens.refresh_token.as_str())
        .filter(|token| !token.is_empty())
        .ok_or(RecoveryJournalError::Invalid)
}

fn marker_matches_lineage(
    marker: &RefreshRecoveryMarker,
    auth: &AuthDotJson,
) -> Result<bool, RecoveryJournalError> {
    Ok(lineage_proof(&marker.lineage_salt, refresh_token(auth)?) == marker.lineage_proof)
}

fn lineage_proof(salt: &[u8], refresh_token: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(LINEAGE_PROOF_DOMAIN);
    digest.update([0]);
    digest.update(salt);
    digest.update([0]);
    digest.update(refresh_token.as_bytes());
    digest.finalize().into()
}

#[cfg(test)]
#[path = "credential_recovery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "manager_recovery_protocol_tests.rs"]
mod protocol_tests;

#[cfg(test)]
#[path = "credential_recovery_fault_tests.rs"]
mod fault_tests;
