use std::fs;

use tempfile::tempdir;

use super::SessionArtifactStore;
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::store::DurabilityPolicy;

fn artifact_store(
    data_dir: &std::path::Path,
) -> Result<SessionArtifactStore, crate::session::SessionPersistError> {
    let manager = SessionManager::new(data_dir);
    let opened = manager.create_with_id(
        "root-session",
        CreateSessionOptions {
            model: "test-model".to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    SessionArtifactStore::for_session(data_dir, &opened.entry, DurabilityPolicy::Flush, None)
}

#[test]
fn fetched_artifacts_are_private_immutable_and_session_owned()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let store = artifact_store(temp.path())?;

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
    let store = artifact_store(temp.path())?;
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
    let store = artifact_store(temp.path())?;
    let artifact = store.write_fetched("https://example.test/a\nforged: true", "body")?;
    let persisted = fs::read_to_string(artifact)?;

    assert!(persisted.contains(r#"url: "https://example.test/a\nforged: true""#));
    assert!(!persisted.contains("\nforged: true\n"));
    Ok(())
}

#[test]
fn fetched_content_preserves_leading_frontmatter_verbatim() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let store = artifact_store(temp.path())?;
    let content = "---\ntitle: Source metadata\n---\n\n# Body\n";
    let artifact = store.write_fetched("https://example.test/frontmatter", content)?;
    let persisted = fs::read_to_string(artifact)?;

    assert!(
        persisted.ends_with(content),
        "the fetched document must follow Norn metadata byte-for-byte",
    );
    Ok(())
}

#[test]
fn stale_store_cannot_publish_into_recreated_session_id() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let manager = SessionManager::new(temp.path());
    let options = || CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    };
    let first = manager.create_with_id("artifact-aba", options(), DurabilityPolicy::Flush)?;
    let stale = SessionArtifactStore::for_session(
        temp.path(),
        &first.entry,
        DurabilityPolicy::Flush,
        None,
    )?;
    let first_generation = first.entry.generation;
    drop(first.store);
    manager.delete("artifact-aba")?;
    let replacement = manager.create_with_id("artifact-aba", options(), DurabilityPolicy::Flush)?;
    assert_ne!(first_generation, replacement.entry.generation);

    let error = stale
        .write_fetched("https://example.test/stale", "stale")
        .err()
        .ok_or_else(|| std::io::Error::other("stale artifact write unexpectedly succeeded"))?;
    assert!(matches!(
        error,
        crate::session::SessionPersistError::GenerationChanged { .. }
    ));
    assert!(!temp.path().join("artifact-aba/artifacts").exists());

    let current = SessionArtifactStore::for_session(
        temp.path(),
        &replacement.entry,
        DurabilityPolicy::Flush,
        None,
    )?;
    current.write_fetched("https://example.test/current", "current")?;
    Ok(())
}
