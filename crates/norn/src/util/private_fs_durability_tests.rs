use std::io;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn durable_creation_syncs_every_traversed_parent_on_every_call() -> TestResult {
    let container = tempfile::tempdir()?;
    let path = container.path().join("one/two");
    let expected = absolute_components(&path)?.len();

    for _ in 0..2 {
        let mut syncs = 0_usize;
        let descriptor = open_absolute_with_parent_sync(&path, true, |_| {
            syncs = syncs.saturating_add(1);
            Ok(())
        })?;
        drop(descriptor);
        assert_eq!(syncs, expected);
    }
    Ok(())
}

#[test]
fn durable_creation_stops_when_parent_sync_fails() -> TestResult {
    let container = tempfile::tempdir()?;
    let path = container.path().join("never/reached");
    let result = open_absolute_with_parent_sync(&path, true, |_| {
        Err(io::Error::other("parent sync sentinel"))
    });

    let error = result
        .err()
        .ok_or_else(|| io::Error::other("parent sync failure was ignored"))?;
    assert_eq!(error.to_string(), "parent sync sentinel");
    assert!(!path.exists());
    Ok(())
}
