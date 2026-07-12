use std::fs;

use tempfile::tempdir;

use super::artifacts::SessionArtifactStore;
use super::store::DurabilityPolicy;

#[test]
fn fetched_artifacts_are_private_immutable_and_session_owned()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let store =
        SessionArtifactStore::for_session(temp.path(), "root-session", DurabilityPolicy::Flush)?;

    let first = store.write_fetched("https://example.test/a", "first body")?;
    let second = store.write_fetched("https://example.test/a", "second body")?;

    assert_ne!(first, second, "each fetch invocation must be immutable");
    assert!(first.starts_with(temp.path().join("root-session/artifacts/fetched")));
    assert!(second.starts_with(temp.path().join("root-session/artifacts/fetched")));
    assert!(fs::read_to_string(first)?.contains("first body"));
    assert!(fs::read_to_string(second)?.contains("second body"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn artifact_tree_and_files_have_private_modes() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempdir()?;
    let store =
        SessionArtifactStore::for_session(temp.path(), "root-session", DurabilityPolicy::Flush)?;
    let artifact = store.write_fetched("https://example.test/a", "body")?;

    let dir_mode = fs::metadata(store.readable_root())?.permissions().mode() & 0o777;
    let file_mode = fs::metadata(artifact)?.permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(file_mode, 0o600);
    Ok(())
}

#[test]
fn source_url_is_encoded_as_a_scalar_not_frontmatter_structure()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let store =
        SessionArtifactStore::for_session(temp.path(), "root-session", DurabilityPolicy::Flush)?;
    let artifact = store.write_fetched("https://example.test/a\nforged: true", "body")?;
    let persisted = fs::read_to_string(artifact)?;

    assert!(persisted.contains(r#"url: "https://example.test/a\nforged: true""#));
    assert!(!persisted.contains("\nforged: true\n"));
    Ok(())
}
