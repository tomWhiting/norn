use std::collections::BTreeMap;
use std::error::Error;
use std::sync::mpsc;

use super::super::auth_root::resolve_norn_auth_root;
use super::super::credential_decode::MalformedCredentialReason;
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::*;

type TestResult = Result<(), Box<dyn Error>>;

fn auth_document(access: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("account"),
        access_token: access.to_owned(),
        refresh_token: "refresh".to_owned(),
        account_id: Some("account".to_owned()),
        additional_fields: BTreeMap::default(),
    })
}

fn root(directory: &tempfile::TempDir) -> Result<NornAuthRoot, Box<dyn Error>> {
    resolve_norn_auth_root(Some(directory.path().join("auth"))).map_err(Into::into)
}

fn lock_timing(deadline: Duration) -> Result<CredentialLockTiming, Box<dyn Error>> {
    CredentialLockTiming::new(deadline, Duration::from_millis(1)).map_err(Into::into)
}

fn require_revision(
    revision: Option<CredentialRevision>,
) -> Result<CredentialRevision, std::io::Error> {
    revision.ok_or_else(|| std::io::Error::other("credential revision missing"))
}

fn parsed_auth(snapshot: CredentialSnapshot) -> Result<AuthDotJson, std::io::Error> {
    match snapshot.document {
        CredentialDocument::Parsed(auth) => Ok(*auth),
        CredentialDocument::Missing | CredentialDocument::Malformed(_) => {
            Err(std::io::Error::other("parsed credential missing"))
        }
    }
}

#[test]
fn compare_before_save_rejects_lock_ignoring_foreign_write() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let first = auth_document("first");
    let first_revision = transaction.save_if_revision(None, &first)?;

    let foreign = auth_document("foreign");
    std::fs::write(
        root.as_path().join(AUTH_JSON_FILE),
        serde_json::to_vec_pretty(&foreign)?,
    )?;
    let result = transaction.save_if_revision(Some(&first_revision), &auth_document("proposed"));

    assert!(matches!(result, Err(CredentialTransactionError::Conflict)));
    assert_eq!(parsed_auth(transaction.snapshot()?)?, foreign);
    Ok(())
}

#[test]
fn replay_of_already_published_bytes_converges() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let first_revision = transaction.save_if_revision(None, &auth_document("first"))?;
    let proposed = auth_document("proposed");
    let published = transaction.save_if_revision(Some(&first_revision), &proposed)?;

    let replayed = transaction.save_if_revision(Some(&first_revision), &proposed)?;

    assert_eq!(replayed, published);
    assert_eq!(parsed_auth(transaction.snapshot()?)?, proposed);
    Ok(())
}

#[test]
fn missing_already_published_bytes_are_a_verification_conflict() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let published = transaction.save_if_revision(None, &auth_document("published"))?;
    std::fs::remove_file(root.as_path().join(AUTH_JSON_FILE))?;

    let result = transaction.sync_existing_revision(&published);

    assert!(matches!(
        result,
        Err(CredentialTransactionError::VerificationConflict)
    ));
    Ok(())
}

#[test]
fn delete_rejects_a_replacement_after_snapshot() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let first = auth_document("first");
    transaction.save_if_revision(None, &first)?;
    let observed = require_revision(transaction.snapshot()?.revision)?;
    let replacement = auth_document("replacement");
    std::fs::write(
        root.as_path().join(AUTH_JSON_FILE),
        serde_json::to_vec_pretty(&replacement)?,
    )?;

    let result = transaction.delete_if_revision(Some(&observed));

    assert!(matches!(result, Err(CredentialTransactionError::Conflict)));
    assert_eq!(parsed_auth(transaction.snapshot()?)?, replacement);
    Ok(())
}

#[test]
fn delete_never_replaces_a_crash_retained_quarantine() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let first = auth_document("first");
    transaction.save_if_revision(None, &first)?;
    let observed = require_revision(transaction.snapshot()?.revision)?;
    let quarantine = quarantine_path();
    let retained = b"retained credential recovery evidence";
    std::fs::write(root.as_path().join(&quarantine), retained)?;

    let result = transaction.delete_if_revision_at(Some(&observed), &quarantine);

    assert!(matches!(
        result,
        Err(CredentialTransactionError::Storage(_))
    ));
    assert_eq!(parsed_auth(transaction.snapshot()?)?, first);
    assert_eq!(std::fs::read(root.as_path().join(quarantine))?, retained);
    Ok(())
}

#[test]
fn process_gate_honors_the_caller_deadline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let held = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let contender_root = root;
    let contender_timing = lock_timing(Duration::from_millis(20))?;
    let (sender, receiver) = mpsc::channel();
    let contender = std::thread::spawn(move || {
        let result = CredentialTransaction::acquire(&contender_root, contender_timing);
        let send_result = sender.send(result);
        drop(send_result);
    });

    let result = receiver.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        result,
        Err(CredentialTransactionError::LockTimeout { .. })
    ));
    drop(held);
    if let Err(panic_payload) = contender.join() {
        drop(panic_payload);
        return Err(std::io::Error::other("credential contender thread panicked").into());
    }
    Ok(())
}

#[test]
fn revisions_track_raw_bytes_not_only_decoded_values() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let document = auth_document("first");
    transaction.save_if_revision(None, &document)?;
    let first = require_revision(transaction.snapshot()?.revision)?;

    std::fs::write(
        root.as_path().join(AUTH_JSON_FILE),
        serde_json::to_vec(&document)?,
    )?;
    let compact = require_revision(transaction.snapshot()?.revision)?;

    assert_ne!(first, compact);
    Ok(())
}

#[test]
fn malformed_document_retains_its_raw_revision() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    std::fs::write(root.as_path().join(AUTH_JSON_FILE), b"{malformed")?;

    let snapshot = transaction.snapshot()?;

    assert!(matches!(
        snapshot.document,
        CredentialDocument::Malformed(MalformedCredentialReason::InvalidJson)
    ));
    assert!(snapshot.revision.is_some());
    Ok(())
}

#[test]
fn semantic_malformed_document_retains_reason_and_revision() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = root(&directory)?;
    let transaction = CredentialTransaction::acquire(&root, lock_timing(Duration::from_secs(1))?)?;
    let id_token = IdTokenInfo::create_for_testing("account-a");
    let raw = serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token.raw_jwt,
            "access_token": "access",
            "refresh_token": "refresh",
            "account_id": "account-b"
        }
    }))?;
    std::fs::write(root.as_path().join(AUTH_JSON_FILE), raw)?;

    let snapshot = transaction.snapshot()?;

    assert!(matches!(
        snapshot.document,
        CredentialDocument::Malformed(MalformedCredentialReason::ConflictingAccountIds)
    ));
    assert!(snapshot.revision.is_some());
    Ok(())
}
