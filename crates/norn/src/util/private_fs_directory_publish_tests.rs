use std::io::Write as _;
use std::path::Path;

use super::mutation::publish_new_relative_directory_with_hooks;
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn directory_publication_moves_one_complete_tree() -> TestResult {
    let container = tempfile::tempdir()?;
    let root = PrivateRoot::create(&container.path().join("private"))?;
    root.create_dir_all(Path::new("staging/tree/nested"))?;
    root.create_dir_all(Path::new("published"))?;
    root.create_new(Path::new("staging/tree/nested/payload"))?
        .write_all(b"complete")?;

    root.publish_new_dir(Path::new("staging/tree"), Path::new("published/session"))?;

    assert!(!root.display_path(Path::new("staging/tree")).exists());
    assert_eq!(
        std::fs::read(root.display_path(Path::new("published/session/nested/payload")))?,
        b"complete"
    );
    Ok(())
}

#[test]
fn directory_publication_never_replaces_an_existing_destination() -> TestResult {
    let container = tempfile::tempdir()?;
    let root = PrivateRoot::create(&container.path().join("private"))?;
    root.create_dir_all(Path::new("staged"))?;
    root.create_dir_all(Path::new("retained"))?;
    root.create_new(Path::new("staged/source"))?
        .write_all(b"source")?;
    root.create_new(Path::new("retained/original"))?
        .write_all(b"retained")?;

    let result = root.publish_new_dir(Path::new("staged"), Path::new("retained"));

    assert!(result.is_err());
    assert_eq!(
        std::fs::read(root.display_path(Path::new("staged/source")))?,
        b"source"
    );
    assert_eq!(
        std::fs::read(root.display_path(Path::new("retained/original")))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn directory_publication_rejects_a_replaced_source_entry() -> TestResult {
    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    root.create_dir_all(Path::new("source-parent/staged"))?;
    root.create_dir_all(Path::new("destination-parent"))?;
    root.create_new(Path::new("source-parent/staged/original"))?
        .write_all(b"original")?;

    let result = publish_new_relative_directory_with_hooks(
        &root,
        Path::new("source-parent/staged"),
        Path::new("destination-parent/published"),
        || {
            assert!(
                std::fs::rename(
                    root_path.join("source-parent/staged"),
                    root_path.join("source-parent/original-parked")
                )
                .is_ok()
            );
            assert!(std::fs::create_dir(root_path.join("source-parent/staged")).is_ok());
            assert!(
                std::fs::write(root_path.join("source-parent/staged/foreign"), b"foreign").is_ok()
            );
        },
    );

    assert!(result.is_err());
    assert_eq!(
        result.err().map(|error| error.kind()),
        Some(io::ErrorKind::PermissionDenied)
    );
    assert_eq!(
        std::fs::read(root_path.join("source-parent/original-parked/original"))?,
        b"original"
    );
    assert_eq!(
        std::fs::read(root_path.join("source-parent/staged/foreign"))?,
        b"foreign"
    );
    assert!(!root_path.join("destination-parent/published").exists());
    Ok(())
}

#[test]
fn directory_publication_never_overwrites_a_raced_destination() -> TestResult {
    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    root.create_dir_all(Path::new("staged"))?;
    root.create_dir_all(Path::new("destination-parent"))?;
    root.create_new(Path::new("staged/source"))?
        .write_all(b"source")?;

    let result = publish_new_relative_directory_with_hooks(
        &root,
        Path::new("staged"),
        Path::new("destination-parent/published"),
        || {
            assert!(std::fs::create_dir(root_path.join("destination-parent/published")).is_ok());
            assert!(
                std::fs::write(
                    root_path.join("destination-parent/published/retained"),
                    b"retained"
                )
                .is_ok()
            );
        },
    );

    assert!(result.is_err());
    assert_eq!(std::fs::read(root_path.join("staged/source"))?, b"source");
    assert_eq!(
        std::fs::read(root_path.join("destination-parent/published/retained"))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn directory_publication_rejects_paths_outside_the_private_root() -> TestResult {
    let container = tempfile::tempdir()?;
    let root = PrivateRoot::create(&container.path().join("private"))?;
    let outside = container.path().join("outside");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("sentinel"), b"outside")?;
    root.create_dir_all(Path::new("staged"))?;
    root.create_dir_all(Path::new("destination"))?;

    assert!(
        root.publish_new_dir(Path::new("../outside"), Path::new("destination/published"))
            .is_err()
    );
    assert!(
        root.publish_new_dir(Path::new("staged"), Path::new("../outside/published"))
            .is_err()
    );
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"outside");
    assert!(!outside.join("published").exists());
    Ok(())
}

#[test]
fn directory_publication_uses_pinned_source_and_destination_parents() -> TestResult {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    root.create_dir_all(Path::new("source-parent/staged"))?;
    root.create_dir_all(Path::new("destination-parent"))?;
    root.create_new(Path::new("source-parent/staged/payload"))?
        .write_all(b"private")?;
    let outside_source = tempfile::tempdir()?;
    let outside_destination = tempfile::tempdir()?;

    publish_new_relative_directory_with_hooks(
        &root,
        Path::new("source-parent/staged"),
        Path::new("destination-parent/published"),
        || {
            assert!(
                std::fs::rename(
                    root_path.join("source-parent"),
                    root_path.join("source-parked")
                )
                .is_ok()
            );
            assert!(symlink(outside_source.path(), root_path.join("source-parent")).is_ok());
            assert!(
                std::fs::rename(
                    root_path.join("destination-parent"),
                    root_path.join("destination-parked")
                )
                .is_ok()
            );
            assert!(
                symlink(
                    outside_destination.path(),
                    root_path.join("destination-parent")
                )
                .is_ok()
            );
        },
    )?;

    assert!(!root_path.join("source-parked/staged").exists());
    assert_eq!(
        std::fs::read(root_path.join("destination-parked/published/payload"))?,
        b"private"
    );
    assert!(std::fs::read_dir(outside_source.path())?.next().is_none());
    assert!(
        std::fs::read_dir(outside_destination.path())?
            .next()
            .is_none()
    );
    Ok(())
}
