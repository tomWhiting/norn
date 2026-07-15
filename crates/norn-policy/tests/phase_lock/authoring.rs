//! Deterministic P1 phase-lock authoring tests.

use norn_policy::Digest;
use norn_policy::baseline::{
    P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    P1_GOVERNANCE_ANCHOR_IDENTITY,
};
use norn_policy::phase_lock::{
    CampaignPhase, GitObjectFormat, P1GateByteDigests, P1PhaseLockAuthoringInput,
    P1ReviewedAuthorityDigests, PhaseLock,
};

const EXPECTED_LOCK: &[u8] = include_bytes!("../evidence/p1_phase_lock_parity.json");

fn digest(byte: u8) -> Digest {
    Digest::from_bytes([byte; 32])
}

fn input() -> P1PhaseLockAuthoringInput {
    P1PhaseLockAuthoringInput {
        authorities: P1ReviewedAuthorityDigests {
            repository_policy: digest(0x00),
            governance: digest(0x11),
            writer_resolutions: digest(0x22),
            writer_families: digest(0x23),
            contract_manifest: digest(0x33),
            evidence_schemas: digest(0x44),
            source_findings: digest(0x55),
            origin: digest(0x66),
        },
        gate: P1GateByteDigests {
            entrypoint_sha256: digest(0x77),
            command_manifest_sha256: digest(0x88),
        },
    }
}

#[test]
fn authoring_uses_only_compiled_fixed_identities() -> Result<(), Box<dyn std::error::Error>> {
    let lock = PhaseLock::author_p1(input())?;

    assert_eq!(lock.active_phase(), CampaignPhase::P1);
    assert_eq!(lock.base().object_format, GitObjectFormat::Sha1);
    assert_eq!(lock.base().commit.as_str(), P1_BASE_COMMIT);
    assert_eq!(lock.base().tree.as_str(), P1_BASE_TREE);
    assert_eq!(
        lock.digests().generated_include_registry,
        P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY
    );
    assert_eq!(
        lock.digests().governance_anchor,
        P1_GOVERNANCE_ANCHOR_IDENTITY
    );
    assert_eq!(lock.gate().entrypoint_path.as_str(), "scripts/p1-gate");
    assert_eq!(
        lock.gate().command_manifest_path.as_str(),
        "policy/gate-commands.json"
    );
    Ok(())
}

#[test]
fn pretty_encoding_is_exact_and_strictly_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let lock = PhaseLock::author_p1(input())?;
    let encoded = lock.encode_p1_pretty()?;

    assert_eq!(encoded, EXPECTED_LOCK);
    assert_eq!(encoded.last(), Some(&b'\n'));
    assert!(!encoded.ends_with(b"\n\n"));
    assert_eq!(PhaseLock::decode_p1(&encoded)?, lock);
    Ok(())
}

#[test]
fn caller_digests_are_preserved_without_selecting_fixed_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let supplied = input();
    let lock = PhaseLock::author_p1(supplied)?;

    assert_eq!(
        lock.digests().repository_policy,
        supplied.authorities.repository_policy
    );
    assert_eq!(lock.digests().governance, supplied.authorities.governance);
    assert_eq!(
        lock.digests().writer_resolutions,
        supplied.authorities.writer_resolutions
    );
    assert_eq!(
        lock.digests().writer_families,
        supplied.authorities.writer_families
    );
    assert_eq!(
        lock.digests().contract_manifest,
        supplied.authorities.contract_manifest
    );
    assert_eq!(
        lock.digests().evidence_schemas,
        supplied.authorities.evidence_schemas
    );
    assert_eq!(
        lock.digests().source_findings,
        supplied.authorities.source_findings
    );
    assert_eq!(lock.digests().origin, supplied.authorities.origin);
    assert_eq!(
        lock.gate().entrypoint_sha256,
        supplied.gate.entrypoint_sha256
    );
    assert_eq!(
        lock.gate().command_manifest_sha256,
        supplied.gate.command_manifest_sha256
    );
    Ok(())
}
