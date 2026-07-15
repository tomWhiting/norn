//! Strict phase-lock schema and identity tests.

#[path = "phase_lock/authoring.rs"]
mod authoring;
#[path = "phase_lock/authority_verification.rs"]
mod authority_verification;

use norn_policy::baseline::{
    P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    P1_GOVERNANCE_ANCHOR_IDENTITY,
};
use norn_policy::phase_lock::{
    CampaignPhase, GitObjectFormat, GitObjectId, PhaseLock, PhaseLockError,
};

const ZERO_SHA1: &str = "0000000000000000000000000000000000000000";
const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const VALID_LOCK_JSON: &str = include_str!("evidence/p1_phase_lock_parity.json");

fn valid_lock_json() -> String {
    VALID_LOCK_JSON.to_owned()
}

#[test]
fn accepts_complete_p1_lock() -> Result<(), Box<dyn std::error::Error>> {
    let lock = PhaseLock::decode_p1(valid_lock_json().as_bytes())?;

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
    assert_eq!(lock.gate().entrypoint_sha256.to_string(), "7".repeat(64));
    let encoded = serde_json::to_vec(&lock)?;
    assert_eq!(PhaseLock::decode_p1(&encoded)?, lock);
    Ok(())
}

#[test]
fn rejects_sha256_as_an_alternate_p1_base() {
    let document = valid_lock_json()
        .replacen("\"sha1\"", "\"sha256\"", 1)
        .replacen(
            &format!("\"commit\": \"{P1_BASE_COMMIT}\""),
            &format!("\"commit\": \"{ZERO_SHA256}\""),
            1,
        )
        .replacen(
            &format!("\"tree\": \"{P1_BASE_TREE}\""),
            &format!("\"tree\": \"{ZERO_SHA256}\""),
            1,
        );

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::P1BaseObjectFormat)
    ));
}

#[test]
fn rejects_alternate_sha1_commit_and_tree() {
    let alternate_commit = valid_lock_json().replacen(P1_BASE_COMMIT, ZERO_SHA1, 1);
    let alternate_tree = valid_lock_json().replacen(P1_BASE_TREE, ZERO_SHA1, 1);

    assert!(matches!(
        PhaseLock::decode_p1(alternate_commit.as_bytes()),
        Err(PhaseLockError::P1BaseCommit)
    ));
    assert!(matches!(
        PhaseLock::decode_p1(alternate_tree.as_bytes()),
        Err(PhaseLockError::P1BaseTree)
    ));
}

#[test]
fn rejects_alternate_generated_registry_authority() {
    let document = valid_lock_json().replacen(
        &P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY.to_string(),
        ZERO_SHA256,
        1,
    );

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::P1GeneratedIncludeRegistry)
    ));
}

#[test]
fn rejects_caller_selected_governance_anchor() {
    let document =
        valid_lock_json().replacen(&P1_GOVERNANCE_ANCHOR_IDENTITY.to_string(), ZERO_SHA256, 1);

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::P1GovernanceAnchor)
    ));
}

#[test]
fn requires_explicit_governance_anchor_identity() {
    let document = valid_lock_json().replacen(
        &format!("    \"governance_anchor\": \"{P1_GOVERNANCE_ANCHOR_IDENTITY}\",\n"),
        "",
        1,
    );

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::Json)
    ));
}

#[test]
fn requires_distinct_writer_resolution_and_family_identities() {
    for field in ["writer_resolutions", "writer_families"] {
        let prefix = format!("    \"{field}\":");
        let document = valid_lock_json()
            .lines()
            .filter(|line| !line.starts_with(&prefix))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            PhaseLock::decode_p1(document.as_bytes()),
            Err(PhaseLockError::Json)
        ));
    }

    let ambiguous =
        valid_lock_json().replacen("    \"writer_resolutions\":", "    \"writer_registry\":", 1);
    assert!(matches!(
        PhaseLock::decode_p1(ambiguous.as_bytes()),
        Err(PhaseLockError::Json)
    ));
}

#[test]
fn rejects_alternate_gate_authority_paths() {
    let entrypoint = valid_lock_json().replacen("scripts/p1-gate", "scripts/other-gate", 1);
    let manifest =
        valid_lock_json().replacen("policy/gate-commands.json", "policy/other-commands.json", 1);

    assert!(matches!(
        PhaseLock::decode_p1(entrypoint.as_bytes()),
        Err(PhaseLockError::P1GateEntrypointPath)
    ));
    assert!(matches!(
        PhaseLock::decode_p1(manifest.as_bytes()),
        Err(PhaseLockError::P1GateCommandManifestPath)
    ));
}

#[test]
fn rejects_duplicate_member_before_schema_decode() {
    let document = valid_lock_json().replacen(
        "\"schema_version\": 1,",
        "\"schema_version\": 1, \"schema_version\": 1,",
        1,
    );

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::Json)
    ));
}

#[test]
fn rejects_unknown_member() {
    let document = valid_lock_json().replacen(
        "\"active_phase\": \"P1\",",
        "\"active_phase\": \"P1\", \"advisory\": true,",
        1,
    );

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::Json)
    ));
}

#[test]
fn rejects_algorithm_drift() {
    let analyzer = valid_lock_json().replacen("norn-policy-1", "norn-policy-2", 1);
    let digest = valid_lock_json().replacen(
        "norn-sha256-canonical-json-1",
        "norn-sha256-canonical-json-2",
        1,
    );

    assert!(matches!(
        PhaseLock::decode_p1(analyzer.as_bytes()),
        Err(PhaseLockError::AnalyzerVersion)
    ));
    assert!(matches!(
        PhaseLock::decode_p1(digest.as_bytes()),
        Err(PhaseLockError::DigestVersion)
    ));
}

#[test]
fn git_object_ids_are_complete_lowercase_hex() -> Result<(), Box<dyn std::error::Error>> {
    let sha1 = GitObjectId::parse(ZERO_SHA1)?;
    let sha256 = GitObjectId::parse(ZERO_SHA256)?;

    assert_eq!(sha1.as_str().len(), 40);
    assert_eq!(sha1.object_format(), GitObjectFormat::Sha1);
    assert_eq!(sha256.as_str().len(), 64);
    assert_eq!(sha256.object_format(), GitObjectFormat::Sha256);
    assert!(GitObjectId::parse("A000000000000000000000000000000000000000").is_err());
    assert!(GitObjectId::parse("abc").is_err());
    Ok(())
}

#[test]
fn rejects_commit_or_tree_outside_the_declared_object_format() {
    let mixed = valid_lock_json().replacen(
        &format!("\"tree\": \"{P1_BASE_TREE}\""),
        &format!("\"tree\": \"{ZERO_SHA256}\""),
        1,
    );
    let false_declaration = valid_lock_json().replacen("\"sha1\"", "\"sha256\"", 1);

    for document in [mixed, false_declaration] {
        assert!(matches!(
            PhaseLock::decode_p1(document.as_bytes()),
            Err(PhaseLockError::GitObjectFormat)
        ));
    }
}

#[test]
fn requires_an_explicit_git_object_format() {
    let document = valid_lock_json().replacen("    \"object_format\": \"sha1\",\n", "", 1);

    assert!(matches!(
        PhaseLock::decode_p1(document.as_bytes()),
        Err(PhaseLockError::Json)
    ));
}
