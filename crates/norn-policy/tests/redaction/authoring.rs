use std::{error::Error, io};

use norn_policy::redaction::{
    RedactionAuthoringError, RedactionCode, RedactionRegistry, RedactionViolation,
    RegistryEncodeError, p1_evidence_tool_paths, validate_retained_artifacts,
};
use norn_policy::{OwnedSnapshot, RepositoryPath, SnapshotEntry};

use super::support::{fixture, protocol_bytes};

const TRACEABILITY_BYTES: &[u8] =
    include_bytes!("../../../../docs/reviews/evidence/p1/finding-traceability.jsonl");
const CONTRACT_BYTES: &[u8] =
    include_bytes!("../../../../policy/contracts/openai-responses-v1/manifest.json");

#[test]
fn registry_encoding_is_pretty_deterministic_and_round_trips() -> Result<(), Box<dyn Error>> {
    let registry = fixture()?.registry;
    let first = registry.encode_p1()?;
    let second = registry.encode_p1()?;
    assert_eq!(first, second);
    assert!(first.starts_with(b"{\n  \"schema_version\": 1,"));
    assert!(first.ends_with(b"\n"));
    assert!(!first.ends_with(b"\n\n"));
    let decoded = RedactionRegistry::decode_p1(&first)?;
    assert_eq!(decoded, registry);
    assert_eq!(decoded.digest(), registry.digest());
    assert_eq!(decoded.encode_p1()?, first);

    let empty = RedactionRegistry::new(Vec::new(), Vec::new())?;
    assert!(matches!(
        empty.encode_p1(),
        Err(RegistryEncodeError::EmptyAuthority)
    ));
    Ok(())
}

#[test]
fn checked_tree_authoring_binds_every_present_family_and_exact_bytes() -> Result<(), Box<dyn Error>>
{
    let protocol_path = RepositoryPath::parse(
        "crates/norn/testdata/openai_responses/public/requests/request.json",
    )?;
    let protocol = protocol_bytes()?;
    let mut entries = vec![
        (protocol_path.clone(), SnapshotEntry::regular(protocol)),
        (
            RepositoryPath::parse("docs/reviews/evidence/p1/finding-traceability.jsonl")?,
            SnapshotEntry::regular(TRACEABILITY_BYTES.to_vec()),
        ),
        (
            RepositoryPath::parse("policy/contracts/openai-responses-v1/manifest.json")?,
            SnapshotEntry::regular(CONTRACT_BYTES.to_vec()),
        ),
    ];
    entries.extend(tool_entries()?);
    let snapshot = OwnedSnapshot::try_from_entries(entries)?;

    let registry = RedactionRegistry::author_checked_tree_p1(&snapshot)?;
    assert_eq!(
        registry.registered_paths().len(),
        5 + p1_evidence_tool_paths().len()
    );
    let violations = validate_retained_artifacts(&registry, &snapshot);
    assert!(
        violations.is_empty(),
        "unexpected violations: {violations:?}"
    );

    let changed = OwnedSnapshot::try_from_entries(snapshot.iter().map(|(path, entry)| {
        let entry = if path == &protocol_path {
            SnapshotEntry::regular(b"{}".to_vec())
        } else {
            entry.clone()
        };
        (path.clone(), entry)
    }))?;
    let changed_codes = validate_retained_artifacts(&registry, &changed)
        .iter()
        .map(RedactionViolation::code)
        .collect::<Vec<_>>();
    assert!(changed_codes.contains(&RedactionCode::ArtifactDigestMismatch));
    Ok(())
}

#[test]
fn checked_tree_authoring_refuses_run_local_observation_authority() -> Result<(), Box<dyn Error>> {
    let mut entries = tool_entries()?;
    entries.push((
        RepositoryPath::parse("target/p1-gate/evidence/gate.json")?,
        SnapshotEntry::regular(b"{}".to_vec()),
    ));
    let snapshot = OwnedSnapshot::try_from_entries(entries)?;
    let result = RedactionRegistry::author_checked_tree_p1(&snapshot);
    assert!(matches!(
        result,
        Err(RedactionAuthoringError::RunLocalAuthorityRequired)
    ));
    Ok(())
}

