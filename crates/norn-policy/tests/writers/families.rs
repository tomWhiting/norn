use std::collections::BTreeSet;
use std::error::Error;
use std::io;

use norn_policy::baseline::{
    OriginLedger, P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
    WriterOperationFact,
};
use norn_policy::facts::{SourceInventoryEntry, source_inventory_identity};
use norn_policy::version::{ANALYZER_VERSION, DIGEST_VERSION};
use norn_policy::writers::{
    ClassificationIssue, WRITER_ANALYZER_VERSION, WRITER_SCHEMA_VERSION, WriterClassification,
    WriterClassificationKind, WriterFamilyRegistry, WriterFamilyRegistryError, WriterInventory,
    WriterRole, builtin_sink_registry,
};
use norn_policy::{Digest, digest_bytes};
use serde::Serialize;

use super::support::{analyze, token};

type FamilyTestResult<T = ()> = Result<T, Box<dyn Error>>;

#[derive(Serialize)]
struct FamilyDocument<'a> {
    schema_version: u32,
    algorithms: FamilyAlgorithms,
    sink_registry: Digest,
    writer_resolutions: Digest,
    vocabulary: FamilyVocabulary,
    classifications: &'a [WriterClassification],
}

#[derive(Clone, Serialize)]
struct FamilyVocabulary {
    families: Vec<norn_policy::writers::WriterToken>,
    shared_primitives: Vec<norn_policy::writers::WriterToken>,
    cleanup_reviews: Vec<norn_policy::writers::WriterToken>,
    false_positive_reviews: Vec<norn_policy::writers::WriterToken>,
}

#[derive(Serialize)]
struct FamilyAlgorithms {
    writer: &'static str,
    digest: &'static str,
}

#[test]
fn strict_authority_binds_sink_registry_and_normalizes_formatting() -> FamilyTestResult {
    let inventory = fixture_inventory()?;
    let rows = family_rows(&inventory)?;
    let document = family_document(&rows)?;
    let with_comment = format!("# reviewed writer families\n{document}");
    let first = WriterFamilyRegistry::decode_p1(document.as_bytes())?;
    let second = WriterFamilyRegistry::decode_p1(with_comment.as_bytes())?;
    let other_resolution = digest_bytes(b"other writer-resolution authority");
    let changed_resolution = document.replacen(
        &resolution_digest().to_string(),
        &other_resolution.to_string(),
        1,
    );
    let third = WriterFamilyRegistry::decode_p1(changed_resolution.as_bytes())?;

    assert_eq!(first, second);
    assert_eq!(first.schema_version(), WRITER_SCHEMA_VERSION);
    assert_eq!(
        first.sink_registry_digest(),
        builtin_sink_registry()?.digest()
    );
    assert_eq!(first.writer_resolutions_digest(), resolution_digest());
    assert_eq!(first.families(), [token("session")?]);
    assert!(first.shared_primitives().is_empty());
    assert_eq!(first.cleanup_reviews(), [token("cleanup-review")?]);
    assert!(first.false_positive_reviews().is_empty());
    assert_eq!(first.classifications(), rows);
    assert_eq!(first.normalized_digest()?, second.normalized_digest()?);
    assert_eq!(third.writer_resolutions_digest(), other_resolution);
    assert_ne!(first.normalized_digest()?, third.normalized_digest()?);
    Ok(())
}

