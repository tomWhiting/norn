use std::io::Write as _;
use std::path::Path;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn atomic_no_replace_rename_preserves_a_raced_source_replacement() -> TestResult {
    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    root.create_new(Path::new("source"))?.write_all(b"old")?;

    rename_new_relative_file_with_hooks(
        &root,
        Path::new("source"),
        Path::new("quarantine"),
        || {
            assert!(std::fs::remove_file(root_path.join("source")).is_ok());
            assert!(std::fs::write(root_path.join("source"), b"replacement").is_ok());
        },
    )?;

    assert!(!root_path.join("source").exists());
    assert_eq!(std::fs::read(root_path.join("quarantine"))?, b"replacement");
    Ok(())
}

#[test]
fn atomic_no_replace_rename_never_overwrites_a_retained_destination() -> TestResult {
    let container = tempfile::tempdir()?;
    let root = PrivateRoot::create(&container.path().join("private"))?;
    root.create_new(Path::new("source"))?.write_all(b"source")?;
    root.create_new(Path::new("retained"))?
        .write_all(b"retained")?;

    let result = root.rename_new(Path::new("source"), Path::new("retained"));

    assert!(result.is_err());
    assert_eq!(
        std::fs::read(root.display_path(Path::new("source")))?,
        b"source"
    );
    assert_eq!(
        std::fs::read(root.display_path(Path::new("retained")))?,
        b"retained"
    );
    Ok(())
}
