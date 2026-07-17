//! Non-mutating descriptor-relative reads for local inspection and migration.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::os::fd::OwnedFd;

use super::PrivateEntryKind;

/// Metadata for one entry in an observational private-tree walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrivateTreeEntry {
    pub(crate) path: PathBuf,
    pub(crate) kind: PrivateEntryKind,
    pub(crate) length: Option<u64>,
    pub(crate) mode: rustix::fs::RawMode,
}

/// A non-mutating private-storage root pinned for the reader lifetime.
#[derive(Debug)]
pub(crate) struct PrivateRootReader {
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    descriptor: OwnedFd,
}

impl PrivateRootReader {
    /// Open an existing absolute root without creating entries or changing modes.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        open_root(path)
    }

    /// Enumerate the complete tree in sorted relative-path order.
    ///
    /// Any symlink or non-file/non-directory entry rejects the entire walk.
    pub(crate) fn read_tree(&self) -> io::Result<Vec<PrivateTreeEntry>> {
        read_tree(self)
    }

    /// Open one known regular file without creating entries or changing modes.
    pub(crate) fn open_file(&self, relative: &Path) -> io::Result<File> {
        open_file(self, relative)
    }
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_root(path: &Path) -> io::Result<PrivateRootReader> {
    use rustix::fs::{Mode, open, openat};

    let mut directory =
        open(Path::new("/"), super::directory_flags(), Mode::empty()).map_err(io::Error::from)?;
    for component in super::absolute_components(path)? {
        directory = openat(
            &directory,
            &component,
            super::directory_flags(),
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
    }
    Ok(PrivateRootReader {
        descriptor: directory,
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_root(_path: &Path) -> io::Result<PrivateRootReader> {
    Err(super::unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_relative_directory(root: &PrivateRootReader, relative: &Path) -> io::Result<OwnedFd> {
    use rustix::fs::{Mode, openat};

    let mut directory = openat(
        &root.descriptor,
        Path::new("."),
        super::directory_flags(),
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    for component in super::relative_components(relative, true)? {
        directory = openat(
            &directory,
            component,
            super::directory_flags(),
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
    }
    Ok(directory)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_file(root: &PrivateRootReader, relative: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};

    let (parent_path, name) = super::split_file_path(relative)?;
    let parent = open_relative_directory(root, &parent_path)?;
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let descriptor = openat(&parent, name, flags, Mode::empty()).map_err(io::Error::from)?;
    let file = File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return Err(super::non_regular());
    }
    Ok(file)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_file(_root: &PrivateRootReader, _relative: &Path) -> io::Result<File> {
    Err(super::unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn read_tree(root: &PrivateRootReader) -> io::Result<Vec<PrivateTreeEntry>> {
    let mut pending = vec![PathBuf::new()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in read_directory(root, &directory)? {
            if entry.kind == PrivateEntryKind::Directory {
                pending.push(entry.path.clone());
            }
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn read_tree(_root: &PrivateRootReader) -> io::Result<Vec<PrivateTreeEntry>> {
    Err(super::unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn read_directory(root: &PrivateRootReader, relative: &Path) -> io::Result<Vec<PrivateTreeEntry>> {
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
        let directory_fd = directory.fd().map_err(io::Error::from)?;
        let stat = statat(directory_fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io::Error::from)?;
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => PrivateEntryKind::File,
            FileType::Directory => PrivateEntryKind::Directory,
            _ => return Err(unsupported_tree_entry(relative, bytes)),
        };
        let path = relative.join(std::ffi::OsString::from_vec(bytes.to_vec()));
        let length = if kind == PrivateEntryKind::File {
            Some(u64::try_from(stat.st_size).map_err(|error| invalid_file_length(&path, error))?)
        } else {
            None
        };
        entries.push(PrivateTreeEntry {
            path,
            kind,
            length,
            mode: stat.st_mode,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn unsupported_tree_entry(parent: &Path, name: &[u8]) -> io::Error {
    use std::os::unix::ffi::OsStringExt as _;

    let path = parent.join(std::ffi::OsString::from_vec(name.to_vec()));
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "observational private-tree reads reject links and special entries: {}",
            path.display()
        ),
    )
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn invalid_file_length(path: &Path, source: std::num::TryFromIntError) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "private regular file has an invalid length at {}: {source}",
            path.display(),
        ),
    )
}

/// Open an existing regular file without creating entries or changing modes.
pub(super) fn open_regular_file(root: &Path, relative: &Path) -> io::Result<File> {
    PrivateRootReader::open(root)?.open_file(relative)
}