#[test]
fn strict_authority_rejects_open_or_non_exact_vocabularies() -> FamilyTestResult {
    let rows = family_rows(&fixture_inventory()?)?;
    let valid = vocabulary_for_rows(&rows);

    let mut unsorted = valid.clone();
    unsorted.families.push(token("alpha")?);
    let unsorted = family_document_with_vocabulary(&rows, unsorted)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(unsorted.as_bytes()),
        Err(WriterFamilyRegistryError::VocabularyOrder { .. })
    ));

    let mut duplicate = valid.clone();
    duplicate.families.push(token("session")?);
    let duplicate = family_document_with_vocabulary(&rows, duplicate)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(duplicate.as_bytes()),
        Err(WriterFamilyRegistryError::VocabularyOrder { .. })
    ));

    let mut overlapping = valid.clone();
    overlapping.false_positive_reviews.push(token("session")?);
    let overlapping = family_document_with_vocabulary(&rows, overlapping)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(overlapping.as_bytes()),
        Err(WriterFamilyRegistryError::VocabularyOverlap)
    ));

    let mut undeclared = valid.clone();
    undeclared.families.clear();
    let undeclared = family_document_with_vocabulary(&rows, undeclared)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(undeclared.as_bytes()),
        Err(WriterFamilyRegistryError::UndeclaredVocabularyReference)
    ));

    let mut unused = valid.clone();
    unused.families.push(token("unused")?);
    let unused = family_document_with_vocabulary(&rows, unused)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(unused.as_bytes()),
        Err(WriterFamilyRegistryError::UnusedVocabularyEntry)
    ));

    let mut wrong_review_class = valid.clone();
    let cleanup_review = wrong_review_class.cleanup_reviews.pop();
    let Some(cleanup_review) = cleanup_review else {
        return Err(missing("cleanup review vocabulary").into());
    };
    wrong_review_class
        .false_positive_reviews
        .push(cleanup_review);
    let wrong_review_class = family_document_with_vocabulary(&rows, wrong_review_class)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(wrong_review_class.as_bytes()),
        Err(WriterFamilyRegistryError::UndeclaredVocabularyReference)
    ));

    let unknown_field =
        family_document(&rows)?.replacen("families =", "advisory = true\nfamilies =", 1);
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(unknown_field.as_bytes()),
        Err(WriterFamilyRegistryError::Toml(_))
    ));
    Ok(())
}

#[test]
fn shared_primitives_require_declared_and_identical_inbound_edges() -> FamilyTestResult {
    let inventory = fixture_inventory()?;
    let mut rows = family_rows(&inventory)?;
    if rows.len() != 2 {
        return Err(missing("two classification rows").into());
    }
    rows[0].classification = WriterClassificationKind::SharedPrimitive {
        primitive: token("private-fs")?,
        inbound_families: vec![token("alpha")?, token("beta")?],
    };
    rows[1].classification = WriterClassificationKind::SharedPrimitive {
        primitive: token("private-fs")?,
        inbound_families: vec![token("alpha")?, token("gamma")?],
    };
    let vocabulary = vocabulary_for_rows(&rows);
    let conflicting = family_document_with_vocabulary(&rows, vocabulary)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(conflicting.as_bytes()),
        Err(WriterFamilyRegistryError::SharedPrimitiveEdgeConflict)
    ));

    rows[1].classification = rows[0].classification.clone();
    let valid = vocabulary_for_rows(&rows);
    let valid_document = family_document_with_vocabulary(&rows, valid.clone())?;
    let registry = WriterFamilyRegistry::decode_p1(valid_document.as_bytes())?;
    assert_eq!(registry.shared_primitives(), [token("private-fs")?]);
    assert_eq!(registry.families(), [token("alpha")?, token("beta")?]);

    let mut missing_inbound = valid;
    let removed = missing_inbound.families.pop();
    assert!(removed.is_some());
    let missing_inbound = family_document_with_vocabulary(&rows, missing_inbound)?;
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(missing_inbound.as_bytes()),
        Err(WriterFamilyRegistryError::UndeclaredVocabularyReference)
    ));
    Ok(())
}

