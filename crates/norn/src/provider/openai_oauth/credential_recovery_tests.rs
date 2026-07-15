use std::collections::BTreeMap;
use std::error::Error;
use std::time::Duration;

use wiremock::MockServer;

use super::super::auth_root::{NornAuthRoot, resolve_norn_auth_root};
use super::super::credential_lock_timing::CredentialLockTiming;
use super::super::credential_revision::serialize_auth;
use super::super::storage::AuthCredentialsStoreMode;
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::*;
use crate::provider::openai_oauth::{AuthManager, RefreshTokenError};

type TestResult = Result<(), Box<dyn Error>>;

fn auth_document(access: &str, refresh: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("recovery-account"),
        access_token: access.to_owned(),
        refresh_token: refresh.to_owned(),
        account_id: Some("recovery-account".to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

fn root(directory: &tempfile::TempDir) -> Result<NornAuthRoot, Box<dyn Error>> {
    resolve_norn_auth_root(Some(directory.path().join("auth"))).map_err(Into::into)
}

fn timing() -> Result<CredentialLockTiming, Box<dyn Error>> {
    CredentialLockTiming::new(Duration::from_secs(1), Duration::from_millis(1)).map_err(Into::into)
}

fn material(seed: u8) -> [[u8; 32]; 2] {
    [[seed; 32]; 2]
}

fn marker_path(root: &NornAuthRoot) -> std::path::PathBuf {
    root.as_path().join(JOURNAL_FILE)
}

fn required_revision(snapshot: &CredentialSnapshot) -> Result<CredentialRevision, std::io::Error> {
    snapshot
        .revision
        .clone()
        .ok_or_else(|| std::io::Error::other("test credential revision is missing"))
}

fn request_count(requests: Option<Vec<wiremock::Request>>) -> Result<usize, std::io::Error> {
    requests
        .map(|requests| requests.len())
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))
}

#[test]
fn outcome_unknown_survives_restart_without_disclosing_credentials() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let auth = auth_document("private-access", "private-refresh");
    transaction.save_if_revision(None, &auth)?;
    let snapshot = transaction.snapshot()?;
    let revision = required_revision(&snapshot)?;
    let operation =
        transaction.begin_refresh_recovery_with_material(&revision, &auth, material(0x31))?;

    let raw = std::fs::read(marker_path(&root))?;
    let rendered = String::from_utf8(raw)?;
    let debug = format!("{operation:?}");
    let root_path = root.as_path().to_string_lossy();
    for secret in [
        "private-access",
        "private-refresh",
        "recovery-account",
        root_path.as_ref(),
        "access_token",
        "refresh_token",
        "account_id",
        "email",
        "path",
        "pid",
        "timestamp",
        "ttl",
        "retry_count",
        "endpoint",
    ] {
        assert!(!rendered.contains(secret));
        assert!(!debug.contains(secret));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = std::fs::metadata(marker_path(&root))?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    drop(transaction);

    let restarted = CredentialTransaction::acquire(&root, timing()?)?;
    let snapshot = restarted.snapshot()?;
    assert_eq!(
        restarted.reconcile_refresh_recovery(&snapshot)?,
        RecoveryReconciliation::RecoveryRequired
    );
    assert!(marker_path(&root).is_file());
    Ok(())
}

#[test]
fn commit_pending_before_credential_save_requires_recovery() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let current = auth_document("old-access", "old-refresh");
    transaction.save_if_revision(None, &current)?;
    let snapshot = transaction.snapshot()?;
    let revision = required_revision(&snapshot)?;
    let mut operation =
        transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x42))?;
    let proposed = auth_document("new-access", "old-refresh");
    let (_, proposed_revision) = serialize_auth(&proposed)?;
    transaction.mark_refresh_commit_pending(&mut operation, &proposed_revision)?;
    drop(transaction);

    let restarted = CredentialTransaction::acquire(&root, timing()?)?;
    let snapshot = restarted.snapshot()?;
    assert_eq!(
        restarted.reconcile_refresh_recovery(&snapshot)?,
        RecoveryReconciliation::RecoveryRequired
    );
    Ok(())
}

