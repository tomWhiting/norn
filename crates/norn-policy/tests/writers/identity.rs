use norn_policy::Digest;
use norn_policy::writers::{
    FlowClass, OperationKind, SinkOrigin, SinkRegistry, SinkSpec, WriterInventory,
    WriterOperationId, WriterRole,
};

use super::support::{TestResult, analyze, analyze_with, operation_id};

#[test]
fn unrelated_line_shifts_preserve_operation_identity() -> TestResult {
    let first = analyze(
        "crates/sample/src/identity.rs",
        "fn run() { std::fs::write(\"alpha\", b\"x\"); }",
    )?;
    let shifted = analyze(
        "crates/sample/src/identity.rs",
        "\n\nfn run() {\n    std::fs::write(\"alpha\", b\"x\");\n}\n",
    )?;
    assert_eq!(
        operation_id(&first, "std.fs.write")?,
        operation_id(&shifted, "std.fs.write")?
    );
    assert_ne!(first.operations()[0].span(), shifted.operations()[0].span());
    Ok(())
}

#[test]
fn raw_identifiers_and_path_comments_preserve_semantic_operation_identity() -> TestResult {
    let plain = analyze(
        "crates/sample/src/raw_identity.rs",
        "fn run() { std::fs::write(\"alpha\", b\"x\"); }",
    )?;
    let decorated = analyze(
        "crates/sample/src/raw_identity.rs",
        "fn run() { std::fs::/* comment */r#write(\"alpha\", b\"x\"); }",
    )?;
    assert_eq!(
        operation_id(&plain, "std.fs.write")?,
        operation_id(&decorated, "std.fs.write")?
    );
    assert_ne!(
        plain.operations()[0].span(),
        decorated.operations()[0].span()
    );
    Ok(())
}

#[test]
fn literal_kinds_have_distinct_normalized_call_identities() -> TestResult {
    let variants = [
        r#"fn run() { std::fs::write("same", b"x"); }"#,
        r#"fn run() { std::fs::write(r"same", b"x"); }"#,
        r#"fn run() { std::fs::write(b"same", b"x"); }"#,
        r#"fn run() { std::fs::write(br"same", b"x"); }"#,
        r#"fn run() { std::fs::write('s', b"x"); }"#,
        r#"fn run() { std::fs::write(b's', b"x"); }"#,
    ];
    let mut digests = Vec::new();
    for source in variants {
        digests.push(normalized_call(source)?);
    }
    for (left_index, left) in digests.iter().enumerate() {
        for right in &digests[left_index + 1..] {
            assert_ne!(left, right);
        }
    }
    Ok(())
}

