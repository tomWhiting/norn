//! Descriptor-pinned, no-follow workspace observations.

use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use norn_policy::{RepositoryPath, SnapshotEntry};
use sha2::{Digest as _, Sha256};

use super::error::{SnapshotAdapterError, WorkspaceEntryIssue};

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::os::fd::OwnedFd;

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use crate::resource::DescriptorPermit;

/// A workspace root pinned independently of its mutable pathname.
pub(super) struct WorkspaceRoot {
    path: PathBuf,
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    descriptor: OwnedFd,
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    identity: DirectoryIdentity,
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    descriptor_permit: DescriptorPermit,
}

impl WorkspaceRoot {
    /// Pin one canonical absolute workspace root without following any ancestor.
    pub(super) fn open(path: PathBuf) -> Result<Self, SnapshotAdapterError> {
        open_workspace_root(path)
    }

    /// Confirm that the original absolute spelling still names the pinned root.
    pub(super) fn verify_named_identity(&self) -> Result<(), SnapshotAdapterError> {
        verify_named_identity(self)
    }

    /// Observe one Git-inventoried leaf without following links.
    pub(super) fn observe(
        &self,
        path: &RepositoryPath,
    ) -> Result<WorkspaceObservation, SnapshotAdapterError> {
        observe_workspace_entry(self, path)
    }
}

impl std::fmt::Debug for WorkspaceRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceRoot")
            .field("descriptor_weight", &self.descriptor_permit.weight())
            .finish_non_exhaustive()
    }
}

/// Exact stable observation retained in a current-snapshot seal.
#[derive(Clone, Eq, PartialEq)]
pub(super) enum WorkspaceObservation {
    Missing,
    Present {
        entry: SnapshotEntry,
        stamp: WorkspaceStamp,
    },
}

impl WorkspaceObservation {
    pub(super) fn entry(&self) -> Option<&SnapshotEntry> {
        match self {
            Self::Missing => None,
            Self::Present { entry, .. } => Some(entry),
        }
    }
}

impl std::fmt::Debug for WorkspaceObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Present { entry, .. } => formatter
                .debug_struct("Present")
                .field("kind", &entry.kind())
                .field("byte_len", &entry.len())
                .finish(),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct WorkspaceStamp {
    identity: StatIdentity,
    content_sha256: [u8; 32],
}

