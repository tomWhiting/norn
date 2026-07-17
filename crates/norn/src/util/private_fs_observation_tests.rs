use std::io::Read as _;
use std::path::Path;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn observational_tree_walk_is_sorted_and_never_changes_modes() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("legacy");
    std::fs::create_dir(&root_path)?;
    std::fs::create_dir(root_path.join("b"))?;
    std::fs::create_dir(root_path.join("a"))?;
    std::fs::write(root_path.join("b/payload"), b"bbb")?;
    std::fs::write(root_path.join("a/payload"), b"a")?;
    std::fs::set_permissions(&root_path, std::fs::Permissions::from_mode(0o751))?;
    std::fs::set_permissions(root_path.join("a"), std::fs::Permissions::from_mode(0o750))?;
    std::fs::set_permissions(root_path.join("b"), std::fs::Permissions::from_mode(0o711))?;
    std::fs::set_permissions(
        root_path.join("a/payload"),
        std::fs::Permissions::from_mode(0o640),
    )?;
    std::fs::set_permissions(
        root_path.join("b/payload"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    let modes_before = modes(&root_path)?;

    let reader = PrivateRootReader::open(&root_path)?;
    let entries = reader.read_tree()?;
    let paths = entries
        .iter()
        .map(|entry| entry.path.as_path())
        .collect::<Vec<_>>();

    assert_eq!(
        paths,
        [
            Path::new("a"),
            Path::new("a/payload"),
            Path::new("b"),
            Path::new("b/payload")
        ]
    );
    assert_eq!(entries[1].kind, PrivateEntryKind::File);
    assert_eq!(entries[1].length, Some(1));
    assert_eq!(entries[1].mode & 0o777, 0o640);
    assert_eq!(reader.read_tree()?, entries);

    let mut payload = String::new();
    reader
        .open_file(Path::new("b/payload"))?
        .read_to_string(&mut payload)?;
    assert_eq!(payload, "bbb");
    assert_eq!(modes(&root_path)?, modes_before);
    Ok(())
}

#[test]
fn observational_tree_walk_and_file_open_reject_links() -> TestResult {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("legacy");
    let outside = container.path().join("outside");
    std::fs::create_dir(&root_path)?;
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("secret"), b"outside")?;
    symlink(outside.join("secret"), root_path.join("linked-file"))?;

    let reader = PrivateRootReader::open(&root_path)?;
    assert!(reader.read_tree().is_err());
    assert!(reader.open_file(Path::new("linked-file")).is_err());
    assert_eq!(std::fs::read(outside.join("secret"))?, b"outside");
    Ok(())
}

#[test]
fn observational_root_open_rejects_a_symlinked_root() -> TestResult {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root_path = container.path().join("legacy");
    let alias = container.path().join("alias");
    std::fs::create_dir(&root_path)?;
    symlink(&root_path, &alias)?;

    assert!(PrivateRootReader::open(&alias).is_err());
    Ok(())
}

fn modes(root: &Path) -> io::Result<Vec<(std::path::PathBuf, u32)>> {
    use std::os::unix::fs::PermissionsExt as _;

    let mut paths = vec![
        root.to_path_buf(),
        root.join("a"),
        root.join("a/payload"),
        root.join("b"),
        root.join("b/payload"),
    ];
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
            Ok((path, mode))
        })
        .collect()
}
