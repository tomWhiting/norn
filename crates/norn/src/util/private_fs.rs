//! Descriptor-pinned private storage on supported descriptor-capable Unix
//! targets, rooted outside the workspace. Other targets fail closed.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::os::fd::OwnedFd;

#[path = "private_fs_mutation.rs"]
mod mutation;
#[path = "private_fs_observation.rs"]
mod observation;
#[path = "private_fs_api.rs"]
mod private_root_api;

pub(crate) use observation::{PrivateRootReader, PrivateTreeEntry};

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
use mutation::rename_new_relative_file_with_hooks;
#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
use mutation::{
    publish_new_relative_file_after, publish_new_relative_file_with_hooks,
    remove_relative_file_after, remove_relative_file_with_hooks, rename_relative_file_after,
    rename_relative_file_with_hooks,
};

/// Kind of an entry enumerated below a [`PrivateRoot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateEntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symlink, device, socket, or other unsupported inode type.
    Other,
}

/// One descriptor-relative directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrivateDirEntry {
    pub(crate) name: OsString,
    pub(crate) kind: PrivateEntryKind,
}

/// Validate one unencoded private-storage path component.
pub fn validate_private_component<'a>(value: &'a str, label: &str) -> io::Result<&'a str> {
    let components = Path::new(value).components().collect::<Vec<_>>();
    let is_one_normal = matches!(
        components.as_slice(),
        [Component::Normal(component)] if component == &OsStr::new(value)
    );
    if !is_one_normal || value.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{label} must be one non-empty normal path component without control characters"
            ),
        ));
    }
    Ok(value)
}

/// An absolute private-storage root pinned by an open directory descriptor.
///
/// Unix opens every absolute ancestor with `O_NOFOLLOW`. [`Self::create`]
/// creates and hardens missing ancestors, including the root, while
/// [`Self::open`] requires the complete path to exist. Descriptor-relative
/// descendants are always confined beneath the pinned root. Platforms without
/// equivalent descriptor-relative primitives fail closed.
#[derive(Debug)]
pub(crate) struct PrivateRoot {
    path: PathBuf,
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    descriptor: OwnedFd,
}

#[derive(Clone, Copy)]
enum FileAccess {
    Read,
    ReadAppend,
    AppendCreate,
    Lock,
    CreateNew,
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn invalid_root() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "private storage root must be an absolute normalized path",
    )
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn invalid_relative() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "private storage paths must be non-empty, relative, and normalized",
    )
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn non_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private storage targets must be regular files without links",
    )
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "private storage requires a supported descriptor-capable Unix target",
    )
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn relative_components(path: &Path, allow_empty: bool) -> io::Result<Vec<&OsStr>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value),
            Component::CurDir if allow_empty && path.as_os_str().is_empty() => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return Err(invalid_relative()),
        }
    }
    if !allow_empty && components.is_empty() {
        return Err(invalid_relative());
    }
    Ok(components)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn absolute_components(path: &Path) -> io::Result<Vec<OsString>> {
    if !path.is_absolute() {
        return Err(invalid_root());
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(invalid_root());
            }
        }
    }
    if components.is_empty() {
        return Err(invalid_root());
    }
    normalize_macos_alias(&mut components);
    Ok(components)
}

