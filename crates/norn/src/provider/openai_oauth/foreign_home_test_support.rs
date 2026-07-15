use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Eq, PartialEq)]
struct EntrySnapshot {
    kind: EntryKind,
    bytes: Option<Vec<u8>>,
    symlink_target: Option<PathBuf>,
    len: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    links: u64,
    #[cfg(unix)]
    owner: u32,
    #[cfg(unix)]
    group: u32,
}

#[derive(Eq, PartialEq)]
pub(crate) struct ForeignHomeSnapshot {
    entries: BTreeMap<PathBuf, EntrySnapshot>,
}

pub(crate) fn populate_foreign_home(root: &Path) -> Result<(), std::io::Error> {
    let profiles = root.join("profiles");
    std::fs::create_dir(&profiles)?;
    std::fs::write(root.join("auth.json"), b"foreign-auth-sentinel")?;
    std::fs::write(root.join("profile.json"), b"foreign-profile-sentinel")?;
    std::fs::write(
        profiles.join("secondary.json"),
        b"foreign-secondary-sentinel",
    )?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("profiles/secondary.json", root.join("current-profile"))?;
        set_fixture_modes(root, &profiles)?;
    }
    Ok(())
}

pub(crate) fn snapshot_foreign_home(root: &Path) -> Result<ForeignHomeSnapshot, std::io::Error> {
    let mut entries = BTreeMap::new();
    snapshot_entry(root, Path::new("."), &mut entries)?;
    Ok(ForeignHomeSnapshot { entries })
}

pub(crate) fn verify_foreign_home_unchanged(
    root: &Path,
    before: &ForeignHomeSnapshot,
) -> Result<(), std::io::Error> {
    let after = snapshot_foreign_home(root)?;
    if &after != before {
        return Err(std::io::Error::other(
            "foreign CODEX_HOME inventory, bytes, or metadata changed",
        ));
    }
    if after.entries.keys().any(|path| is_norn_artifact(path)) {
        return Err(std::io::Error::other(
            "foreign CODEX_HOME contains a Norn credential transaction artifact",
        ));
    }
    Ok(())
}

fn snapshot_entry(
    root: &Path,
    relative: &Path,
    entries: &mut BTreeMap<PathBuf, EntrySnapshot>,
) -> Result<(), std::io::Error> {
    let absolute = if relative == Path::new(".") {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let metadata = std::fs::symlink_metadata(&absolute)?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    };
    let bytes = file_type
        .is_file()
        .then(|| std::fs::read(&absolute))
        .transpose()?;
    let symlink_target = file_type
        .is_symlink()
        .then(|| std::fs::read_link(&absolute))
        .transpose()?;
    entries.insert(
        relative.to_path_buf(),
        entry_snapshot(kind, bytes, symlink_target, &metadata),
    );

    if file_type.is_dir() {
        let mut children = std::fs::read_dir(&absolute)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        for child in children {
            snapshot_entry(root, &relative.join(child), entries)?;
        }
    }
    Ok(())
}

fn entry_snapshot(
    kind: EntryKind,
    bytes: Option<Vec<u8>>,
    symlink_target: Option<PathBuf>,
    metadata: &std::fs::Metadata,
) -> EntrySnapshot {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;

    EntrySnapshot {
        kind,
        bytes,
        symlink_target,
        len: metadata.len(),
        created: metadata.created().ok(),
        modified: metadata.modified().ok(),
        readonly: metadata.permissions().readonly(),
        #[cfg(unix)]
        mode: metadata.mode(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        links: metadata.nlink(),
        #[cfg(unix)]
        owner: metadata.uid(),
        #[cfg(unix)]
        group: metadata.gid(),
    }
}

fn is_norn_artifact(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name == ".norn-auth.lock"
            || (name.starts_with("auth.json.") && name.ends_with(".tmp"))
            || (name.starts_with("auth.json.") && name.ends_with(".logout-quarantine"))
    })
}

#[cfg(unix)]
fn set_fixture_modes(root: &Path, profiles: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))?;
    std::fs::set_permissions(profiles, std::fs::Permissions::from_mode(0o710))?;
    std::fs::set_permissions(
        root.join("auth.json"),
        std::fs::Permissions::from_mode(0o640),
    )?;
    std::fs::set_permissions(
        root.join("profile.json"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    std::fs::set_permissions(
        profiles.join("secondary.json"),
        std::fs::Permissions::from_mode(0o644),
    )
}
