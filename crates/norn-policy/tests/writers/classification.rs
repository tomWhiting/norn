use norn_policy::digest_bytes;
use norn_policy::writers::{
    ClassificationIssue, FlowClass, OperationKind, SinkOrigin, SinkRegistry, SinkSpec,
    WriterClassification, WriterClassificationKind, WriterFamilyRegistry, WriterRole,
    builtin_sink_registry, validate_writer_classifications,
};

use super::support::{TestResult, analyze, analyze_with, token};

#[derive(Clone, Copy)]
enum ClassificationFixture {
    Family,
    Cleanup,
    FalsePositive,
    Shared,
}

#[test]
fn family_cleanup_false_positive_and_shared_rows_are_exact() -> TestResult {
    let mut specs = builtin_sink_registry()?.specs().to_vec();
    specs.push(SinkSpec::function(
        "fixture.render_write",
        "render_write",
        OperationKind::Write,
        WriterRole::FalsePositive,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?);
    let registry = SinkRegistry::try_new(1, specs)?;
    let inventory = analyze_with(
        "crates/sample/src/classes.rs",
        r#"
use crate::util::PrivateRoot;

fn run() {
    let root = PrivateRoot::create("root");
    root.sync_dir("child");
    root.remove_file("child");
    render_write();
}
"#,
        &registry,
    )?;
    assert!(inventory.candidates().is_empty());
    let mut classifications = Vec::new();
    for operation in inventory.operations() {
        let classification = if operation.sink().as_str() == "project.private_root.create" {
            WriterClassificationKind::Family {
                family: token("fixture")?,
            }
        } else {
            match operation.role() {
            WriterRole::SharedPrimitive => WriterClassificationKind::SharedPrimitive {
                primitive: token("private-fs")?,
                inbound_families: vec![token("session")?, token("tasks")?],
            },
            WriterRole::Cleanup => WriterClassificationKind::ReviewedCleanup {
                review: token("cleanup-1")?,
            },
            WriterRole::FalsePositive => WriterClassificationKind::ReviewedFalsePositive {
                review: token("render-1")?,
            },
            _ => WriterClassificationKind::Family {
                family: token("fixture")?,
            },
            }
        };
        classifications.push(WriterClassification {
            operation: operation.id(),
            classification,
        });
    }
    assert!(validate_writer_classifications(&inventory, &classifications).is_empty());
    let authority = WriterFamilyRegistry::author_p1(
        digest_bytes(b"writer-resolution-authority"),
        vec![token("fixture")?, token("session")?, token("tasks")?],
        vec![token("private-fs")?],
        vec![token("cleanup-1")?],
        vec![token("render-1")?],
        classifications.clone(),
    )?;
    assert_eq!(authority.families().len(), 3);
    assert_eq!(authority.shared_primitives(), [token("private-fs")?]);
    assert_eq!(authority.cleanup_reviews(), [token("cleanup-1")?]);
    assert_eq!(authority.false_positive_reviews(), [token("render-1")?]);

    let mut missing = classifications.clone();
    let removed = missing.pop();
    assert!(removed.is_some());
    assert!(
        validate_writer_classifications(&inventory, &missing)
            .iter()
            .any(|issue| matches!(issue, ClassificationIssue::Missing { .. }))
    );

    let mut duplicate = classifications.clone();
    if let Some(first) = classifications.first() {
        duplicate.push(first.clone());
    }
    assert!(
        validate_writer_classifications(&inventory, &duplicate)
            .iter()
            .any(|issue| matches!(issue, ClassificationIssue::Duplicate { .. }))
    );
    Ok(())
}

#[test]
fn stale_rows_and_invalid_shared_edges_are_reported() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/current.rs",
        "fn run() { std::fs::write(\"alpha\", b\"x\"); }",
    )?;
    let other = analyze(
        "crates/sample/src/other.rs",
        "fn run() { std::fs::write(\"alpha\", b\"x\"); }",
    )?;
    let stale = WriterClassification {
        operation: other.operations()[0].id(),
        classification: WriterClassificationKind::Family {
            family: token("fixture")?,
        },
    };
    let issues = validate_writer_classifications(&inventory, &[stale]);
    assert!(
        issues
            .iter()
            .any(|issue| matches!(issue, ClassificationIssue::Stale { .. }))
    );

    let shared = analyze(
        "crates/sample/src/shared.rs",
        "use crate::util::PrivateRoot; fn run() { let root = PrivateRoot::create(\"root\"); root.sync_dir(\"child\"); }",
    )?;
    let primitive = token("private-fs")?;
    let family = token("session")?;
    let rows: Vec<WriterClassification> = shared
        .operations()
        .iter()
        .map(|operation| WriterClassification {
            operation: operation.id(),
            classification: WriterClassificationKind::SharedPrimitive {
                primitive: primitive.clone(),
                inbound_families: vec![family.clone(), family.clone()],
            },
        })
        .collect();
    assert!(
        validate_writer_classifications(&shared, &rows)
            .iter()
            .any(|issue| matches!(issue, ClassificationIssue::SharedEdges { .. }))
    );

    let tasks = token("tasks")?;
    let session = token("session")?;
    let reversed_rows: Vec<WriterClassification> = shared
        .operations()
        .iter()
        .map(|operation| WriterClassification {
            operation: operation.id(),
            classification: WriterClassificationKind::SharedPrimitive {
                primitive: primitive.clone(),
                inbound_families: vec![tasks.clone(), session.clone()],
            },
        })
        .collect();
    assert!(
        validate_writer_classifications(&shared, &reversed_rows)
            .iter()
            .any(|issue| matches!(issue, ClassificationIssue::SharedEdges { .. }))
    );
    Ok(())
}

