use std::error::Error;

use norn_policy::ResponsesContractError;
use norn_policy::baseline::{
    P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    P1_GOVERNANCE_ANCHOR_IDENTITY,
};
use norn_policy::finding::EvidenceTraceabilityIssue;
use norn_policy::phase_lock::{P1AuthorityError, P1AuthorityKind, ReadyP1Authorities};
use norn_policy::version::{ANALYZER_VERSION, DIGEST_VERSION};
use norn_policy::{
    CompleteCurrentSnapshot, Digest, EntryKind, GitObjectId, OwnedSnapshot, P1BaseSnapshot,
    P1EvaluationInput, RepositoryPath, SnapshotEntry,
};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PHASE_LOCK_PATH: &str = "policy/phase-lock.json";
const GENERATED_INCLUDES_PATH: &str = "policy/generated-includes.json";

#[test]
fn fixed_phase_lock_path_is_mandatory() -> TestResult {
    let empty = OwnedSnapshot::empty();
    assert!(matches!(
        acquire(&empty),
        Err(P1AuthorityError::Missing(P1AuthorityKind::PhaseLock))
    ));

    let alternate = snapshot(&[(
        "policy/alternate-phase-lock.json",
        EntryKind::Regular,
        lock_bytes()?.as_slice(),
    )])?;
    assert!(matches!(
        acquire(&alternate),
        Err(P1AuthorityError::Missing(P1AuthorityKind::PhaseLock))
    ));
    Ok(())
}

#[test]
fn phase_lock_must_be_regular_and_strict() -> TestResult {
    let bytes = lock_bytes()?;
    let symlink = snapshot(&[(PHASE_LOCK_PATH, EntryKind::Symlink, bytes.as_slice())])?;
    assert!(matches!(
        acquire(&symlink),
        Err(P1AuthorityError::NotRegular(P1AuthorityKind::PhaseLock))
    ));

    let invalid = snapshot(&[(
        PHASE_LOCK_PATH,
        EntryKind::Regular,
        br#"{"schema_version":1,"schema_version":1}"#,
    )])?;
    assert!(matches!(
        acquire(&invalid),
        Err(P1AuthorityError::Invalid(P1AuthorityKind::PhaseLock))
    ));
    Ok(())
}

#[test]
fn generated_registry_is_read_from_its_fixed_path() -> TestResult {
    let lock = lock_bytes()?;
    let only_lock = snapshot(&[(PHASE_LOCK_PATH, EntryKind::Regular, lock.as_slice())])?;
    assert!(matches!(
        acquire(&only_lock),
        Err(P1AuthorityError::Missing(
            P1AuthorityKind::GeneratedIncludes
        ))
    ));

    let invalid_registry = snapshot(&[
        (PHASE_LOCK_PATH, EntryKind::Regular, lock.as_slice()),
        (
            GENERATED_INCLUDES_PATH,
            EntryKind::Regular,
            br#"{"schema_version":1,"entries":[],"unknown":true}"#,
        ),
    ])?;
    assert!(matches!(
        acquire(&invalid_registry),
        Err(P1AuthorityError::Invalid(
            P1AuthorityKind::GeneratedIncludes
        ))
    ));

    let valid_registry = include_bytes!("../../../../policy/generated-includes.json");
    let current = snapshot(&[
        (PHASE_LOCK_PATH, EntryKind::Regular, lock.as_slice()),
        (GENERATED_INCLUDES_PATH, EntryKind::Regular, valid_registry),
    ])?;
    assert!(matches!(
        acquire(&current),
        Err(P1AuthorityError::ExactBase)
    ));
    Ok(())
}

#[test]
fn acquisition_errors_do_not_disclose_authority_bytes() -> TestResult {
    let sentinel = "norn-synthetic-private-authority-value";
    let current = snapshot(&[(PHASE_LOCK_PATH, EntryKind::Regular, sentinel.as_bytes())])?;
    let Some(error) = acquire(&current).err() else {
        return Err("invalid phase lock unexpectedly acquired".into());
    };
    assert!(!error.to_string().contains(sentinel));
    assert!(!format!("{error:?}").contains(sentinel));
    Ok(())
}