#[derive(Clone, Eq, PartialEq)]
struct StatIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    modified_seconds: i64,
    modified_nanos: i64,
    changed_seconds: i64,
    changed_nanos: i64,
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[derive(Clone, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
impl DirectoryIdentity {
    fn from_stat(stat: &rustix::fs::Stat) -> Result<Self, SnapshotAdapterError> {
        Ok(Self {
            device: checked_unsigned(stat.st_dev)?,
            inode: checked_unsigned(stat.st_ino)?,
        })
    }
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
impl StatIdentity {
    fn from_stat(stat: &rustix::fs::Stat) -> Result<Self, SnapshotAdapterError> {
        Ok(Self {
            device: checked_unsigned(stat.st_dev)?,
            inode: checked_unsigned(stat.st_ino)?,
            mode: checked_mode(stat.st_mode)?,
            size: checked_unsigned(stat.st_size)?,
            modified_seconds: checked_signed(stat.st_mtime)?,
            modified_nanos: checked_signed(stat.st_mtime_nsec)?,
            changed_seconds: checked_signed(stat.st_ctime)?,
            changed_nanos: checked_signed(stat.st_ctime_nsec)?,
        })
    }
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn checked_unsigned<T>(value: T) -> Result<u64, SnapshotAdapterError>
where
    T: TryInto<u64>,
{
    value.try_into().or(Err(SnapshotAdapterError::Filesystem {
        kind: io::ErrorKind::InvalidData,
    }))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn checked_mode<T>(value: T) -> Result<u32, SnapshotAdapterError>
where
    T: TryInto<u32>,
{
    value.try_into().or(Err(SnapshotAdapterError::Filesystem {
        kind: io::ErrorKind::InvalidData,
    }))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn checked_signed<T>(value: T) -> Result<i64, SnapshotAdapterError>
where
    T: TryInto<i64>,
{
    value.try_into().or(Err(SnapshotAdapterError::Filesystem {
        kind: io::ErrorKind::InvalidData,
    }))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn directory_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_workspace_root(path: PathBuf) -> Result<WorkspaceRoot, SnapshotAdapterError> {
    use rustix::fs::{Mode, fstat, open, openat};

    if !path.is_absolute() {
        return Err(SnapshotAdapterError::RepositoryRoot);
    }
    let traversal_permit = crate::resource::acquire_filesystem_operation()?;
    let descriptor_permit = crate::resource::DescriptorGovernor::global()?.try_acquire(1)?;
    let mut descriptor = open(Path::new("/"), directory_flags(), Mode::empty())
        .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                descriptor = openat(&descriptor, name, directory_flags(), Mode::empty())
                    .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(SnapshotAdapterError::RepositoryRoot);
            }
        }
    }
    let identity = DirectoryIdentity::from_stat(
        &fstat(&descriptor)
            .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?,
    )?;
    drop(traversal_permit);
    Ok(WorkspaceRoot {
        path,
        descriptor,
        identity,
        descriptor_permit,
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_workspace_root(_: PathBuf) -> Result<WorkspaceRoot, SnapshotAdapterError> {
    Err(SnapshotAdapterError::UnsupportedPlatform)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn verify_named_identity(root: &WorkspaceRoot) -> Result<(), SnapshotAdapterError> {
    use rustix::fs::fstat;

    let permit = crate::resource::acquire_filesystem_operation()?;
    let result = (|| {
        let reopened = open_absolute_directory(&root.path)?;
        let observed = DirectoryIdentity::from_stat(
            &fstat(&reopened)
                .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?,
        )?;
        if observed != root.identity {
            return Err(SnapshotAdapterError::SnapshotChanged);
        }
        Ok(())
    })();
    drop(permit);
    result
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn verify_named_identity(_: &WorkspaceRoot) -> Result<(), SnapshotAdapterError> {
    Err(SnapshotAdapterError::UnsupportedPlatform)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_absolute_directory(path: &Path) -> Result<OwnedFd, SnapshotAdapterError> {
    use rustix::fs::{Mode, open, openat};

    let mut descriptor = open(Path::new("/"), directory_flags(), Mode::empty())
        .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                descriptor = openat(&descriptor, name, directory_flags(), Mode::empty())
                    .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(SnapshotAdapterError::RepositoryRoot);
            }
        }
    }
    Ok(descriptor)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn observe_workspace_entry(
    root: &WorkspaceRoot,
    path: &RepositoryPath,
) -> Result<WorkspaceObservation, SnapshotAdapterError> {
    let permit = crate::resource::acquire_filesystem_operation()?;
    let result = observe_workspace_entry_admitted(root, path);
    drop(permit);
    result
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn observe_workspace_entry_admitted(
    root: &WorkspaceRoot,
    path: &RepositoryPath,
) -> Result<WorkspaceObservation, SnapshotAdapterError> {
    use rustix::fs::{AtFlags, FileType, statat};
    use rustix::io::Errno;

    let (parent, name) = match open_parent(root, path) {
        Ok(value) => value,
        Err(SnapshotAdapterError::Filesystem {
            kind: io::ErrorKind::NotFound,
        }) => return Ok(WorkspaceObservation::Missing),
        Err(error) => return Err(error),
    };
    let before = match statat(&parent, &name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(WorkspaceObservation::Missing),
        Err(error) => return Err(entry_io(path, &io::Error::from(error))),
    };
    match FileType::from_raw_mode(before.st_mode) {
        FileType::RegularFile => observe_regular(path, &parent, &name, &before),
        FileType::Symlink => observe_symlink(path, &parent, &name, &before),
        _ => Err(SnapshotAdapterError::WorkspaceEntry {
            path: path.clone(),
            issue: WorkspaceEntryIssue::UnsupportedKind,
        }),
    }
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn observe_workspace_entry(
    _: &WorkspaceRoot,
    _: &RepositoryPath,
) -> Result<WorkspaceObservation, SnapshotAdapterError> {
    Err(SnapshotAdapterError::UnsupportedPlatform)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_parent(
    root: &WorkspaceRoot,
    path: &RepositoryPath,
) -> Result<(OwnedFd, std::ffi::OsString), SnapshotAdapterError> {
    use rustix::fs::{Mode, openat};

    let mut components = Path::new(path.as_str()).components().collect::<Vec<_>>();
    let Some(Component::Normal(name)) = components.pop() else {
        return Err(SnapshotAdapterError::RepositoryPath);
    };
    let mut parent = rustix::io::fcntl_dupfd_cloexec(&root.descriptor, 0)
        .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?;
    for component in components {
        let Component::Normal(name) = component else {
            return Err(SnapshotAdapterError::RepositoryPath);
        };
        parent = openat(&parent, name, directory_flags(), Mode::empty())
            .map_err(|error| SnapshotAdapterError::filesystem(&io::Error::from(error)))?;
    }
    Ok((parent, name.to_os_string()))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn observe_regular(
    path: &RepositoryPath,
    parent: &OwnedFd,
    name: &std::ffi::OsStr,
    before: &rustix::fs::Stat,
) -> Result<WorkspaceObservation, SnapshotAdapterError> {
    use std::fs::File;

    use rustix::fs::{AtFlags, Mode, OFlags, fstat, openat, statat};

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let descriptor = openat(parent, name, flags, Mode::empty())
        .map_err(|error| entry_io(path, &io::Error::from(error)))?;
    let opened = fstat(&descriptor).map_err(|error| entry_io(path, &io::Error::from(error)))?;
    if StatIdentity::from_stat(before)? != StatIdentity::from_stat(&opened)? {
        return Err(entry_changed(path));
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::new();
    let observed_size =
        u64::try_from(opened.st_size).map_err(|_| SnapshotAdapterError::Capacity)?;
    reserve_regular_capacity(&mut bytes, observed_size)?;
    file.read_to_end(&mut bytes)
        .map_err(|error| entry_io(path, &error))?;
    let after = fstat(&file).map_err(|error| entry_io(path, &io::Error::from(error)))?;
    let named = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| entry_io(path, &io::Error::from(error)))?;
    let identity = StatIdentity::from_stat(before)?;
    if identity != StatIdentity::from_stat(&after)? || identity != StatIdentity::from_stat(&named)?
    {
        return Err(entry_changed(path));
    }
    Ok(present(SnapshotEntry::regular(bytes), identity))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn reserve_regular_capacity(
    bytes: &mut Vec<u8>,
    observed_size: u64,
) -> Result<(), SnapshotAdapterError> {
    let capacity = usize::try_from(observed_size).map_err(|_| SnapshotAdapterError::Capacity)?;
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| SnapshotAdapterError::Capacity)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn observe_symlink(
    path: &RepositoryPath,
    parent: &OwnedFd,
    name: &std::ffi::OsStr,
    before: &rustix::fs::Stat,
) -> Result<WorkspaceObservation, SnapshotAdapterError> {
    use rustix::fs::{AtFlags, readlinkat, statat};

    let target = readlinkat(parent, name, Vec::new())
        .map_err(|error| entry_io(path, &io::Error::from(error)))?
        .into_bytes();
    let after = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| entry_io(path, &io::Error::from(error)))?;
    let identity = StatIdentity::from_stat(before)?;
    if identity != StatIdentity::from_stat(&after)? {
        return Err(entry_changed(path));
    }
    Ok(present(SnapshotEntry::symlink(target), identity))
}

fn present(entry: SnapshotEntry, identity: StatIdentity) -> WorkspaceObservation {
    let content_sha256 = Sha256::digest(entry.bytes()).into();
    WorkspaceObservation::Present {
        entry,
        stamp: WorkspaceStamp {
            identity,
            content_sha256,
        },
    }
}

fn entry_io(path: &RepositoryPath, error: &io::Error) -> SnapshotAdapterError {
    let issue = if error.kind() == io::ErrorKind::NotFound {
        WorkspaceEntryIssue::Changed
    } else {
        WorkspaceEntryIssue::Unreadable
    };
    SnapshotAdapterError::WorkspaceEntry {
        path: path.clone(),
        issue,
    }
}

fn entry_changed(path: &RepositoryPath) -> SnapshotAdapterError {
    SnapshotAdapterError::WorkspaceEntry {
        path: path.clone(),
        issue: WorkspaceEntryIssue::Changed,
    }
}

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
mod tests {
    use super::{SnapshotAdapterError, reserve_regular_capacity};

    #[test]
    fn oversized_regular_file_reservation_is_a_typed_capacity_error() {
        let mut bytes = Vec::new();
        assert!(matches!(
            reserve_regular_capacity(&mut bytes, u64::MAX),
            Err(SnapshotAdapterError::Capacity)
        ));
    }
}
