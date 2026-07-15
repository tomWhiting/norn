use norn_policy::baseline::{
    ItemGroupError, OriginAuthorityError, OriginError, OriginLedger,
    P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY, ProductionFactError, ProductionLocClass,
};
use norn_policy::rust::modules::CompileTestExpectation;

use super::support::{
    TestResult, baseline_from_manifest, baseline_from_sources, decoded_origin_fixture, digest,
    origin,
};

#[test]
fn round_trips_strict_origin_and_hashes_normalized_content() -> TestResult {
    let first = origin()?;
    let bytes = first.encode_p1()?;
    let second = OriginLedger::decode_p1(&bytes)?;

    assert!(bytes.ends_with(b"\n"));
    assert!(!bytes.ends_with(b"\n\n"));
    assert_eq!(bytes, first.encode_p1()?);
    assert_eq!(first, second);
    assert_eq!(first.normalized_digest()?, second.normalized_digest()?);
    assert_eq!(second.production_files().len(), 2);
    assert!(!second.item_groups().is_empty());
    let legacy = second
        .production_files()
        .iter()
        .find(|fact| fact.path().as_str() == "src/legacy.rs")
        .ok_or_else(|| super::support::missing("legacy production fact"))?;
    assert_eq!(legacy.loc_class(), ProductionLocClass::Other);
    assert_eq!(
        second.base_commit().as_str(),
        "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d"
    );
    assert_eq!(
        second.base_tree().as_str(),
        "9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582"
    );
    assert_eq!(
        second.generated_include_registry_digest(),
        P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY
    );
    Ok(())
}

#[test]
fn strict_origin_round_trip_retains_compile_fixture_provenance() -> TestResult {
    let manifest = concat!(
        "[workspace]\n",
        "[package]\nname = \"sample\"\nedition = \"2024\"\nbuild = false\n",
        "[dev-dependencies]\ntrybuild = \"1\"\n",
    );
    let lock = r#"
version = 4

[[package]]
name = "sample"
version = "0.0.0"
dependencies = ["trybuild"]

[[package]]
name = "trybuild"
version = "1.0.117"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9"
"#;
    let harness = r#"
#[test]
fn ui() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
"#;
    let baseline = baseline_from_manifest(
        manifest,
        &[
            ("Cargo.lock", lock),
            ("src/lib.rs", ""),
            ("tests/harness.rs", harness),
            ("tests/ui/first.rs", "fn main() {}"),
            ("tests/ui/second.rs", "fn main() {}"),
        ],
    )?;
    let ledger = decoded_origin_fixture(digest(0x19), &baseline)?;
    assert_eq!(ledger.compile_test_fixtures().len(), 2);
    assert!(ledger.compile_test_fixtures().iter().all(|fixture| {
        fixture.expectation == CompileTestExpectation::CompileFail
            && fixture.harness.root.as_str() == "tests/harness.rs"
    }));
    let encoded = ledger.encode_p1()?;
    assert_eq!(OriginLedger::decode_p1(&encoded)?, ledger);

    let mut duplicate = serde_json::to_value(&ledger)?;
    let fixtures = duplicate
        .get_mut("compile_test_fixtures")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| super::support::missing("serialized compile-test fixtures"))?;
    let first_path = fixtures
        .first()
        .and_then(|fixture| fixture.get("path"))
        .cloned()
        .ok_or_else(|| super::support::missing("first compile-test fixture path"))?;
    let second_path = fixtures
        .get_mut(1)
        .and_then(|fixture| fixture.get_mut("path"))
        .ok_or_else(|| super::support::missing("second compile-test fixture path"))?;
    *second_path = first_path;
    assert!(matches!(
        OriginLedger::decode_p1(&serde_json::to_vec(&duplicate)?),
        Err(OriginError::CompileTestFixtureOrder { .. })
    ));
    Ok(())
}