#[test]
fn literal_whitespace_and_escape_spelling_remain_identity_bytes() -> TestResult {
    let compact = normalized_call(r#"fn run() { std::fs::write("ab", b"x"); }"#)?;
    let spaced = normalized_call(r#"fn run() { std::fs::write("a b", b"x"); }"#)?;
    let literal = normalized_call(r#"fn run() { std::fs::write("a", b"x"); }"#)?;
    let escaped = normalized_call(r#"fn run() { std::fs::write("\x61", b"x"); }"#)?;
    assert_ne!(compact, spaced);
    assert_ne!(literal, escaped);
    Ok(())
}

#[test]
fn path_moves_change_operation_identity() -> TestResult {
    let first = analyze(
        "crates/sample/src/first.rs",
        "fn run() { std::fs::write(\"alpha\", b\"x\"); }",
    )?;
    let moved = analyze(
        "crates/sample/src/moved.rs",
        "fn run() { std::fs::write(\"alpha\", b\"x\"); }",
    )?;
    assert_ne!(
        operation_id(&first, "std.fs.write")?,
        operation_id(&moved, "std.fs.write")?
    );
    Ok(())
}

#[test]
fn identical_occurrences_receive_distinct_multiset_ordinals() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/duplicates.rs",
        r#"
fn run() {
    std::fs::write("alpha", b"x");
    std::fs::write("alpha", b"x");
}
"#,
    )?;
    assert_eq!(inventory.operations().len(), 2);
    assert_eq!(inventory.operations()[0].ordinal(), 0);
    assert_eq!(inventory.operations()[1].ordinal(), 1);
    assert_ne!(
        inventory.operations()[0].id(),
        inventory.operations()[1].id()
    );
    Ok(())
}

#[test]
fn full_impl_headers_keep_existing_operation_identity_when_an_impl_is_inserted() -> TestResult {
    let original = r#"
impl TraitA for Target {
    fn save() { std::fs::write("same", b"x"); }
}
"#;
    let inserted = r#"
impl TraitB for Target {
    fn save() { std::fs::write("same", b"x"); }
}

impl TraitA for Target {
    fn save() { std::fs::write("same", b"x"); }
}
"#;
    let before = analyze("crates/sample/src/impls.rs", original)?;
    let after = analyze("crates/sample/src/impls.rs", inserted)?;
    let trait_a = operation_after(&after, marker_offset(inserted, "impl TraitA")?)?;
    assert_eq!(before.operations()[0].id(), trait_a);
    Ok(())
}

#[test]
fn structural_closure_scope_avoids_ordinal_renumbering() -> TestResult {
    let original = r#"
fn run() {
    let alpha = || std::fs::write("same", b"x");
}
"#;
    let inserted = r#"
fn run() {
    let beta = || std::fs::write("same", b"x");
    let alpha = || std::fs::write("same", b"x");
}
"#;
    let before = analyze("crates/sample/src/closures.rs", original)?;
    let after = analyze("crates/sample/src/closures.rs", inserted)?;
    let alpha = operation_after(&after, marker_offset(inserted, "let alpha")?)?;
    assert_eq!(before.operations()[0].id(), alpha);
    Ok(())
}

#[test]
fn operation_ids_bind_sink_and_role_semantics() -> TestResult {
    let first = registry("fixture.first", WriterRole::HandleMutation)?;
    let renamed = registry("fixture.second", WriterRole::HandleMutation)?;
    let reclassified = registry("fixture.first", WriterRole::Publication)?;
    let source = "fn run() { fixture::write(\"same\"); }";
    let first = analyze_with("crates/sample/src/semantic.rs", source, &first)?;
    let renamed = analyze_with("crates/sample/src/semantic.rs", source, &renamed)?;
    let reclassified = analyze_with("crates/sample/src/semantic.rs", source, &reclassified)?;
    assert_ne!(first.operations()[0].id(), renamed.operations()[0].id());
    assert_ne!(
        first.operations()[0].id(),
        reclassified.operations()[0].id()
    );
    Ok(())
}

#[test]
fn unknown_ids_bind_the_candidate_identity() -> TestResult {
    let first = registry("fixture.first", WriterRole::HandleMutation)?;
    let second = registry("fixture.second", WriterRole::HandleMutation)?;
    let source = "fn consume<T>(value: T) {} fn run() { consume(fixture::write); }";
    let first = analyze_with("crates/sample/src/escape.rs", source, &first)?;
    let second = analyze_with("crates/sample/src/escape.rs", source, &second)?;
    assert_eq!(first.candidates().len(), 1);
    assert_eq!(second.candidates().len(), 1);
    assert_ne!(first.candidates()[0].id(), second.candidates()[0].id());
    Ok(())
}

fn registry(id: &str, role: WriterRole) -> Result<SinkRegistry, Box<dyn std::error::Error>> {
    let spec = SinkSpec::function(
        id,
        "fixture::write",
        OperationKind::Write,
        role,
        FlowClass::None,
        SinkOrigin::Reviewed,
    )?;
    Ok(SinkRegistry::try_new(1, vec![spec])?)
}

fn operation_after(
    inventory: &WriterInventory,
    offset: usize,
) -> Result<WriterOperationId, Box<dyn std::error::Error>> {
    let offset = u64::try_from(offset)?;
    inventory
        .operations()
        .iter()
        .find(|operation| operation.span().start() >= offset)
        .map(norn_policy::writers::WriterOperation::id)
        .ok_or_else(|| std::io::Error::other("operation after structural marker is absent").into())
}

fn marker_offset(source: &str, marker: &str) -> Result<usize, std::io::Error> {
    source
        .find(marker)
        .ok_or_else(|| std::io::Error::other("structural marker is absent"))
}

fn normalized_call(source: &str) -> Result<Digest, Box<dyn std::error::Error>> {
    let inventory = analyze("crates/sample/src/literal_identity.rs", source)?;
    inventory
        .operations()
        .first()
        .map(norn_policy::writers::WriterOperation::normalized_call)
        .ok_or_else(|| std::io::Error::other("writer operation is absent").into())
}
