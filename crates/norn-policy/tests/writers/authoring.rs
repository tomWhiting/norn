use std::error::Error;
use std::io;

use norn_policy::digest_bytes;
use norn_policy::writers::{
    WriterClassification, WriterClassificationKind, WriterFamilyRegistry, WriterRole,
};

use super::support::{analyze, token};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn reviewed_rows_are_sorted_and_encoded_deterministically() -> TestResult {
    let inventory = analyze(
        "crates/sample/src/authoring.rs",
        r#"
fn persist() {
    let first = std::fs::write("one", b"x");
    drop(first);
    let second = std::fs::remove_file("two");
    drop(second);
}
"#,
    )?;
    let mut rows = inventory
        .operations()
        .iter()
        .map(|operation| {
            let classification = if operation.role() == WriterRole::Cleanup {
                WriterClassificationKind::ReviewedCleanup {
                    review: token("cleanup-authority")?,
                }
            } else {
                WriterClassificationKind::Family {
                    family: token("session-artifacts")?,
                }
            };
            Ok(WriterClassification {
                operation: operation.id(),
                classification,
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    rows.reverse();

    let registry = WriterFamilyRegistry::author_p1(
        digest_bytes(b"writer-resolution-authority"),
        vec![token("session-artifacts")?],
        Vec::new(),
        vec![token("cleanup-authority")?],
        Vec::new(),
        rows,
    )?;
    if !registry
        .classifications()
        .windows(2)
        .all(|pair| pair[0].operation < pair[1].operation)
    {
        return Err(io::Error::other("authored rows are not sorted").into());
    }
    let first = registry.encode_p1()?;
    let second = registry.encode_p1()?;
    assert_eq!(first, second);
    assert!(first.ends_with(b"\n"));
    assert!(!first.ends_with(b"\n\n"));
    assert_eq!(WriterFamilyRegistry::decode_p1(&first)?, registry);
    Ok(())
}