#[test]
fn responses_traceability_details_survive_the_authority_boundary() {
    for (issue, count) in [
        (EvidenceTraceabilityIssue::FindingMissing, 2_u64),
        (EvidenceTraceabilityIssue::SourceMismatch, 3_u64),
        (EvidenceTraceabilityIssue::EvidenceMissing, 5_u64),
    ] {
        let error =
            P1AuthorityError::from(ResponsesContractError::EvidenceTraceability { issue, count });
        assert!(matches!(
            error,
            P1AuthorityError::EvidenceTraceability {
                issue: observed_issue,
                count: observed_count,
            } if observed_issue == issue && observed_count == count
        ));
    }
}

#[test]
fn governance_anchor_failures_are_closed_and_non_disclosing() {
    let sentinel = "norn-synthetic-private-governance-value";
    for error in [
        P1AuthorityError::GovernanceAnchorLink,
        P1AuthorityError::GovernanceTransition,
        P1AuthorityError::Digest(P1AuthorityKind::GovernanceAnchor),
    ] {
        assert!(!error.to_string().contains(sentinel));
        assert!(!format!("{error:?}").contains(sentinel));
    }
}

#[test]
fn writer_resolution_authority_kind_has_stable_closed_errors() {
    let kind = P1AuthorityKind::WriterResolutions;
    assert_eq!(kind.as_str(), "writer_resolutions");
    assert_eq!(kind.to_string(), "writer resolutions");
    assert_eq!(
        P1AuthorityError::Missing(kind).to_string(),
        "required writer resolutions authority is missing"
    );
    assert_eq!(
        P1AuthorityError::NotRegular(kind).to_string(),
        "required writer resolutions authority is not a regular file"
    );
    assert_eq!(
        P1AuthorityError::Invalid(kind).to_string(),
        "writer resolutions authority is invalid"
    );
    assert_eq!(
        P1AuthorityError::Digest(kind).to_string(),
        "writer resolutions authority does not match the phase lock"
    );
}

fn lock_bytes() -> TestResult<Vec<u8>> {
    let zero = Digest::from_bytes([0_u8; 32]);
    Ok(serde_json::to_vec(&json!({
        "schema_version": 1,
        "active_phase": "P1",
        "base": {
            "object_format": "sha1",
            "commit": P1_BASE_COMMIT,
            "tree": P1_BASE_TREE,
        },
        "algorithms": {
            "analyzer": ANALYZER_VERSION,
            "digest": DIGEST_VERSION,
        },
        "digests": {
            "repository_policy": zero,
            "governance": zero,
            "governance_anchor": P1_GOVERNANCE_ANCHOR_IDENTITY,
            "writer_resolutions": zero,
            "writer_families": zero,
            "generated_include_registry": P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
            "contract_manifest": zero,
            "evidence_schemas": zero,
            "source_findings": zero,
            "origin": zero,
        },
        "gate": {
            "entrypoint_path": "scripts/p1-gate",
            "entrypoint_sha256": zero,
            "command_manifest_path": "policy/gate-commands.json",
            "command_manifest_sha256": zero,
        },
    }))?)
}

fn snapshot(entries: &[(&str, EntryKind, &[u8])]) -> TestResult<OwnedSnapshot> {
    let entries = entries
        .iter()
        .map(|(path, kind, bytes)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::copy_from_slice(*kind, bytes),
            ))
        })
        .collect::<Result<Vec<_>, norn_policy::RepositoryPathError>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn acquire(current: &OwnedSnapshot) -> Result<ReadyP1Authorities, P1AuthorityError> {
    let current = CompleteCurrentSnapshot::from_complete_snapshot(current.clone());
    let Ok(commit) = GitObjectId::parse(P1_BASE_COMMIT) else {
        return Err(P1AuthorityError::ExactBase);
    };
    let Ok(tree) = GitObjectId::parse(P1_BASE_TREE) else {
        return Err(P1AuthorityError::ExactBase);
    };
    let Ok(base) = P1BaseSnapshot::try_from_git_tree(commit, tree, std::iter::empty()) else {
        return Err(P1AuthorityError::ExactBase);
    };
    ReadyP1Authorities::acquire(P1EvaluationInput::new(&current, &base))
}
