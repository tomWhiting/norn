//! Rust production-item identity integration fixtures.

use std::error::Error;

use norn_policy::RepositoryPath;
use norn_policy::rust::{RustItemProjection, rust_item_projections};

type TestResult = Result<(), Box<dyn Error>>;

fn items(source: &str) -> Result<Vec<RustItemProjection>, Box<dyn Error>> {
    let path: RepositoryPath = "src/lib.rs".parse()?;
    Ok(rust_item_projections(&path, source.as_bytes())?)
}

#[test]
fn moving_unchanged_item_behind_test_transfers_the_group_count() -> TestResult {
    let before = items("pub fn execute() { let value = 1; }\n")?;
    let after = items("#[cfg(test)]\npub fn execute() { let value = 1; }\n")?;

    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 1);
    assert_eq!(before[0].base_identity(), after[0].base_identity());
    assert_eq!(before[0].content(), after[0].content());
    assert_eq!(before[0].production_count(), 1);
    assert_eq!(before[0].test_only_count(), 0);
    assert_eq!(after[0].production_count(), 0);
    assert_eq!(after[0].test_only_count(), 1);
    Ok(())
}

#[test]
fn genuine_content_change_keeps_identity_but_changes_projection() -> TestResult {
    let before = items("pub fn execute() { let value = 1; }\n")?;
    let after = items("pub fn execute() { let value = 2; }\n")?;

    assert_eq!(before[0].base_identity(), after[0].base_identity());
    assert_ne!(before[0].content(), after[0].content());
    assert_eq!(after[0].production_count(), 1);
    assert_eq!(after[0].test_only_count(), 0);
    Ok(())
}

#[test]
fn comments_line_shifts_and_crlf_do_not_change_item_projection() -> TestResult {
    let compact = items("pub fn execute() { 1 + 2; }\n")?;
    let shifted = items("\r\n// synthetic note\r\npub fn execute() {\r\n1 + 2;\r\n}\r\n")?;

    assert_eq!(compact, shifted);
    assert_ne!(compact[0].production_spans(), shifted[0].production_spans());
    Ok(())
}

#[test]
fn impl_header_trivia_does_not_mask_nested_test_reclassification() -> TestResult {
    let before =
        items("struct Widget<T>(T);\nimpl<T: Copy> Widget<T> {\n    fn execute(&self) {}\n}\n")?;
    let after = items(
        "struct Widget<T>(T);\r\nimpl <\r\n    T /* bound */ : Copy\r\n> Widget < T > {\r\n    #[cfg(test)]\r\n    fn execute(&self) {}\r\n}\r\n",
    )?;

    assert_eq!(test_transfer_count(&before, &after), 1);
    Ok(())
}

#[test]
fn foreign_header_trivia_does_not_mask_nested_test_reclassification() -> TestResult {
    let before = items("extern \"C\" {\n    fn execute();\n}\n")?;
    let after = items(
        "extern /* stable comment */ \"C\" {\r\n    #[cfg(test)]\r\n    fn execute();\r\n}\r\n",
    )?;

    assert_eq!(test_transfer_count(&before, &after), 1);
    Ok(())
}

#[test]
fn swapping_identical_production_and_test_occurrences_is_invariant() -> TestResult {
    let before = items("fn repeated() {}\n#[cfg(test)]\nfn repeated() {}\n")?;
    let after = items("#[cfg(test)]\nfn repeated() {}\nfn repeated() {}\n")?;

    assert_eq!(before, after);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].production_count(), 1);
    assert_eq!(before[0].test_only_count(), 1);
    assert_eq!(before[0].production_spans().len(), 1);
    assert_eq!(before[0].test_only_spans().len(), 1);
    Ok(())
}

#[test]
fn duplicate_identical_occurrences_form_one_lossless_group() -> TestResult {
    let source = "fn repeated() {}\nfn repeated() {}\n#[cfg(test)]\nfn repeated() {}\n#[cfg(test)]\nfn repeated() {}\n";
    let first = items(source)?;
    let second = items(source)?;

    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].production_count(), 2);
    assert_eq!(first[0].test_only_count(), 2);
    assert_eq!(first[0].production_spans().len(), 2);
    assert_eq!(first[0].test_only_spans().len(), 2);
    assert!(spans_are_sorted(first[0].production_spans()));
    assert!(spans_are_sorted(first[0].test_only_spans()));
    Ok(())
}

#[test]
fn one_duplicate_moved_to_tests_is_a_count_transfer() -> TestResult {
    let before = items("fn repeated() {}\nfn repeated() {}\n#[cfg(test)]\nfn repeated() {}\n")?;
    let after = items(
        "fn repeated() {}\n#[cfg(test)]\nfn repeated() {}\n#[cfg(test)]\nfn repeated() {}\n",
    )?;

    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 1);
    assert_eq!(before[0].base_identity(), after[0].base_identity());
    assert_eq!(before[0].content(), after[0].content());
    assert_eq!(before[0].production_count(), 2);
    assert_eq!(before[0].test_only_count(), 1);
    assert_eq!(after[0].production_count(), 1);
    assert_eq!(after[0].test_only_count(), 2);
    assert_ne!(before, after);
    Ok(())
}

#[test]
fn deletion_and_content_change_have_distinct_group_shapes() -> TestResult {
    let before = items("fn repeated() { 1; }\nfn repeated() { 1; }\n")?;
    let deleted = items("fn repeated() { 1; }\n")?;
    let changed = items("fn repeated() { 1; }\nfn repeated() { 2; }\n")?;

    assert_eq!(before.len(), 1);
    assert_eq!(before[0].production_count(), 2);
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].production_count(), 1);
    assert_eq!(changed.len(), 2);
    assert!(changed.iter().all(|group| group.production_count() == 1));
    Ok(())
}

#[test]
fn raw_and_plain_item_identifiers_have_one_semantic_projection() -> TestResult {
    let plain = items("pub fn execute() { let value = 1; }\n")?;
    let raw = items("pub fn r#execute() { let r#value = 1; }\n")?;

    assert_eq!(plain, raw);
    Ok(())
}

fn spans_are_sorted(spans: &[norn_policy::finding::ByteSpan]) -> bool {
    spans.windows(2).all(|pair| pair[0] <= pair[1])
}

fn test_transfer_count(before: &[RustItemProjection], after: &[RustItemProjection]) -> usize {
    before
        .iter()
        .filter(|origin| {
            origin.production_count() == 1
                && origin.test_only_count() == 0
                && after.iter().any(|current| {
                    current.base_identity() == origin.base_identity()
                        && current.content() == origin.content()
                        && current.production_count() == 0
                        && current.test_only_count() == 1
                })
        })
        .count()
}