#[test]
fn strict_authority_rejects_schema_sink_order_and_shared_edge_drift() -> FamilyTestResult {
    let inventory = fixture_inventory()?;
    let rows = family_rows(&inventory)?;
    let document = family_document(&rows)?;
    let unknown = document.replacen(
        "schema_version = 1",
        "schema_version = 1\nadvisory = true",
        1,
    );
    let wrong_analyzer = document.replacen(WRITER_ANALYZER_VERSION, "norn-writers-other", 1);
    let reviewed_sink = builtin_sink_registry()?.digest();
    let wrong_sink = document.replacen(
        &reviewed_sink.to_string(),
        &digest_bytes(b"other writer sink registry").to_string(),
        1,
    );
    let resolution_line = format!("writer_resolutions = \"{}\"\n", resolution_digest());
    let missing_resolution = document.replacen(&resolution_line, "", 1);

    assert!(matches!(
        WriterFamilyRegistry::decode_p1(unknown.as_bytes()),
        Err(WriterFamilyRegistryError::Toml(_))
    ));
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(wrong_analyzer.as_bytes()),
        Err(WriterFamilyRegistryError::WriterAnalyzerVersion)
    ));
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(wrong_sink.as_bytes()),
        Err(WriterFamilyRegistryError::SinkRegistry)
    ));
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(missing_resolution.as_bytes()),
        Err(WriterFamilyRegistryError::Toml(_))
    ));

    let mut duplicate = rows.clone();
    let repeated = duplicate
        .first()
        .cloned()
        .ok_or_else(|| missing("classification row"))?;
    duplicate.push(repeated);
    duplicate.sort_by_key(|row| row.operation);
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(family_document(&duplicate)?.as_bytes()),
        Err(WriterFamilyRegistryError::ClassificationOrder { .. })
    ));

    let mut reversed = rows.clone();
    if reversed.len() < 2 {
        return Err(missing("two classification rows").into());
    }
    reversed.reverse();
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(family_document(&reversed)?.as_bytes()),
        Err(WriterFamilyRegistryError::ClassificationOrder { .. })
    ));

    let mut invalid_shared = rows;
    let first = invalid_shared
        .first_mut()
        .ok_or_else(|| missing("classification row"))?;
    let duplicate_family = token("session")?;
    first.classification = WriterClassificationKind::SharedPrimitive {
        primitive: token("private-fs")?,
        inbound_families: vec![duplicate_family.clone(), duplicate_family],
    };
    assert!(matches!(
        WriterFamilyRegistry::decode_p1(family_document(&invalid_shared)?.as_bytes()),
        Err(WriterFamilyRegistryError::SharedFamilies { .. })
    ));
    Ok(())
}

#[test]
fn origin_link_requires_complete_rows_and_allows_reviewed_additions() -> FamilyTestResult {
    let inventory = fixture_inventory()?;
    let origin = origin_from_inventory(&inventory)?;
    let rows = family_rows(&inventory)?;
    let registry = WriterFamilyRegistry::decode_p1(family_document(&rows)?.as_bytes())?;
    assert!(registry.validate_against_origin(&origin).is_empty());

    let mut missing_row = rows.clone();
    let removed = missing_row.pop();
    assert!(removed.is_some());
    assert!(
        registry_with_rows(&missing_row)?
            .validate_against_origin(&origin)
            .iter()
            .any(|issue| matches!(issue, ClassificationIssue::Missing { .. }))
    );

    let other = analyze(
        "crates/sample/src/other_writer.rs",
        "fn other() { let result = std::fs::write(\"other\", b\"x\"); drop(result); }",
    )?;
    let other_operation = other
        .operations()
        .first()
        .ok_or_else(|| missing("other writer operation"))?;
    let mut stale_rows = rows.clone();
    stale_rows.push(WriterClassification {
        operation: other_operation.id(),
        classification: WriterClassificationKind::Family {
            family: token("session")?,
        },
    });
    stale_rows.sort_by_key(|row| row.operation);
    assert!(
        registry_with_rows(&stale_rows)?
            .validate_against_origin(&origin)
            .is_empty()
    );

    let mut occurrence_override = rows;
    let row = occurrence_override
        .first_mut()
        .ok_or_else(|| missing("classification row"))?;
    row.classification = WriterClassificationKind::SharedPrimitive {
        primitive: token("private-fs")?,
        inbound_families: vec![token("session")?, token("tasks")?],
    };
    let issues = registry_with_rows(&occurrence_override)?.validate_against_origin(&origin);
    assert!(issues.is_empty());
    Ok(())
}

fn fixture_inventory() -> FamilyTestResult<WriterInventory> {
    analyze(
        "crates/sample/src/writers.rs",
        r#"
fn save() {
    let first = std::fs::write("first", b"x");
    drop(first);
    let second = std::fs::remove_file("second");
    drop(second);
}
"#,
    )
}

fn family_rows(inventory: &WriterInventory) -> FamilyTestResult<Vec<WriterClassification>> {
    let mut rows = inventory
        .operations()
        .iter()
        .map(|operation| {
            let classification = if operation.role() == WriterRole::Cleanup {
                WriterClassificationKind::ReviewedCleanup {
                    review: token("cleanup-review")?,
                }
            } else {
                WriterClassificationKind::Family {
                    family: token("session")?,
                }
            };
            Ok(WriterClassification {
                operation: operation.id(),
                classification,
            })
        })
        .collect::<FamilyTestResult<Vec<_>>>()?;
    rows.sort_by_key(|row| row.operation);
    Ok(rows)
}