#[test]
fn decoder_recomputes_item_identity_and_rejects_empty_aggregate() -> TestResult {
    let ledger = origin()?;
    let serialized = serde_json::to_string(&ledger)?;
    let item_id = ledger.item_groups()[0].origin_id().digest().to_string();
    let forged = serialized.replacen(
        &item_id,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );
    assert!(matches!(
        OriginLedger::decode_p1(forged.as_bytes()),
        Err(OriginError::ItemGroupId { .. })
    ));
    let forged_count = serialized.replacen("\"production_count\":1", "\"production_count\":2", 1);
    assert!(matches!(
        OriginLedger::decode_p1(forged_count.as_bytes()),
        Err(OriginError::ItemGroupId { .. })
    ));

    let mut value = serde_json::to_value(&ledger)?;
    let group = value
        .get_mut("item_groups")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|groups| groups.first_mut())
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| super::support::missing("serialized item group"))?;
    group.insert("production_count".to_owned(), serde_json::json!(0));
    group.insert("test_only_count".to_owned(), serde_json::json!(0));
    assert!(matches!(
        OriginLedger::decode_p1(&serde_json::to_vec(&value)?),
        Err(OriginError::ItemGroup {
            source: ItemGroupError::Empty,
            ..
        })
    ));
    Ok(())
}

#[test]
fn item_groups_are_normalized_and_strictly_decoded() -> TestResult {
    let ledger = origin()?;
    let mut value = serde_json::to_value(&ledger)?;
    let groups = value
        .get_mut("item_groups")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| super::support::missing("serialized item groups"))?;
    groups.swap(0, 1);
    assert!(matches!(
        OriginLedger::decode_p1(&serde_json::to_vec(&value)?),
        Err(OriginError::ItemGroupOrder { .. })
    ));

    let mut value = serde_json::to_value(&ledger)?;
    let groups = value
        .get_mut("item_groups")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| super::support::missing("serialized item groups"))?;
    let duplicate = groups
        .first()
        .cloned()
        .ok_or_else(|| super::support::missing("serialized item group"))?;
    groups.insert(1, duplicate);
    assert!(matches!(
        OriginLedger::decode_p1(&serde_json::to_vec(&value)?),
        Err(OriginError::ItemGroupOrder { .. })
    ));
    Ok(())
}

#[test]
fn decoder_recomputes_target_set_identity_and_semantic_loc_class() -> TestResult {
    let ledger = origin()?;
    let serialized = serde_json::to_string(&ledger)?;
    let target_identity = ledger.production_files()[0]
        .target_set_identity()
        .to_string();
    let forged_target_identity = serialized.replacen(
        &target_identity,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );
    let forged_loc_class = serialized.replacen(
        "\"loc_class\":\"other\"",
        "\"loc_class\":\"thin_entrypoint\"",
        1,
    );

    assert!(matches!(
        OriginLedger::decode_p1(forged_target_identity.as_bytes()),
        Err(OriginError::ProductionFact {
            source: ProductionFactError::TargetSetIdentity,
            ..
        })
    ));
    assert!(matches!(
        OriginLedger::decode_p1(forged_loc_class.as_bytes()),
        Err(OriginError::ProductionFact {
            source: ProductionFactError::LocClass,
            ..
        })
    ));

    let mut value = serde_json::to_value(&ledger)?;
    let production = value
        .get_mut("production_files")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|rows| rows.first_mut())
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| super::support::missing("serialized production fact"))?;
    let current_loc = production
        .get("production_loc")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| super::support::missing("serialized production LOC"))?;
    production.insert(
        "production_loc".to_owned(),
        serde_json::Value::from(current_loc + 1),
    );
    assert!(matches!(
        OriginLedger::decode_p1(&serde_json::to_vec(&value)?),
        Err(OriginError::ProductionId { .. })
    ));
    Ok(())
}

