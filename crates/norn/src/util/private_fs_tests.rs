use std::io::{Read as _, Write as _};
use std::path::Path;

use super::*;

#[cfg(target_os = "macos")]
fn worker_panic(label: &str, payload: &(dyn std::any::Any + Send)) -> io::Error {
    let detail = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    io::Error::other(format!("{label}: {detail}"))
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
#[test]
fn unsupported_targets_fail_before_private_artifact_io() -> Result<(), Box<dyn std::error::Error>> {
    let error = PrivateRoot::create(Path::new("/private-artifact"))
        .err()
        .ok_or("unsupported target unexpectedly opened private storage")?;
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn creates_private_tree_and_reopens_regular_file() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    root.create_dir_all(Path::new("nested/deep"))?;
    let mut file = root.create_new(Path::new("nested/deep/data.json"))?;
    file.write_all(b"private")?;
    drop(file);

    let mode = |path: &Path| std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777);
    assert_eq!(mode(&root_path)?, 0o700);
    assert_eq!(mode(&root_path.join("nested"))?, 0o700);
    assert_eq!(mode(&root_path.join("nested/deep"))?, 0o700);
    assert_eq!(mode(&root_path.join("nested/deep/data.json"))?, 0o600);

    let mut content = String::new();
    root.open_read(Path::new("nested/deep/data.json"))?
        .read_to_string(&mut content)?;
    assert_eq!(content, "private");
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn repeated_root_directory_reads_restart_from_the_beginning()
-> Result<(), Box<dyn std::error::Error>> {
    let container = tempfile::tempdir()?;
    let root = PrivateRoot::create(&container.path().join("private"))?;
    root.create_new(Path::new("first"))?.write_all(b"one")?;
    root.create_new(Path::new("second"))?.write_all(b"two")?;

    let first = root.read_dir(Path::new(""))?;
    let second = root.read_dir(Path::new(""))?;
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn macos_create_retry_is_single_bounded_and_create_only() {
    use std::cell::Cell;

    let transient_attempts = Cell::new(0_u8);
    let transient = openat_with_macos_create_retry(true, || {
        let attempt = transient_attempts.get();
        transient_attempts.set(attempt.saturating_add(1));
        if attempt == 0 {
            Err(rustix::io::Errno::NOENT)
        } else {
            Ok(())
        }
    });
    assert_eq!(transient, Ok(()));
    assert_eq!(transient_attempts.get(), 2);

    let persistent_attempts = Cell::new(0_u8);
    let persistent: Result<(), rustix::io::Errno> = openat_with_macos_create_retry(true, || {
        persistent_attempts.set(persistent_attempts.get().saturating_add(1));
        Err(rustix::io::Errno::NOENT)
    });
    assert_eq!(persistent, Err(rustix::io::Errno::NOENT));
    assert_eq!(persistent_attempts.get(), 2);

    let read_attempts = Cell::new(0_u8);
    let read: Result<(), rustix::io::Errno> = openat_with_macos_create_retry(false, || {
        read_attempts.set(read_attempts.get().saturating_add(1));
        Err(rustix::io::Errno::NOENT)
    });
    assert_eq!(read, Err(rustix::io::Errno::NOENT));
    assert_eq!(read_attempts.get(), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn concurrent_independent_roots_open_one_shared_lock() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::{Arc, Barrier};

    const CALLERS: usize = 8;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    PrivateRoot::create(&root_path)?;
    let root_path = Arc::new(root_path);
    let barrier = Arc::new(Barrier::new(CALLERS));
    let handles = (0..CALLERS)
        .map(|_| {
            let root_path = Arc::clone(&root_path);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || -> io::Result<()> {
                let root = PrivateRoot::open(&root_path)?;
                barrier.wait();
                drop(root.open_lock(Path::new("index.lock"))?);
                Ok(())
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle
            .join()
            .map_err(|payload| worker_panic("shared-lock worker panicked", payload.as_ref()))??;
    }
    assert!(root_path.join("index.lock").is_file());
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn concurrent_create_new_has_exactly_one_winner() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::{Arc, Barrier};

    const CALLERS: usize = 8;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    PrivateRoot::create(&root_path)?;
    let root_path = Arc::new(root_path);
    let barrier = Arc::new(Barrier::new(CALLERS));
    let handles = (0..CALLERS)
        .map(|_| {
            let root_path = Arc::clone(&root_path);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || -> io::Result<bool> {
                let root = PrivateRoot::open(&root_path)?;
                barrier.wait();
                match root.create_new(Path::new("exclusive.bin")) {
                    Ok(file) => {
                        drop(file);
                        Ok(true)
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                    Err(error) => Err(error),
                }
            })
        })
        .collect::<Vec<_>>();

    let mut winners = 0_usize;
    for handle in handles {
        if handle
            .join()
            .map_err(|payload| worker_panic("create-new worker panicked", payload.as_ref()))??
        {
            winners = winners.saturating_add(1);
        }
    }
    assert_eq!(winners, 1);
    assert!(root_path.join("exclusive.bin").is_file());
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn read_only_reopen_hardens_existing_descendants() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let nested = root_path.join("nested/deep");
    let payload = nested.join("data.json");
    std::fs::create_dir_all(&nested)?;
    std::fs::write(&payload, "private")?;
    for directory in [&root_path, &root_path.join("nested"), &nested] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o644))?;

    let root = PrivateRoot::open(&root_path)?;
    let mut content = String::new();
    root.open_read(Path::new("nested/deep/data.json"))?
        .read_to_string(&mut content)?;
    let mode = |path: &Path| std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777);

    assert_eq!(content, "private");
    assert_eq!(mode(&root_path)?, 0o700);
    assert_eq!(mode(&root_path.join("nested"))?, 0o700);
    assert_eq!(mode(&nested)?, 0o700);
    assert_eq!(mode(&payload)?, 0o600);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn rejects_relative_root_and_final_or_intermediate_links() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::symlink;

    assert_eq!(
        PrivateRoot::create(Path::new("relative/private"))
            .err()
            .ok_or("relative root unexpectedly opened")?
            .kind(),
        io::ErrorKind::InvalidInput,
    );

    let container = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    std::fs::write(outside.path().join("secret"), "unchanged")?;
    symlink(outside.path(), root_path.join("linked-dir"))?;
    symlink(outside.path().join("secret"), root_path.join("linked-file"))?;

    assert!(root.open_read(Path::new("linked-dir/secret")).is_err());
    assert!(root.create_new(Path::new("linked-file")).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret"))?,
        "unchanged",
    );
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn refuses_root_or_ancestor_replaced_by_symlink() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let ancestor = container.path().join("ancestor");
    let root_path = ancestor.join("private");
    std::fs::create_dir_all(&root_path)?;
    let root = PrivateRoot::open(&root_path)?;
    root.create_new(Path::new("kept"))?;

    std::fs::rename(&ancestor, container.path().join("parked"))?;
    let outside = tempfile::tempdir()?;
    std::fs::create_dir(outside.path().join("private"))?;
    symlink(outside.path(), &ancestor)?;

    assert!(PrivateRoot::open(&root_path).is_err());
    root.create_new(Path::new("still-pinned"))?;
    assert!(
        container
            .path()
            .join("parked/private/still-pinned")
            .exists()
    );
    assert!(!outside.path().join("private/still-pinned").exists());
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn atomic_operations_reject_non_regular_destinations() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let root = PrivateRoot::create(&root_path)?;
    root.create_new(Path::new("source"))?.write_all(b"source")?;
    std::fs::create_dir(root_path.join("directory"))?;
    symlink("source", root_path.join("link"))?;

    assert!(
        root.rename(Path::new("source"), Path::new("directory"))
            .is_err()
    );
    assert!(root.rename(Path::new("source"), Path::new("link")).is_err());
    assert!(root.remove_file(Path::new("link")).is_err());
    assert_eq!(std::fs::read_to_string(root_path.join("source"))?, "source");
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn pinned_parent_survives_ancestor_replacement_before_rename()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let parked = container.path().join("parked");
    let outside = tempfile::tempdir()?;
    let root = PrivateRoot::create(&root_path)?;
    root.create_new(Path::new("source"))?.write_all(b"source")?;
    root.create_new(Path::new("destination"))?
        .write_all(b"old")?;
    std::fs::write(outside.path().join("destination"), "outside")?;

    rename_relative_file_after(&root, Path::new("source"), Path::new("destination"), || {
        assert!(std::fs::rename(&root_path, &parked).is_ok());
        assert!(symlink(outside.path(), &root_path).is_ok());
    })?;

    assert_eq!(
        std::fs::read_to_string(parked.join("destination"))?,
        "source"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("destination"))?,
        "outside",
    );
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn final_replacement_before_mutation_is_revalidated_on_pinned_parent()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("outside");
    std::fs::write(&outside_file, "outside")?;
    let root = PrivateRoot::create(&root_path)?;
    root.create_new(Path::new("source"))?.write_all(b"source")?;
    root.create_new(Path::new("destination"))?
        .write_all(b"old")?;

    let rename_result =
        rename_relative_file_after(&root, Path::new("source"), Path::new("destination"), || {
            assert!(std::fs::remove_file(root_path.join("destination")).is_ok());
            assert!(symlink(&outside_file, root_path.join("destination")).is_ok());
        });
    assert!(rename_result.is_err());
    assert_eq!(std::fs::read_to_string(&outside_file)?, "outside");
    assert_eq!(std::fs::read_to_string(root_path.join("source"))?, "source");

    root.create_new(Path::new("remove-me"))?
        .write_all(b"private")?;
    let remove_result = remove_relative_file_after(&root, Path::new("remove-me"), || {
        assert!(std::fs::remove_file(root_path.join("remove-me")).is_ok());
        assert!(symlink(&outside_file, root_path.join("remove-me")).is_ok());
    });
    assert!(remove_result.is_err());
    assert_eq!(std::fs::read_to_string(&outside_file)?, "outside");

    root.create_new(Path::new("link-source"))?
        .write_all(b"private")?;
    let link_result = publish_new_relative_file_after(
        &root,
        Path::new("link-source"),
        Path::new("linked"),
        || {
            assert!(std::fs::remove_file(root_path.join("link-source")).is_ok());
            assert!(symlink(&outside_file, root_path.join("link-source")).is_ok());
        },
    );
    assert!(link_result.is_err());
    assert!(!root_path.join("linked").exists());
    assert_eq!(std::fs::read_to_string(outside_file)?, "outside");
    Ok(())
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn final_name_races_remain_confined_and_never_follow_outside_target()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("private");
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("outside");
    std::fs::write(&outside_file, "outside-sentinel")?;
    let root = PrivateRoot::create(&root_path)?;

    root.create_new(Path::new("remove-source"))?
        .write_all(b"private")?;
    remove_relative_file_with_hooks(
        &root,
        Path::new("remove-source"),
        || {},
        || {
            assert!(std::fs::remove_file(root_path.join("remove-source")).is_ok());
            assert!(symlink(&outside_file, root_path.join("remove-source")).is_ok());
        },
    )?;
    assert!(!root_path.join("remove-source").exists());
    assert_eq!(std::fs::read_to_string(&outside_file)?, "outside-sentinel");

    root.create_new(Path::new("rename-source"))?
        .write_all(b"private")?;
    root.create_new(Path::new("rename-destination"))?
        .write_all(b"old")?;
    rename_relative_file_with_hooks(
        &root,
        Path::new("rename-source"),
        Path::new("rename-destination"),
        || {},
        || {
            assert!(std::fs::remove_file(root_path.join("rename-source")).is_ok());
            assert!(symlink(&outside_file, root_path.join("rename-source")).is_ok());
        },
    )?;
    assert!(
        std::fs::symlink_metadata(root_path.join("rename-destination"))?
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&outside_file)?, "outside-sentinel");

    root.create_new(Path::new("rename-destination-source"))?
        .write_all(b"private-destination")?;
    root.create_new(Path::new("rename-destination-race"))?
        .write_all(b"old")?;
    rename_relative_file_with_hooks(
        &root,
        Path::new("rename-destination-source"),
        Path::new("rename-destination-race"),
        || {},
        || {
            assert!(std::fs::remove_file(root_path.join("rename-destination-race")).is_ok());
            assert!(symlink(&outside_file, root_path.join("rename-destination-race")).is_ok());
        },
    )?;
    assert_eq!(
        std::fs::read_to_string(root_path.join("rename-destination-race"))?,
        "private-destination",
    );
    assert_eq!(std::fs::read_to_string(&outside_file)?, "outside-sentinel");

    root.create_new(Path::new("publish-destination-source"))?
        .write_all(b"private")?;
    let destination_race = publish_new_relative_file_with_hooks(
        &root,
        Path::new("publish-destination-source"),
        Path::new("publish-destination-race"),
        || {},
        || {
            assert!(symlink(&outside_file, root_path.join("publish-destination-race")).is_ok());
        },
    );
    assert!(destination_race.is_err());
    assert!(
        std::fs::symlink_metadata(root_path.join("publish-destination-race"))?
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&outside_file)?, "outside-sentinel");
    assert_eq!(
        std::fs::read_to_string(root_path.join("publish-destination-source"))?,
        "private",
    );

    root.create_new(Path::new("publish-source"))?
        .write_all(b"private")?;
    let published = publish_new_relative_file_with_hooks(
        &root,
        Path::new("publish-source"),
        Path::new("published"),
        || {},
        || {
            assert!(std::fs::remove_file(root_path.join("publish-source")).is_ok());
            assert!(symlink(&outside_file, root_path.join("publish-source")).is_ok());
        },
    );
    assert!(published.is_err());
    assert!(!root_path.join("published").exists());
    assert_eq!(std::fs::read_to_string(outside_file)?, "outside-sentinel");
    Ok(())
}
