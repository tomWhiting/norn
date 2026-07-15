//! Exact P1 base and generated-registry authority tests.

use std::error::Error;
use std::io;

use norn_policy::baseline::{
    ExactP1Base, ExactP1BaseError, P1_BASE_ANALYSIS_SNAPSHOT_IDENTITY, P1_BASE_COMMIT,
    P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY, generated_registry_technical_identity,
};
use norn_policy::rust::modules::GeneratedIncludeRegistry;
use norn_policy::strict_json::decode_strict_json;
use norn_policy::{GitObjectId, P1_BASE_GIT_INVENTORY_IDENTITY, P1BaseSnapshot};
use serde_json::Value;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
fn retained_evidence_matches_compiled_authorities() -> TestResult {
    let evidence = retained_evidence()?;
    let analysis_snapshot_identity = P1_BASE_ANALYSIS_SNAPSHOT_IDENTITY.to_string();
    let generated_registry_identity = P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY.to_string();
    let git_inventory_identity = P1_BASE_GIT_INVENTORY_IDENTITY.to_string();
    assert_eq!(
        evidence
            .get("git_inventory_identity")
            .and_then(Value::as_str),
        Some(git_inventory_identity.as_str())
    );
    assert_eq!(
        evidence
            .get("analysis_snapshot_identity")
            .and_then(Value::as_str),
        Some(analysis_snapshot_identity.as_str())
    );
    assert_eq!(
        evidence
            .get("generated_include_registry_identity")
            .and_then(Value::as_str),
        Some(generated_registry_identity.as_str())
    );

    let registry = evidence_registry(&evidence)?;
    let checked_registry: GeneratedIncludeRegistry =
        decode_strict_json(include_bytes!("../../../policy/generated-includes.json"))?;
    assert_eq!(registry, checked_registry);
    assert_eq!(
        generated_registry_technical_identity(&registry)?,
        P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY
    );
    let base = P1BaseSnapshot::try_from_git_tree(
        GitObjectId::parse(P1_BASE_COMMIT)?,
        GitObjectId::parse(P1_BASE_TREE)?,
        std::iter::empty(),
    )?;
    assert!(matches!(
        ExactP1Base::acquire(&base, &registry),
        Err(ExactP1BaseError::GitIdentity)
    ));
    Ok(())
}

#[test]
fn registry_identity_binds_every_nested_technical_field() -> TestResult {
    let evidence = retained_evidence()?;
    let baseline_value = evidence
        .get("generated_include_registry")
        .cloned()
        .ok_or_else(|| missing("generated registry"))?;
    let baseline = serde_json::from_value::<GeneratedIncludeRegistry>(baseline_value.clone())?;
    let expected = generated_registry_technical_identity(&baseline)?;
    let replacements = [
        ("/schema_version", serde_json::json!(2)),
        (
            "/entries/0/source",
            serde_json::json!("crates/norn/src/other.rs"),
        ),
        ("/entries/0/callsite/start", serde_json::json!(7868)),
        ("/entries/0/callsite/end", serde_json::json!(7931)),
        ("/entries/0/enclosing_item/start", serde_json::json!(7726)),
        ("/entries/0/enclosing_item/end", serde_json::json!(7934)),
        (
            "/entries/0/invocation_digest",
            serde_json::json!("0000000000000000000000000000000000000000000000000000000000000000"),
        ),
        ("/entries/0/target/package", serde_json::json!("other")),
        ("/entries/0/target/package_root", Value::Null),
        ("/entries/0/target/kind", serde_json::json!("binary")),
        ("/entries/0/target/name", serde_json::json!("other")),
        (
            "/entries/0/target/root",
            serde_json::json!("crates/norn/src/main.rs"),
        ),
        (
            "/entries/0/generator/path",
            serde_json::json!("crates/norn/other.rs"),
        ),
        (
            "/entries/0/generator/digest",
            serde_json::json!("1111111111111111111111111111111111111111111111111111111111111111"),
        ),
        (
            "/entries/0/inputs/0/path",
            serde_json::json!("assets/other.json"),
        ),
        (
            "/entries/0/inputs/0/digest",
            serde_json::json!("2222222222222222222222222222222222222222222222222222222222222222"),
        ),
        (
            "/entries/0/output_basename",
            serde_json::json!("other_generated.rs"),
        ),
    ];
    for (pointer, replacement) in replacements {
        let mut changed = baseline_value.clone();
        let field = changed
            .pointer_mut(pointer)
            .ok_or_else(|| missing("registry field"))?;
        *field = replacement;
        let registry = serde_json::from_value::<GeneratedIncludeRegistry>(changed)?;
        assert_ne!(generated_registry_technical_identity(&registry)?, expected);
    }

    let empty = GeneratedIncludeRegistry::empty();
    assert_ne!(generated_registry_technical_identity(&empty)?, expected);
    let mut additional = baseline;
    let extra = additional
        .entries
        .first()
        .cloned()
        .ok_or_else(|| missing("registry entry"))?;
    additional.entries.push(extra);
    assert_ne!(
        generated_registry_technical_identity(&additional)?,
        expected
    );

    let mut forward = additional;
    let second = forward
        .entries
        .get_mut(1)
        .ok_or_else(|| missing("second registry entry"))?;
    second.source = "crates/norn/src/other.rs".parse()?;
    let mut reversed = forward.clone();
    reversed.entries.reverse();
    assert_ne!(
        generated_registry_technical_identity(&forward)?,
        generated_registry_technical_identity(&reversed)?
    );
    Ok(())
}

#[test]
fn removed_inert_registry_labels_are_rejected_as_unknown() -> TestResult {
    let evidence = retained_evidence()?;
    let baseline = evidence
        .get("generated_include_registry")
        .cloned()
        .ok_or_else(|| missing("generated registry"))?;
    for field in ["owner", "output_schema"] {
        let mut changed = baseline.clone();
        let entry = changed
            .pointer_mut("/entries/0")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| missing("registry entry"))?;
        entry.insert(field.to_owned(), Value::String("inert-label".to_owned()));
        assert!(serde_json::from_value::<GeneratedIncludeRegistry>(changed).is_err());
    }
    Ok(())
}

fn evidence_registry(evidence: &Value) -> TestResult<GeneratedIncludeRegistry> {
    let value = evidence
        .get("generated_include_registry")
        .cloned()
        .ok_or_else(|| missing("generated registry"))?;
    Ok(serde_json::from_value(value)?)
}

fn retained_evidence() -> TestResult<Value> {
    Ok(decode_strict_json(include_bytes!(
        "evidence/p1_base_authority.json"
    ))?)
}

fn missing(name: &str) -> io::Error {
    io::Error::other(format!("retained evidence is missing {name}"))
}
