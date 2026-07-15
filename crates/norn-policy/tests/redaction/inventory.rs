use std::error::Error;

use norn_policy::finding::EvidenceRedactionIssue;
use norn_policy::redaction::{RedactionCode, validate_retained_artifacts};
use norn_policy::{FindingCode, OwnedSnapshot, RepositoryPath, SnapshotEntry};

use super::support::{add, assert_has, codes, fixture, remove, replace};

#[test]
fn accepts_complete_exact_regular_inventory() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let violations = validate_retained_artifacts(&fixture.registry, &fixture.snapshot);
    assert!(
        violations.is_empty(),
        "unexpected violations: {violations:?}"
    );
    Ok(())
}

#[test]
fn fixed_roots_detect_missing_and_extra_artifacts() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let missing = remove(&fixture.snapshot, &fixture.paths.protocol)?;
    assert_has(
        &codes(&fixture.registry, &missing),
        RedactionCode::RegisteredArtifactMissing,
    );

    let extra_path = RepositoryPath::parse("docs/reviews/evidence/p1/extra.json")?;
    let extra = add(
        &fixture.snapshot,
        extra_path,
        SnapshotEntry::regular(b"{}".to_vec()),
    )?;
    assert_has(
        &codes(&fixture.registry, &extra),
        RedactionCode::UnregisteredArtifact,
    );
    Ok(())
}

#[test]
fn rejects_links_other_entries_and_changed_bytes() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let linked = replace(
        &fixture.snapshot,
        &fixture.paths.log,
        SnapshotEntry::symlink(b"elsewhere".to_vec()),
    )?;
    let linked_codes = codes(&fixture.registry, &linked);
    assert_has(&linked_codes, RedactionCode::NonRegularArtifact);
    assert_has(&linked_codes, RedactionCode::ReferencedArtifactNonRegular);

    let other = replace(
        &fixture.snapshot,
        &fixture.paths.protocol,
        SnapshotEntry::other(Vec::<u8>::new()),
    )?;
    assert_has(
        &codes(&fixture.registry, &other),
        RedactionCode::NonRegularArtifact,
    );

    let changed = replace(
        &fixture.snapshot,
        &fixture.paths.log,
        SnapshotEntry::regular(b"tests=19 passed=19 failed=0\n".to_vec()),
    )?;
    let changed_codes = codes(&fixture.registry, &changed);
    assert_has(&changed_codes, RedactionCode::ArtifactDigestMismatch);
    assert_has(
        &changed_codes,
        RedactionCode::ReferencedArtifactDigestMismatch,
    );
    Ok(())
}

#[test]
fn governed_root_replacement_is_not_outside_redaction_authority() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let root = RepositoryPath::parse("docs/reviews/evidence/p1")?;
    let snapshot =
        OwnedSnapshot::try_from_entries([(root, SnapshotEntry::symlink(b"elsewhere".to_vec()))])?;
    let observed = codes(&fixture.registry, &snapshot);

    assert_has(&observed, RedactionCode::NonRegularArtifact);
    assert_has(&observed, RedactionCode::RegisteredArtifactMissing);
    Ok(())
}

#[test]
fn sensitive_unregistered_filename_never_enters_output() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let secret = ["acc", "ount-sk", "-fixture-value.json"].concat();
    let path = RepositoryPath::parse(format!("docs/reviews/evidence/p1/{secret}"))?;
    let snapshot = add(
        &fixture.snapshot,
        path,
        SnapshotEntry::regular(b"{}".to_vec()),
    )?;
    let violations = validate_retained_artifacts(&fixture.registry, &snapshot);
    let debug = format!("{violations:?}");
    let serialized = serde_json::to_string(&violations)?;
    assert!(!debug.contains(&secret));
    assert!(!serialized.contains(&secret));
    assert!(
        violations
            .iter()
            .any(|violation| violation.code() == RedactionCode::UnregisteredArtifact)
    );
    let Some(violation) = violations
        .iter()
        .find(|violation| violation.code() == RedactionCode::UnregisteredArtifact)
    else {
        return Err("missing unregistered-artifact violation".into());
    };
    let finding = violation.into_finding();
    assert_eq!(finding.code(), FindingCode::EvidenceRedaction);
    assert_eq!(finding.path(), None);
    assert_eq!(finding.artifact(), Some(violation.artifact()));
    assert_eq!(
        finding.evidence_redaction_issue(),
        Some(EvidenceRedactionIssue::UnregisteredArtifact)
    );
    let finding_debug = format!("{finding:?}");
    let finding_json = serde_json::to_string(&finding)?;
    assert!(!finding_debug.contains(&secret));
    assert!(!finding_json.contains(&secret));
    assert_eq!(violation.artifact().path_digest(), None);
    let finding_value = serde_json::to_value(&finding)?;
    assert!(
        finding_value["location"]["artifact"]
            .get("path_digest")
            .is_none()
    );
    Ok(())
}