#[test]
fn decoder_rejects_unsorted_serialized_target_set() -> TestResult {
    let manifest = r#"[workspace]
[package]
name = "sample"
edition = "2024"
build = false
autolib = false
autobins = false

[lib]
path = "src/lib_root.rs"

[[bin]]
name = "sample-bin"
path = "src/bin_root.rs"
"#;
    let baseline = baseline_from_manifest(
        manifest,
        &[
            ("src/lib_root.rs", "mod shared;\n"),
            ("src/bin_root.rs", "mod shared;\nfn main() {}\n"),
            ("src/shared.rs", "pub fn shared() {}\n"),
        ],
    )?;
    let ledger = decoded_origin_fixture(digest(1), &baseline)?;
    let mut value = serde_json::to_value(&ledger)?;
    let target_rows = value
        .get_mut("production_files")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|rows| {
            rows.iter_mut().find(|row| {
                row.get("path").and_then(serde_json::Value::as_str) == Some("src/shared.rs")
            })
        })
        .and_then(|row| row.get_mut("targets"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| super::support::missing("serialized target rows"))?;
    target_rows.swap(0, 1);
    let bytes = serde_json::to_vec(&value)?;

    assert!(matches!(
        OriginLedger::decode_p1(&bytes),
        Err(OriginError::ProductionFact {
            source: ProductionFactError::TargetOrder { .. },
            ..
        })
    ));
    Ok(())
}

#[test]
fn rejects_duplicate_unknown_and_trailing_json() -> TestResult {
    let serialized = serde_json::to_string(&origin()?)?;
    let duplicate = serialized.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    let unknown = serialized.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"advisory\":true",
        1,
    );
    let nested_unknown = serialized.replacen("\"path\":", "\"advisory\":false,\"path\":", 1);
    let trailing = format!("{serialized} false");

    for document in [duplicate, unknown, nested_unknown, trailing] {
        assert!(matches!(
            OriginLedger::decode_p1(document.as_bytes()),
            Err(OriginError::Json(_))
        ));
    }
    Ok(())
}

#[test]
fn strict_decoder_rejects_unsorted_production_rows() -> TestResult {
    let ledger = origin()?;
    let mut value = serde_json::to_value(&ledger)?;
    let rows = value
        .get_mut("production_files")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| super::support::missing("production JSON rows"))?;
    rows.swap(0, 1);
    let bytes = serde_json::to_vec(&value)?;

    assert!(matches!(
        OriginLedger::decode_p1(&bytes),
        Err(OriginError::ProductionOrder { .. })
    ));
    Ok(())
}

#[test]
fn rejects_wrong_versions_base_and_forged_fact_identity() -> TestResult {
    let serialized = serde_json::to_string(&origin()?)?;
    let schema = serialized.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
    let analyzer = serialized.replacen("norn-policy-1", "norn-policy-2", 1);
    let commit = serialized.replacen(
        "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d",
        "0000000000000000000000000000000000000000",
        1,
    );
    let registry = serialized.replacen(
        &P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY.to_string(),
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );
    let actual_id = origin()?
        .production_files()
        .first()
        .ok_or_else(|| super::support::missing("production fact"))?
        .origin_id()
        .digest()
        .to_string();
    let forged = serialized.replacen(
        &actual_id,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );

    assert!(matches!(
        OriginLedger::decode_p1(schema.as_bytes()),
        Err(OriginError::SchemaVersion { actual: 2 })
    ));
    assert!(matches!(
        OriginLedger::decode_p1(analyzer.as_bytes()),
        Err(OriginError::AnalyzerVersion)
    ));
    assert!(matches!(
        OriginLedger::decode_p1(commit.as_bytes()),
        Err(OriginError::BaseCommit)
    ));
    assert!(matches!(
        OriginLedger::decode_p1(registry.as_bytes()),
        Err(OriginError::GeneratedRegistry)
    ));
    assert!(matches!(
        OriginLedger::decode_p1(forged.as_bytes()),
        Err(OriginError::ProductionId { .. })
    ));
    Ok(())
}