fn family_document(rows: &[WriterClassification]) -> FamilyTestResult<String> {
    family_document_with_vocabulary(rows, vocabulary_for_rows(rows))
}

fn family_document_with_vocabulary(
    rows: &[WriterClassification],
    vocabulary: FamilyVocabulary,
) -> FamilyTestResult<String> {
    let document = FamilyDocument {
        schema_version: WRITER_SCHEMA_VERSION,
        algorithms: FamilyAlgorithms {
            writer: WRITER_ANALYZER_VERSION,
            digest: DIGEST_VERSION,
        },
        sink_registry: builtin_sink_registry()?.digest(),
        writer_resolutions: resolution_digest(),
        vocabulary,
        classifications: rows,
    };
    Ok(toml::to_string(&document)?)
}

fn vocabulary_for_rows(rows: &[WriterClassification]) -> FamilyVocabulary {
    let mut families = BTreeSet::new();
    let mut shared_primitives = BTreeSet::new();
    let mut cleanup_reviews = BTreeSet::new();
    let mut false_positive_reviews = BTreeSet::new();
    for row in rows {
        match &row.classification {
            WriterClassificationKind::Family { family } => {
                families.insert(family.clone());
            }
            WriterClassificationKind::ReviewedCleanup { review } => {
                cleanup_reviews.insert(review.clone());
            }
            WriterClassificationKind::ReviewedFalsePositive { review } => {
                false_positive_reviews.insert(review.clone());
            }
            WriterClassificationKind::SharedPrimitive {
                primitive,
                inbound_families,
            } => {
                shared_primitives.insert(primitive.clone());
                families.extend(inbound_families.iter().cloned());
            }
        }
    }
    FamilyVocabulary {
        families: families.into_iter().collect(),
        shared_primitives: shared_primitives.into_iter().collect(),
        cleanup_reviews: cleanup_reviews.into_iter().collect(),
        false_positive_reviews: false_positive_reviews.into_iter().collect(),
    }
}

fn resolution_digest() -> Digest {
    digest_bytes(b"writer-resolution-authority")
}

fn registry_with_rows(rows: &[WriterClassification]) -> FamilyTestResult<WriterFamilyRegistry> {
    Ok(WriterFamilyRegistry::decode_p1(
        family_document(rows)?.as_bytes(),
    )?)
}

fn origin_from_inventory(inventory: &WriterInventory) -> FamilyTestResult<OriginLedger> {
    let source_inventory = inventory
        .sources()
        .iter()
        .map(|source| SourceInventoryEntry {
            path: source.path().clone(),
            content: source.content(),
            production: true,
            test_only: false,
        })
        .collect::<Vec<_>>();
    let mut writer_operations = inventory
        .operations()
        .iter()
        .map(WriterOperationFact::from_canonical)
        .collect::<Vec<_>>();
    writer_operations.sort_by(|left, right| {
        (left.operation_id(), left.path(), left.span()).cmp(&(
            right.operation_id(),
            right.path(),
            right.span(),
        ))
    });
    let document = serde_json::json!({
        "schema_version": 1,
        "algorithms": {
            "analyzer": ANALYZER_VERSION,
            "digest": DIGEST_VERSION,
        },
        "base": {
            "commit": P1_BASE_COMMIT,
            "tree": P1_BASE_TREE,
        },
        "digests": {
            "repository_policy": digest_bytes(b"writer-family-policy"),
            "source_inventory": source_inventory_identity(&source_inventory),
            "generated_include_registry": P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
        },
        "source_inventory": source_inventory,
        "compile_test_fixtures": [],
        "production_files": [],
        "item_groups": [],
        "prohibited_debt": [],
        "writer_operations": writer_operations,
    });
    Ok(OriginLedger::decode_p1(&serde_json::to_vec(&document)?)?)
}

fn missing(name: &str) -> io::Error {
    io::Error::other(format!("fixture is missing {name}"))
}
