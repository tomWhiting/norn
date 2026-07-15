//! Strict repository-policy document tests.

use std::error::Error;

use norn_policy::config::{
    EnforcementMode, LocLimitKind, RepositoryPolicy, RepositoryPolicyError, RuleFamily,
};

fn valid_policy() -> &'static str {
    r#"schema_version = 1

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"

[production_loc]
entrypoint_max = 200
other_rust_max = 500
"#
}

#[test]
fn accepts_closed_hard_policy_and_exposes_typed_invariants() -> Result<(), Box<dyn Error>> {
    let policy = RepositoryPolicy::decode(valid_policy().as_bytes())?;

    assert_eq!(policy.schema_version(), 1);
    assert_eq!(policy.analyzer_version(), "norn-policy-1");
    assert_eq!(policy.digest_version(), "norn-sha256-canonical-json-1");
    assert_eq!(policy.enforcement_mode(), EnforcementMode::Hard);
    assert_eq!(policy.production_loc().entrypoint_max(), 200);
    assert_eq!(policy.production_loc().other_rust_max(), 500);
    assert_eq!(
        policy.required_rule_families(),
        &[
            RuleFamily::ProductionReachability,
            RuleFamily::GeneratedIncludes,
            RuleFamily::ProductionLoc,
            RuleFamily::ModuleShape,
            RuleFamily::ProhibitedDebt,
            RuleFamily::ProductionProjection,
            RuleFamily::OriginGovernance,
            RuleFamily::WriterInventory,
            RuleFamily::EvidenceRedaction,
            RuleFamily::EvidenceTraceability,
        ]
    );
    Ok(())
}

#[test]
fn rejects_duplicate_fields_at_root_and_nested_levels() {
    let duplicate_root = valid_policy().replacen(
        "schema_version = 1",
        "schema_version = 1\nschema_version = 1",
        1,
    );
    let duplicate_nested = valid_policy().replacen(
        "entrypoint_max = 200",
        "entrypoint_max = 200\nentrypoint_max = 199",
        1,
    );

    assert!(matches!(
        RepositoryPolicy::decode(duplicate_root.as_bytes()),
        Err(RepositoryPolicyError::Toml(_))
    ));
    assert!(matches!(
        RepositoryPolicy::decode(duplicate_nested.as_bytes()),
        Err(RepositoryPolicyError::Toml(_))
    ));
}

#[test]
fn rejects_unknown_fields_at_every_document_level() {
    let unknown_root = valid_policy().replacen(
        "schema_version = 1",
        "schema_version = 1\nexclusions = [\"crates/norn\"]",
        1,
    );
    let unknown_algorithm = valid_policy().replacen(
        "analyzer = \"norn-policy-1\"",
        "analyzer = \"norn-policy-1\"\nexecutable = \"cargo\"",
        1,
    );
    let unknown_limit = valid_policy().replacen(
        "entrypoint_max = 200",
        "entrypoint_max = 200\nadvisory = true",
        1,
    );

    for document in [unknown_root, unknown_algorithm, unknown_limit] {
        assert!(matches!(
            RepositoryPolicy::decode(document.as_bytes()),
            Err(RepositoryPolicyError::Toml(_))
        ));
    }
}

#[test]
fn rejects_non_utf8_malformed_toml_and_wrong_types() {
    assert!(matches!(
        RepositoryPolicy::decode(&[0xff]),
        Err(RepositoryPolicyError::Utf8(_))
    ));
    assert!(matches!(
        RepositoryPolicy::decode(b"schema_version = ["),
        Err(RepositoryPolicyError::Toml(_))
    ));

    let wrong_type = valid_policy().replacen("entrypoint_max = 200", "entrypoint_max = 1.5", 1);
    assert!(matches!(
        RepositoryPolicy::decode(wrong_type.as_bytes()),
        Err(RepositoryPolicyError::Toml(_))
    ));
}