#[test]
fn checked_tree_authoring_fails_closed_on_unknown_or_non_regular_entries()
-> Result<(), Box<dyn Error>> {
    let mut unknown_entries = tool_entries()?;
    unknown_entries.push((
        RepositoryPath::parse("docs/reviews/evidence/p1/unclassified.json")?,
        SnapshotEntry::regular(b"{}".to_vec()),
    ));
    let unknown = OwnedSnapshot::try_from_entries(unknown_entries)?;
    assert!(matches!(
        RedactionRegistry::author_checked_tree_p1(&unknown),
        Err(RedactionAuthoringError::UnclassifiedGovernedArtifact)
    ));

    let linked_path = p1_evidence_tool_paths()
        .next()
        .ok_or_else(|| io::Error::other("compiled evidence-tool inventory is empty"))?;
    let linked =
        OwnedSnapshot::try_from_entries(tool_entries()?.into_iter().map(|(path, entry)| {
            let entry = if path.as_str() == linked_path {
                SnapshotEntry::symlink(b"elsewhere".to_vec())
            } else {
                entry
            };
            (path, entry)
        }))?;
    assert!(matches!(
        RedactionRegistry::author_checked_tree_p1(&linked),
        Err(RedactionAuthoringError::NonRegularCheckedTreeArtifact)
    ));
    Ok(())
}

#[test]
fn checked_tree_authoring_rejects_missing_or_unregistered_tool_sources()
-> Result<(), Box<dyn Error>> {
    let missing = OwnedSnapshot::try_from_entries(tool_entries()?.into_iter().skip(1))?;
    assert!(matches!(
        RedactionRegistry::author_checked_tree_p1(&missing),
        Err(RedactionAuthoringError::MissingEvidenceToolSource)
    ));

    let mut entries = tool_entries()?;
    entries.push((
        RepositoryPath::parse("docs/reviews/evidence/p1/unreviewed_tool.py")?,
        SnapshotEntry::regular(b"print('unreviewed')\n".to_vec()),
    ));
    let unexpected = OwnedSnapshot::try_from_entries(entries)?;
    assert!(matches!(
        RedactionRegistry::author_checked_tree_p1(&unexpected),
        Err(RedactionAuthoringError::EvidenceToolInventoryMismatch)
    ));

    let mut script_entries = tool_entries()?;
    script_entries.push((
        RepositoryPath::parse("scripts/p1-hidden")?,
        SnapshotEntry::regular(b"exit 0\n".to_vec()),
    ));
    let unexpected_script = OwnedSnapshot::try_from_entries(script_entries)?;
    assert!(matches!(
        RedactionRegistry::author_checked_tree_p1(&unexpected_script),
        Err(RedactionAuthoringError::EvidenceToolInventoryMismatch)
    ));

    let dependency_entries = tool_entries()?
        .into_iter()
        .map(|(path, entry)| {
            let entry = if path.as_str() == "scripts/p1_origin_evidence.py" {
                SnapshotEntry::regular(b"from helper import value\n".to_vec())
            } else {
                entry
            };
            (path, entry)
        })
        .chain([(
            RepositoryPath::parse("scripts/helper.py")?,
            SnapshotEntry::regular(b"value = 1\n".to_vec()),
        )]);
    let unregistered_dependency = OwnedSnapshot::try_from_entries(dependency_entries)?;
    assert!(matches!(
        RedactionRegistry::author_checked_tree_p1(&unregistered_dependency),
        Err(RedactionAuthoringError::EvidenceToolInventoryMismatch)
    ));
    Ok(())
}

fn tool_entries() -> Result<Vec<(RepositoryPath, SnapshotEntry)>, Box<dyn Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    p1_evidence_tool_paths()
        .chain([
            "policy/gate-commands.json",
            "policy/evidence-schemas/gate-run.schema.json",
        ])
        .map(|raw_path| {
            Ok((
                RepositoryPath::parse(raw_path)?,
                SnapshotEntry::regular(std::fs::read(root.join(raw_path))?),
            ))
        })
        .collect()
}
