//! Role and exact-Git-inventory tests for P1 evaluation inputs.

use std::error::Error;

use norn_policy::baseline::{P1_BASE_COMMIT, P1_BASE_TREE};
use norn_policy::phase_lock::{P1AuthorityError, P1AuthorityKind};
use norn_policy::snapshot::{MutationProposal, SnapshotMutation};
use norn_policy::{
    AuthorityIssue, CompleteCurrentSnapshot, EntryKind, GitLeafMode, GitObjectId, GitTreeLeaf,
    GitTreeLeafError, OwnedSnapshot, P1BaseSnapshot, P1EvaluationInput, PolicyAuthority,
    PolicyInvalidReason, PolicyState, RepositoryPath, SnapshotEntry, evaluate_p1,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const EMPTY_BLOB_ID: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

#[test]
fn git_leaf_requires_exact_mode_kind_and_blob_identity() -> TestResult {
    let path = RepositoryPath::parse("src/lib.rs")?;
    let object_id = GitObjectId::parse(EMPTY_BLOB_ID)?;
    assert!(matches!(
        GitTreeLeaf::new(
            path.clone(),
            GitLeafMode::Regular,
            object_id,
            SnapshotEntry::symlink(Vec::<u8>::new()),
        ),
        Err(GitTreeLeafError::EntryKind)
    ));
    assert!(matches!(
        GitTreeLeaf::new(
            path.clone(),
            GitLeafMode::Regular,
            GitObjectId::parse("0000000000000000000000000000000000000000")?,
            SnapshotEntry::regular(Vec::<u8>::new()),
        ),
        Err(GitTreeLeafError::ObjectIdentity)
    ));
    assert!(matches!(
        GitTreeLeaf::new(
            path,
            GitLeafMode::Regular,
            GitObjectId::parse("0000000000000000000000000000000000000000000000000000000000000000",)?,
            SnapshotEntry::regular(Vec::<u8>::new()),
        ),
        Err(GitTreeLeafError::ObjectFormat)
    ));
    Ok(())
}

#[test]
fn git_inventory_identity_retains_executable_mode() -> TestResult {
    let regular = base_with_mode(GitLeafMode::Regular)?;
    let executable = base_with_mode(GitLeafMode::Executable)?;
    assert_ne!(
        regular.git_inventory_identity(),
        executable.git_inventory_identity()
    );
    assert_eq!(regular.snapshot(), executable.snapshot());
    Ok(())
}

#[test]
fn deleting_a_previously_observed_marker_cannot_become_absent() -> TestResult {
    let marker = RepositoryPath::parse("policy/phase-lock.json")?;
    let snapshot = OwnedSnapshot::try_from_entries([(
        marker.clone(),
        SnapshotEntry::new(EntryKind::Regular, b"{}".to_vec()),
    )])?;
    let current = CompleteCurrentSnapshot::from_complete_snapshot(snapshot);
    let proposal = MutationProposal::try_from_mutations([SnapshotMutation::delete(marker)])?;
    let staged = current.overlay(&proposal)?;
    let base = empty_base()?;
    let state = evaluate_p1(P1EvaluationInput::new(&staged, &base));
    assert!(matches!(
        state,
        PolicyState::Invalid(ref invalid)
            if matches!(
                invalid.reason(),
                PolicyInvalidReason::Authority {
                    authority: Some(PolicyAuthority::PhaseLock),
                    issue: AuthorityIssue::Missing,
                }
            )
    ));
    Ok(())
}

#[test]
fn direct_authority_acquisition_keeps_missing_marker_typed() -> TestResult {
    let current = CompleteCurrentSnapshot::from_complete_snapshot(OwnedSnapshot::empty());
    let base = empty_base()?;
    assert!(matches!(
        norn_policy::phase_lock::ReadyP1Authorities::acquire(P1EvaluationInput::new(
            &current, &base,
        )),
        Err(P1AuthorityError::Missing(P1AuthorityKind::PhaseLock))
    ));
    Ok(())
}

fn base_with_mode(mode: GitLeafMode) -> TestResult<P1BaseSnapshot> {
    let leaf = GitTreeLeaf::new(
        RepositoryPath::parse("src/lib.rs")?,
        mode,
        GitObjectId::parse(EMPTY_BLOB_ID)?,
        SnapshotEntry::regular(Vec::<u8>::new()),
    )?;
    Ok(P1BaseSnapshot::try_from_git_tree(
        GitObjectId::parse(P1_BASE_COMMIT)?,
        GitObjectId::parse(P1_BASE_TREE)?,
        [leaf],
    )?)
}

fn empty_base() -> TestResult<P1BaseSnapshot> {
    Ok(P1BaseSnapshot::try_from_git_tree(
        GitObjectId::parse(P1_BASE_COMMIT)?,
        GitObjectId::parse(P1_BASE_TREE)?,
        std::iter::empty(),
    )?)
}