#[cfg(target_os = "macos")]
fn normalize_macos_alias(components: &mut Vec<OsString>) {
    if matches!(components.first(), Some(first) if first == "var" || first == "tmp") {
        components.insert(0, OsString::from("private"));
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "redox", target_os = "espidf"))
))]
fn normalize_macos_alias(_components: &mut Vec<OsString>) {}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn directory_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn harden_directory(directory: &OwnedFd, created: bool) -> io::Result<()> {
    use rustix::fs::{Mode, fchmod, fstat};

    let mode = if created {
        Mode::RWXU
    } else {
        Mode::from_raw_mode(fstat(directory).map_err(io::Error::from)?.st_mode) & Mode::RWXU
    };
    fchmod(directory, mode).map_err(io::Error::from)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_absolute(path: &Path, create_missing: bool) -> io::Result<OwnedFd> {
    open_absolute_with_parent_sync(path, create_missing, |_| Ok(()))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_absolute_with_parent_sync(
    path: &Path,
    create_missing: bool,
    mut sync_parent: impl FnMut(&OwnedFd) -> io::Result<()>,
) -> io::Result<OwnedFd> {
    use rustix::fs::{Mode, mkdirat, open, openat};
    use rustix::io::Errno;

    let components = absolute_components(path)?;
    let mut directory =
        open(Path::new("/"), directory_flags(), Mode::empty()).map_err(io::Error::from)?;
    let mut final_created = false;
    for component in &components {
        let mut created = false;
        let child = match openat(&directory, component, directory_flags(), Mode::empty()) {
            Ok(opened) => opened,
            Err(Errno::NOENT) if create_missing => {
                match mkdirat(&directory, component, Mode::RWXU) {
                    Ok(()) => created = true,
                    Err(Errno::EXIST) => {}
                    Err(error) => return Err(io::Error::from(error)),
                }
                openat(&directory, component, directory_flags(), Mode::empty())
                    .map_err(io::Error::from)?
            }
            Err(error) => return Err(io::Error::from(error)),
        };
        sync_parent(&directory)?;
        directory = child;
        final_created = created;
    }
    harden_directory(&directory, final_created)?;
    Ok(directory)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn create_root(path: &Path) -> io::Result<PrivateRoot> {
    Ok(PrivateRoot {
        path: path.to_path_buf(),
        descriptor: open_absolute(path, true)?,
    })
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn create_root_with_durable_ancestors(path: &Path) -> io::Result<PrivateRoot> {
    Ok(PrivateRoot {
        path: path.to_path_buf(),
        descriptor: open_absolute_with_parent_sync(path, true, |parent| {
            rustix::fs::fsync(parent).map_err(io::Error::from)
        })?,
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn create_root(_path: &Path) -> io::Result<PrivateRoot> {
    Err(unsupported())
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn create_root_with_durable_ancestors(_path: &Path) -> io::Result<PrivateRoot> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_root(path: &Path) -> io::Result<PrivateRoot> {
    Ok(PrivateRoot {
        path: path.to_path_buf(),
        descriptor: open_absolute(path, false)?,
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_root(_path: &Path) -> io::Result<PrivateRoot> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_relative_directory(root: &PrivateRoot, relative: &Path) -> io::Result<OwnedFd> {
    use rustix::fs::{Mode, openat};

    let mut directory = openat(
        &root.descriptor,
        Path::new("."),
        directory_flags(),
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    for component in relative_components(relative, true)? {
        directory = openat(&directory, component, directory_flags(), Mode::empty())
            .map_err(io::Error::from)?;
        harden_directory(&directory, false)?;
    }
    Ok(directory)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn create_relative_directory(root: &PrivateRoot, relative: &Path) -> io::Result<OwnedFd> {
    use rustix::fs::{Mode, mkdirat, openat};
    use rustix::io::Errno;

    let mut directory =
        rustix::io::fcntl_dupfd_cloexec(&root.descriptor, 0).map_err(io::Error::from)?;
    for component in relative_components(relative, true)? {
        let mut created = false;
        directory = match openat(&directory, component, directory_flags(), Mode::empty()) {
            Ok(opened) => opened,
            Err(Errno::NOENT) => {
                match mkdirat(&directory, component, Mode::RWXU) {
                    Ok(()) => created = true,
                    Err(Errno::EXIST) => {}
                    Err(error) => return Err(io::Error::from(error)),
                }
                openat(&directory, component, directory_flags(), Mode::empty())
                    .map_err(io::Error::from)?
            }
            Err(error) => return Err(io::Error::from(error)),
        };
        harden_directory(&directory, created)?;
    }
    Ok(directory)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn create_relative_directory(_root: &PrivateRoot, _relative: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn split_file_path(path: &Path) -> io::Result<(PathBuf, &OsStr)> {
    let components = relative_components(path, false)?;
    let (name, parents) = components.split_last().ok_or_else(invalid_relative)?;
    Ok((parents.iter().collect(), name))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_file(root: &PrivateRoot, relative: &Path, access: FileAccess) -> io::Result<File> {
    let (parent_path, name) = split_file_path(relative)?;
    let parent = match access {
        FileAccess::AppendCreate | FileAccess::Lock | FileAccess::CreateNew => {
            create_relative_directory(root, &parent_path)?
        }
        FileAccess::Read | FileAccess::ReadAppend => open_relative_directory(root, &parent_path)?,
    };
    open_file_at(&parent, name, access)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_file_at(parent: &OwnedFd, name: &OsStr, access: FileAccess) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, fchmod, openat};

    let mut flags = OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    flags |= match access {
        FileAccess::Read => OFlags::RDONLY,
        FileAccess::ReadAppend => OFlags::RDWR | OFlags::APPEND,
        FileAccess::AppendCreate => OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
        FileAccess::Lock => OFlags::WRONLY | OFlags::CREATE,
        FileAccess::CreateNew => OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
    };
    let file_mode = Mode::RUSR | Mode::WUSR;
    #[cfg(target_os = "macos")]
    let descriptor = openat_with_macos_create_retry(flags.contains(OFlags::CREATE), || {
        openat(parent, name, flags, file_mode)
    })
    .map_err(io::Error::from)?;
    #[cfg(not(target_os = "macos"))]
    let descriptor = openat(parent, name, flags, file_mode).map_err(io::Error::from)?;
    let file = File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return Err(non_regular());
    }
    fchmod(&file, file_mode).map_err(io::Error::from)?;
    Ok(file)
}

/// Retry one macOS-only transient failure observed for concurrent same-name
/// descriptor-relative creates on Darwin 25.3/APFS.
///
/// The retained Gate D reproducer observed every `ENOENT` converging on the
/// immediately following call. One retry is therefore the smallest
/// evidence-backed correction. Extending the bound requires new retained
/// evidence. Non-create operations and every other error return immediately.
#[cfg(target_os = "macos")]
fn openat_with_macos_create_retry<T>(
    creates_file: bool,
    mut open: impl FnMut() -> Result<T, rustix::io::Errno>,
) -> Result<T, rustix::io::Errno> {
    match open() {
        Err(rustix::io::Errno::NOENT) if creates_file => open(),
        result => result,
    }
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_file(_root: &PrivateRoot, _relative: &Path, _access: FileAccess) -> io::Result<File> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn read_directory(root: &PrivateRoot, relative: &Path) -> io::Result<Vec<PrivateDirEntry>> {
    use std::os::unix::ffi::OsStringExt as _;

    use rustix::fs::{AtFlags, Dir, FileType, statat};

    let descriptor = open_relative_directory(root, relative)?;
    let mut directory = Dir::new(descriptor).map_err(io::Error::from)?;
    let mut entries = Vec::new();
    while let Some(entry) = directory.read() {
        let entry = entry.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let file_type = if entry.file_type() == FileType::Unknown {
            let directory_fd = directory.fd().map_err(io::Error::from)?;
            let stat = statat(directory_fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            FileType::from_raw_mode(stat.st_mode)
        } else {
            entry.file_type()
        };
        let kind = match file_type {
            FileType::RegularFile => PrivateEntryKind::File,
            FileType::Directory => PrivateEntryKind::Directory,
            _ => PrivateEntryKind::Other,
        };
        entries.push(PrivateDirEntry {
            name: OsString::from_vec(bytes.to_vec()),
            kind,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn read_directory(_root: &PrivateRoot, _relative: &Path) -> io::Result<Vec<PrivateDirEntry>> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn sync_relative_directory(root: &PrivateRoot, relative: &Path) -> io::Result<()> {
    File::from(open_relative_directory(root, relative)?).sync_all()
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn sync_relative_directory(_root: &PrivateRoot, _relative: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(test)]
#[path = "private_fs_tests.rs"]
mod tests;

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "private_fs_rename_tests.rs"]
mod rename_tests;

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "private_fs_directory_publish_tests.rs"]
mod directory_publish_tests;

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[path = "private_fs_durability_tests.rs"]
mod durability_tests;

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[path = "private_fs_observation_tests.rs"]
mod observation_tests;