#[test]
fn durable_credential_commit_proves_success_and_clears_retained_marker() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let current = auth_document("old-access", "old-refresh");
    let revision = transaction.save_if_revision(None, &current)?;
    let mut operation =
        transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x53))?;
    let proposed = auth_document("new-access", "old-refresh");
    let (_, proposed_revision) = serialize_auth(&proposed)?;
    transaction.mark_refresh_commit_pending(&mut operation, &proposed_revision)?;
    let published = transaction.save_if_revision(Some(&revision), &proposed)?;
    assert_eq!(published, proposed_revision);
    drop(transaction);

    let restarted = CredentialTransaction::acquire(&root, timing()?)?;
    let snapshot = restarted.snapshot()?;
    assert_eq!(
        restarted.reconcile_refresh_recovery(&snapshot)?,
        RecoveryReconciliation::CommitCompleted
    );
    assert!(!marker_path(&root).exists());
    Ok(())
}

#[test]
fn resolved_no_rotation_clears_after_crash_before_delete() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let current = auth_document("old-access", "old-refresh");
    let revision = transaction.save_if_revision(None, &current)?;
    let mut operation =
        transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x64))?;
    transaction.mark_refresh_resolved_no_rotation(&mut operation)?;
    drop(transaction);

    let restarted = CredentialTransaction::acquire(&root, timing()?)?;
    let snapshot = restarted.snapshot()?;
    assert_eq!(
        restarted.reconcile_refresh_recovery(&snapshot)?,
        RecoveryReconciliation::Clean
    );
    assert!(!marker_path(&root).exists());
    Ok(())
}

#[test]
fn durable_external_replacement_supersedes_unknown_outcome() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let current = auth_document("old-access", "old-refresh");
    let revision = transaction.save_if_revision(None, &current)?;
    transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x75))?;
    let replacement = auth_document("login-access", "login-refresh");
    transaction.save_if_revision(Some(&revision), &replacement)?;
    drop(transaction);

    let restarted = CredentialTransaction::acquire(&root, timing()?)?;
    let snapshot = restarted.snapshot()?;
    assert_eq!(
        restarted.reconcile_refresh_recovery(&snapshot)?,
        RecoveryReconciliation::Clean
    );
    assert!(!marker_path(&root).exists());
    Ok(())
}

#[test]
fn same_lineage_rewrite_does_not_clear_replay_barrier() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let current = auth_document("old-access", "old-refresh");
    let revision = transaction.save_if_revision(None, &current)?;
    transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x76))?;
    std::fs::write(
        root.as_path().join(super::super::storage::AUTH_JSON_FILE),
        serde_json::to_vec(&current)?,
    )?;
    let snapshot = transaction.snapshot()?;
    assert_ne!(snapshot.revision.as_ref(), Some(&revision));
    assert_eq!(
        transaction.reconcile_refresh_recovery(&snapshot)?,
        RecoveryReconciliation::RecoveryRequired
    );
    assert!(marker_path(&root).is_file());
    Ok(())
}

#[test]
fn corrupted_marker_fields_never_clear_the_replay_barrier() -> TestResult {
    for field in [
        "version",
        "operation_nonce",
        "prior_revision",
        "lineage_salt",
        "lineage_proof",
        "phase",
        "integrity_proof",
    ] {
        let directory = tempfile::tempdir()?;
        let root = root(&directory)?;
        let transaction = CredentialTransaction::acquire(&root, timing()?)?;
        let current = auth_document("old-access", "old-refresh");
        let revision = transaction.save_if_revision(None, &current)?;
        let _operation = transaction.begin_refresh_recovery_with_material(
            &revision,
            &current,
            material(0x78),
        )?;
        let mut marker = transaction
            .load_marker()?
            .ok_or_else(|| std::io::Error::other("test recovery marker is missing"))?;
        match field {
            "version" => marker.version ^= 1,
            "operation_nonce" => marker.operation_nonce[0] ^= 1,
            "prior_revision" => marker.prior_revision[0] ^= 1,
            "lineage_salt" => marker.lineage_salt[0] ^= 1,
            "lineage_proof" => marker.lineage_proof[0] ^= 1,
            "phase" => marker.phase = RecoveryPhase::ResolvedNoRotation,
            "integrity_proof" => marker.integrity_proof[0] ^= 1,
            _ => return Err(std::io::Error::other("unknown marker mutation").into()),
        }
        std::fs::write(marker_path(&root), serde_json::to_vec_pretty(&marker)?)?;

        assert!(matches!(
            transaction.reconcile_refresh_recovery(&transaction.snapshot()?),
            Err(RecoveryJournalError::Invalid)
        ));
        assert!(marker_path(&root).is_file());
    }
    Ok(())
}

