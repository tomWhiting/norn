use std::fs;
use std::io;
use std::path::Path;

use super::PrivateLineLog;

#[test]
fn rejects_relative_paths() {
    let result = PrivateLineLog::new(Path::new("history.txt"));
    assert!(matches!(
        result,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn creates_private_parent_and_file_and_round_trips() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir()?;
    let parent = temporary.path().join("nested");
    let path = parent.join("history.txt");
    let log = PrivateLineLog::new(&path)?;

    log.append_line("first")?;
    log.append_line("second")?;

    assert_eq!(log.path(), path);
    assert_eq!(log.read_to_string()?, "first\nsecond\n");
    assert_eq!(fs::metadata(parent)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn refuses_final_symlink_without_mutating_its_target() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let parent = temporary.path().join("private");
    fs::create_dir(&parent)?;
    let outside = temporary.path().join("outside.txt");
    fs::write(&outside, "sentinel")?;
    let path = parent.join("history.txt");
    symlink(&outside, &path)?;

    let log = PrivateLineLog::new(&path)?;
    let result = log.append_line("secret");

    assert!(result.is_err());
    assert_eq!(fs::read_to_string(outside)?, "sentinel");
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn refuses_to_read_through_final_symlink() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let parent = temporary.path().join("private");
    fs::create_dir(&parent)?;
    let outside = temporary.path().join("outside.txt");
    fs::write(&outside, "sentinel\n")?;
    let path = parent.join("history.txt");
    symlink(&outside, &path)?;

    let log = PrivateLineLog::new(&path)?;
    assert!(log.read_to_string().is_err());
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn heals_existing_modes() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir()?;
    let parent = temporary.path().join("private");
    fs::create_dir(&parent)?;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755))?;
    let path = parent.join("history.txt");
    fs::write(&path, "first\n")?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;

    let log = PrivateLineLog::new(&path)?;
    log.append_line("second")?;

    assert_eq!(fs::metadata(parent)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn truncates_torn_tail_before_append() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("history.txt");
    fs::write(&path, "complete\ntorn")?;
    let log = PrivateLineLog::new(&path)?;

    assert_eq!(log.read_to_string()?, "complete\n");
    log.append_line("next")?;

    assert_eq!(log.read_to_string()?, "complete\nnext\n");
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn rejects_bound_file_identity_replacement() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("history.txt");
    let displaced = temporary.path().join("history.displaced");
    let log = PrivateLineLog::new(&path)?;
    log.append_line("trusted")?;

    fs::rename(&path, displaced)?;
    fs::write(&path, "replacement\n")?;

    let error = log.read_to_string().err().ok_or_else(|| {
        io::Error::other("replaced private line-log identity was unexpectedly accepted")
    })?;
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn concurrent_writers_preserve_complete_records() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("history.txt");
    let mut writers = Vec::new();
    for writer in 0..8 {
        let path = path.clone();
        writers.push(std::thread::spawn(move || -> io::Result<()> {
            let log = PrivateLineLog::new(&path)?;
            for record in 0..50 {
                log.append_line(&format!("{writer}:{record}"))?;
            }
            Ok(())
        }));
    }
    for writer in writers {
        match writer.join() {
            Ok(result) => result?,
            Err(payload) => {
                drop(payload);
                return Err(io::Error::other("private line-log writer panicked"));
            }
        }
    }

    let contents = PrivateLineLog::new(&path)?.read_to_string()?;
    let mut records = contents.lines().collect::<Vec<_>>();
    records.sort_unstable();
    let mut expected = (0..8)
        .flat_map(|writer| (0..50).map(move |record| format!("{writer}:{record}")))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(records, expected);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn rejects_records_with_physical_newlines() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let log = PrivateLineLog::new(&temporary.path().join("history.txt"))?;

    let result = log.append_line("first\nsecond");

    assert!(matches!(
        result,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));
    Ok(())
}