#[test]
fn rejects_unknown_schema_and_algorithm_identities() {
    let schema = valid_policy().replacen("schema_version = 1", "schema_version = 2", 1);
    let analyzer = valid_policy().replacen("norn-policy-1", "norn-policy-2", 1);
    let digest = valid_policy().replacen(
        "norn-sha256-canonical-json-1",
        "norn-sha256-canonical-json-2",
        1,
    );

    assert!(matches!(
        RepositoryPolicy::decode(schema.as_bytes()),
        Err(RepositoryPolicyError::SchemaVersion { actual: 2 })
    ));
    assert!(matches!(
        RepositoryPolicy::decode(analyzer.as_bytes()),
        Err(RepositoryPolicyError::AnalyzerVersion)
    ));
    assert!(matches!(
        RepositoryPolicy::decode(digest.as_bytes()),
        Err(RepositoryPolicyError::DigestVersion)
    ));
}

#[test]
fn rejects_zero_or_loosened_limits() {
    let zero = valid_policy().replacen("entrypoint_max = 200", "entrypoint_max = 0", 1);
    let loose_entrypoint =
        valid_policy().replacen("entrypoint_max = 200", "entrypoint_max = 201", 1);
    let loose_other = valid_policy().replacen("other_rust_max = 500", "other_rust_max = 501", 1);

    assert!(matches!(
        RepositoryPolicy::decode(zero.as_bytes()),
        Err(RepositoryPolicyError::ZeroLimit {
            kind: LocLimitKind::Entrypoint
        })
    ));
    assert!(matches!(
        RepositoryPolicy::decode(loose_entrypoint.as_bytes()),
        Err(RepositoryPolicyError::LimitLoosened {
            kind: LocLimitKind::Entrypoint,
            actual: 201,
            maximum: 200
        })
    ));
    assert!(matches!(
        RepositoryPolicy::decode(loose_other.as_bytes()),
        Err(RepositoryPolicyError::LimitLoosened {
            kind: LocLimitKind::OtherRust,
            actual: 501,
            maximum: 500
        })
    ));
}

#[test]
fn rejects_attempts_to_disable_or_demote_rule_families() {
    let rules = r"

[rules]
prohibited_debt = false
";
    let enforcement = "\nenforcement = \"advisory\"\n";

    for suffix in [rules, enforcement] {
        let document = format!("{}{suffix}", valid_policy());
        assert!(matches!(
            RepositoryPolicy::decode(document.as_bytes()),
            Err(RepositoryPolicyError::Toml(_))
        ));
    }
}

#[test]
fn accepts_numeric_tightening() -> Result<(), Box<dyn Error>> {
    let document = valid_policy()
        .replacen("entrypoint_max = 200", "entrypoint_max = 150", 1)
        .replacen("other_rust_max = 500", "other_rust_max = 400", 1);
    let policy = RepositoryPolicy::decode(document.as_bytes())?;

    assert_eq!(policy.production_loc().entrypoint_max(), 150);
    assert_eq!(policy.production_loc().other_rust_max(), 400);
    Ok(())
}

#[test]
fn normalized_digest_is_deterministic_and_ignores_toml_key_order() -> Result<(), Box<dyn Error>> {
    let reordered = r#"schema_version = 1

[production_loc]
other_rust_max = 500
entrypoint_max = 200

[algorithms]
digest = "norn-sha256-canonical-json-1"
analyzer = "norn-policy-1"
"#;
    let first = RepositoryPolicy::decode(valid_policy().as_bytes())?;
    let second = RepositoryPolicy::decode(reordered.as_bytes())?;

    assert_eq!(first, second);
    assert_eq!(first.normalized_digest()?, second.normalized_digest()?);
    assert_eq!(first.normalized_digest()?, first.normalized_digest()?);
    assert_eq!(
        first.normalized_digest()?.to_string(),
        "bcd5d83ac26202c05579b6a6da8e5e02912c567823b1e7af9c00dffab385a155"
    );
    Ok(())
}