#[test]
fn exact_prior_revision_rejects_a_valid_but_displaced_lineage_proof() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let current = auth_document("old-access", "old-refresh");
    let revision = transaction.save_if_revision(None, &current)?;
    let _operation =
        transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x79))?;
    let mut marker = transaction
        .load_marker()?
        .ok_or_else(|| std::io::Error::other("test recovery marker is missing"))?;
    marker.lineage_proof = lineage_proof(&marker.lineage_salt, "different-refresh-lineage");
    marker.integrity_proof = marker_integrity(&marker);
    std::fs::write(marker_path(&root), serde_json::to_vec_pretty(&marker)?)?;

    assert!(matches!(
        transaction.reconcile_refresh_recovery(&transaction.snapshot()?),
        Err(RecoveryJournalError::Invalid)
    ));
    assert!(marker_path(&root).is_file());
    Ok(())
}

#[test]
fn missing_or_malformed_replacement_cannot_prove_lineage_advancement() -> TestResult {
    for replacement in [None, Some(b"{malformed".as_slice())] {
        let directory = tempfile::tempdir()?;
        let root = root(&directory)?;
        let transaction = CredentialTransaction::acquire(&root, timing()?)?;
        let current = auth_document("old-access", "old-refresh");
        let revision = transaction.save_if_revision(None, &current)?;
        transaction.begin_refresh_recovery_with_material(&revision, &current, material(0x77))?;
        let auth_path = root.as_path().join(super::super::storage::AUTH_JSON_FILE);
        match replacement {
            Some(raw) => std::fs::write(auth_path, raw)?,
            None => std::fs::remove_file(auth_path)?,
        }
        let snapshot = transaction.snapshot()?;
        assert!(matches!(
            transaction.reconcile_refresh_recovery(&snapshot),
            Err(RecoveryJournalError::Invalid)
        ));
        assert!(marker_path(&root).is_file());
    }
    Ok(())
}

#[test]
fn malformed_journal_error_never_echoes_retained_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let auth = auth_document("old-access", "old-refresh");
    transaction.save_if_revision(None, &auth)?;
    std::fs::write(
        marker_path(&root),
        br#"{"private":"journal-secret-sentinel"}"#,
    )?;

    let result = transaction.reconcile_refresh_recovery(&transaction.snapshot()?);
    let Err(error) = result else {
        return Err(std::io::Error::other("malformed journal unexpectedly reconciled").into());
    };
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains("journal-secret-sentinel"));
    assert!(!rendered.contains("old-refresh"));
    Ok(())
}

#[tokio::test]
async fn restarted_manager_blocks_same_lineage_without_http_dispatch() -> TestResult {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, timing()?)?;
    let auth = auth_document("old-access", "old-refresh");
    let revision = transaction.save_if_revision(None, &auth)?;
    transaction.begin_refresh_recovery(&revision, &auth)?;
    drop(transaction);

    let manager =
        AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, server.uri()).await?;
    let result = manager.refresh_token_from_authority().await;

    assert!(matches!(result, Err(RefreshTokenError::Indeterminate(_))));
    assert_eq!(request_count(server.received_requests().await)?, 0);
    Ok(())
}