#[test]
fn rejects_zero_length_writer_span_before_accepting_inventory() -> TestResult {
    let ledger = origin()?;
    let writer = ledger
        .writer_operations()
        .first()
        .ok_or_else(|| super::support::missing("writer fact"))?;
    let (start, _) = writer.span();
    let zero = forged_writer_document(&ledger, "span_end", serde_json::Value::from(start))?;
    assert!(matches!(
        OriginLedger::decode_p1(&zero),
        Err(OriginError::WriterSpan { .. })
    ));
    Ok(())
}

#[test]
fn decoder_binds_every_writer_semantic_field() -> TestResult {
    let ledger = origin()?;
    let writer = ledger
        .writer_operations()
        .first()
        .ok_or_else(|| super::support::missing("writer fact"))?;
    let (start, end) = writer.span();
    let replacements = [
        ("operation_id", serde_json::json!(digest(90)), true),
        ("path", serde_json::json!("src/other.rs"), true),
        (
            "span_start",
            serde_json::json!(start.saturating_sub(1)),
            false,
        ),
        ("span_end", serde_json::json!(end + 1), false),
        ("enclosing_item", serde_json::json!(digest(91)), true),
        ("normalized_call", serde_json::json!(digest(92)), true),
        ("sink", serde_json::json!("std.fs.remove_file"), true),
        ("operation_kind", serde_json::json!("remove"), true),
        ("role", serde_json::json!("cleanup"), true),
        ("discovery", serde_json::json!("macro_invocation"), true),
        ("ordinal", serde_json::json!(writer.ordinal() + 1), true),
    ];

    for (field, replacement, changes_operation_id) in replacements {
        let forged = forged_writer_document(&ledger, field, replacement)?;
        let result = OriginLedger::decode_p1(&forged);
        if changes_operation_id {
            assert!(matches!(result, Err(OriginError::WriterOperationId { .. })));
        } else {
            assert!(matches!(result, Err(OriginError::WriterId { .. })));
        }
    }
    Ok(())
}

#[test]
fn verifies_every_origin_authority_exactly() -> TestResult {
    let ledger = origin()?;
    let source = ledger.source_inventory_digest();
    ledger.verify_authorities(digest(10), source, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY)?;

    assert_eq!(
        ledger.verify_authorities(digest(99), source, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,),
        Err(OriginAuthorityError::RepositoryPolicy)
    );
    assert_eq!(
        ledger.verify_authorities(
            digest(10),
            digest(99),
            P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
        ),
        Err(OriginAuthorityError::SourceInventory)
    );
    assert_eq!(
        ledger.verify_authorities(digest(10), source, digest(99)),
        Err(OriginAuthorityError::GeneratedRegistry)
    );
    Ok(())
}

#[test]
fn debt_multiset_preserves_repeated_canonical_occurrences() -> TestResult {
    let prohibited = ["pan", "ic!"].concat();
    let source = format!("pub fn debt() {{ {prohibited}(\"same\"); {prohibited}(\"same\"); }}\n");
    let baseline = baseline_from_sources(&[("src/lib.rs", &source)])?;
    let ledger = decoded_origin_fixture(digest(1), &baseline)?;

    assert_eq!(ledger.prohibited_debt().len(), 2);
    assert_ne!(
        ledger.prohibited_debt()[0].origin_id(),
        ledger.prohibited_debt()[1].origin_id()
    );
    Ok(())
}

fn forged_writer_document(
    ledger: &OriginLedger,
    field: &str,
    replacement: serde_json::Value,
) -> TestResult<Vec<u8>> {
    let mut value = serde_json::to_value(ledger)?;
    let writer = value
        .get_mut("writer_operations")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|rows| rows.first_mut())
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| super::support::missing("serialized writer fact"))?;
    writer.insert(field.to_owned(), replacement);
    Ok(serde_json::to_vec(&value)?)
}
