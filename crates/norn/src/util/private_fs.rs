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

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
use mutation::rename_new_relative_file_with_hooks;
use mutation::{
    publish_new_relative_file, remove_relative_directory, remove_relative_file,
    rename_new_relative_file, rename_relative_file,
};
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

impl PrivateRoot {
    /// Open or create an absolute private root and enforce mode `0700`.
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        create_root(path)
    }

    /// Create the root and durably publish every absolute ancestor entry.
    pub(crate) fn create_with_durable_ancestors(path: &Path) -> io::Result<Self> {
        create_root_with_durable_ancestors(path)
    }

    /// Open an existing absolute private root and enforce mode `0700`.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        open_root(path)
    }

    /// Observe one existing regular file without creating or changing modes.
    pub(crate) fn open_read_observational(root: &Path, relative: &Path) -> io::Result<File> {
        observation::open_regular_file(root, relative)
    }

    /// The original absolute spelling supplied by the caller.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Render a relative path below the root for diagnostics only.
    pub(crate) fn display_path(&self, relative: &Path) -> PathBuf {
        self.path.join(relative)
    }

    /// Create and harden all relative directory components to `0700`.
    pub(crate) fn create_dir_all(&self, relative: &Path) -> io::Result<()> {
        create_relative_directory(self, relative).map(drop)
    }

    /// Open an existing private regular file for reading.
    pub(crate) fn open_read(&self, relative: &Path) -> io::Result<File> {
        open_file(self, relative, FileAccess::Read)
    }

    /// Open an existing private regular file for reading and appending.
    pub(crate) fn open_read_append(&self, relative: &Path) -> io::Result<File> {
        open_file(self, relative, FileAccess::ReadAppend)
    }

    /// Open or create a private regular append-only file.
    pub(crate) fn open_append_create(&self, relative: &Path) -> io::Result<File> {
        open_file(self, relative, FileAccess::AppendCreate)
    }

    /// Open or create a private regular advisory-lock file.
    pub(crate) fn open_lock(&self, relative: &Path) -> io::Result<File> {
        open_file(self, relative, FileAccess::Lock)
    }

    /// Exclusively create a private regular file.
    pub(crate) fn create_new(&self, relative: &Path) -> io::Result<File> {
        open_file(self, relative, FileAccess::CreateNew)
    }

    /// Return whether `relative` securely resolves to a regular file.
    pub(crate) fn regular_file_exists(&self, relative: &Path) -> io::Result<bool> {
        match self.open_read(relative) {
            Ok(file) => {
                drop(file);
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Enumerate an existing directory through a pinned descriptor.
    pub(crate) fn read_dir(&self, relative: &Path) -> io::Result<Vec<PrivateDirEntry>> {
        read_directory(self, relative)
    }

    /// Remove a relative regular-file entry without following it.
    pub(crate) fn remove_file(&self, relative: &Path) -> io::Result<()> {
        remove_relative_file(self, relative)
    }

    /// Remove a relative directory tree without following any entry.
    pub(crate) fn remove_dir_all(&self, relative: &Path) -> io::Result<()> {
        remove_relative_directory(self, relative)
    }

    /// Atomically rename one regular file over another within this root.
    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_relative_file(self, from, to)
    }

    /// Atomically rename one regular file to an unoccupied name.
    pub(crate) fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_new_relative_file(self, from, to)
    }

    /// Atomically publish an existing regular file at an unoccupied name.
    ///
    /// Supported POSIX-style Unix targets use descriptor-relative `linkat`.
    /// Redox, ESP-IDF, and non-Unix targets fail closed as unsupported.
    pub(crate) fn publish_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        publish_new_relative_file(self, from, to)
    }

    /// Synchronize a relative directory's metadata and entries.
    pub(crate) fn sync_dir(&self, relative: &Path) -> io::Result<()> {
        sync_relative_directory(self, relative)
    }
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

    let mut directory =
        rustix::io::fcntl_dupfd_cloexec(&root.descriptor, 0).map_err(io::Error::from)?;
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

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[path = "private_fs_durability_tests.rs"]
mod durability_tests;