#[test]
fn occurrence_review_can_override_macro_and_shared_primitive_hints() -> TestResult {
    let formatting = analyze(
        "crates/sample/src/formatting.rs",
        r#"
fn render() {
    let mut output = String::new();
    write!(&mut output, "value");
}
"#,
    )?;
    assert_eq!(formatting.operations().len(), 1);
    let false_positive = WriterClassification {
        operation: formatting.operations()[0].id(),
        classification: WriterClassificationKind::ReviewedFalsePositive {
            review: token("string-formatting")?,
        },
    };
    assert!(validate_writer_classifications(&formatting, &[false_positive]).is_empty());

    let primitive = analyze(
        "crates/sample/src/private_root.rs",
        r#"
use crate::util::PrivateRoot;

fn prepare() {
    let root = PrivateRoot::create("root");
    root.sync_dir("child");
}
"#,
    )?;
    let session = token("session")?;
    let rows = primitive
        .operations()
        .iter()
        .map(|operation| WriterClassification {
            operation: operation.id(),
            classification: WriterClassificationKind::Family {
                family: session.clone(),
            },
        })
        .collect::<Vec<_>>();
    assert!(validate_writer_classifications(&primitive, &rows).is_empty());
    Ok(())
}

#[test]
fn reviewed_classification_is_per_occurrence_not_fixed_by_sink_role() -> TestResult {
    let roles = [
        WriterRole::RootOpen,
        WriterRole::HandleMutation,
        WriterRole::Publication,
        WriterRole::Permissions,
        WriterRole::Durability,
        WriterRole::Cleanup,
        WriterRole::SharedPrimitive,
        WriterRole::FalsePositive,
    ];
    let fixtures = [
        ClassificationFixture::Family,
        ClassificationFixture::Cleanup,
        ClassificationFixture::FalsePositive,
        ClassificationFixture::Shared,
    ];
    for role in roles {
        let registry = SinkRegistry::try_new(
            1,
            vec![SinkSpec::function(
                "fixture.sink",
                "fixture::sink",
                OperationKind::Write,
                role,
                FlowClass::None,
                SinkOrigin::Reviewed,
            )?],
        )?;
        let inventory = analyze_with(
            "crates/sample/src/class_matrix.rs",
            "fn run() { fixture::sink(); }",
            &registry,
        )?;
        let operation = inventory.operations()[0].id();
        for fixture in fixtures.iter().copied() {
            let row = WriterClassification {
                operation,
                classification: classification(fixture)?,
            };
            assert!(validate_writer_classifications(&inventory, &[row]).is_empty());
        }
    }
    Ok(())
}

fn classification(
    fixture: ClassificationFixture,
) -> Result<WriterClassificationKind, Box<dyn std::error::Error>> {
    Ok(match fixture {
        ClassificationFixture::Family => WriterClassificationKind::Family {
            family: token("family")?,
        },
        ClassificationFixture::Cleanup => WriterClassificationKind::ReviewedCleanup {
            review: token("cleanup-review")?,
        },
        ClassificationFixture::FalsePositive => WriterClassificationKind::ReviewedFalsePositive {
            review: token("false-positive-review")?,
        },
        ClassificationFixture::Shared => WriterClassificationKind::SharedPrimitive {
            primitive: token("shared")?,
            inbound_families: vec![token("alpha")?, token("beta")?],
        },
    })
}