#[test]
fn unknown_path_does_not_renumber_registered_artifact_identity() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let changed = replace(
        &fixture.snapshot,
        &fixture.paths.contract,
        SnapshotEntry::regular(b"{}".to_vec()),
    )?;
    let baseline_identity = validate_retained_artifacts(&fixture.registry, &changed)
        .into_iter()
        .find(|violation| violation.code() == RedactionCode::ArtifactDigestMismatch)
        .ok_or("missing registered digest violation")?
        .artifact();

    let unknown = RepositoryPath::parse("docs/reviews/evidence/p1/000-unknown.json")?;
    let with_unknown = add(&changed, unknown, SnapshotEntry::regular(b"{}".to_vec()))?;
    let shifted_identity = validate_retained_artifacts(&fixture.registry, &with_unknown)
        .into_iter()
        .find(|violation| violation.code() == RedactionCode::ArtifactDigestMismatch)
        .ok_or("missing registered digest violation after unknown insertion")?
        .artifact();

    assert_eq!(baseline_identity, shifted_identity);
    assert!(baseline_identity.path_digest().is_some());
    Ok(())
}

#[test]
fn exact_p1_scripts_are_governed_without_claiming_the_whole_scripts_tree()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    for (path, bytes) in [
        ("scripts/p1-gate", b"exit 0\n".as_slice()),
        (
            "scripts/p1_origin_evidence.py",
            b"raise SystemExit(0)\n".as_slice(),
        ),
        (
            "scripts/test_p1_origin_evidence.py",
            b"raise SystemExit(0)\n".as_slice(),
        ),
    ] {
        let governed = add(
            &fixture.snapshot,
            RepositoryPath::parse(path)?,
            SnapshotEntry::regular(bytes.to_vec()),
        )?;
        assert_has(
            &codes(&fixture.registry, &governed),
            RedactionCode::UnregisteredArtifact,
        );
    }

    let unrelated = add(
        &fixture.snapshot,
        RepositoryPath::parse("scripts/unrelated-helper")?,
        SnapshotEntry::regular(b"exit 0\n".to_vec()),
    )?;
    let observed = codes(&fixture.registry, &unrelated);
    assert!(observed.is_empty(), "unexpected codes: {observed:?}");

    let manifest = add(
        &fixture.snapshot,
        RepositoryPath::parse("policy/gate-commands.json")?,
        SnapshotEntry::regular(b"{}".to_vec()),
    )?;
    assert_has(
        &codes(&fixture.registry, &manifest),
        RedactionCode::UnregisteredArtifact,
    );
    let base_authority = add(
        &fixture.snapshot,
        RepositoryPath::parse("crates/norn-policy/tests/evidence/p1_base_authority.json")?,
        SnapshotEntry::regular(b"{}".to_vec()),
    )?;
    assert_has(
        &codes(&fixture.registry, &base_authority),
        RedactionCode::UnregisteredArtifact,
    );
    let unrelated_policy = add(
        &fixture.snapshot,
        RepositoryPath::parse("policy/unrelated.json")?,
        SnapshotEntry::regular(b"{}".to_vec()),
    )?;
    let observed = codes(&fixture.registry, &unrelated_policy);
    assert!(observed.is_empty(), "unexpected codes: {observed:?}");
    Ok(())
}
