//! Inherent API for descriptor-pinned private storage.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{FileAccess, PrivateDirEntry, PrivateRoot};

impl PrivateRoot {
    /// Open or create an absolute private root and enforce mode `0700`.
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        super::create_root(path)
    }

    /// Create the root and durably publish every absolute ancestor entry.
    pub(crate) fn create_with_durable_ancestors(path: &Path) -> io::Result<Self> {
        super::create_root_with_durable_ancestors(path)
    }

    /// Open an existing absolute private root and enforce mode `0700`.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        super::open_root(path)
    }

    /// Observe one existing regular file without creating or changing modes.
    pub(crate) fn open_read_observational(root: &Path, relative: &Path) -> io::Result<File> {
        super::observation::open_regular_file(root, relative)
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
        super::create_relative_directory(self, relative).map(drop)
    }

    /// Open an existing private regular file for reading.
    pub(crate) fn open_read(&self, relative: &Path) -> io::Result<File> {
        super::open_file(self, relative, FileAccess::Read)
    }

    /// Open an existing private regular file for reading and appending.
    pub(crate) fn open_read_append(&self, relative: &Path) -> io::Result<File> {
        super::open_file(self, relative, FileAccess::ReadAppend)
    }

    /// Open or create a private regular append-only file.
    pub(crate) fn open_append_create(&self, relative: &Path) -> io::Result<File> {
        super::open_file(self, relative, FileAccess::AppendCreate)
    }

    /// Open or create a private regular advisory-lock file.
    pub(crate) fn open_lock(&self, relative: &Path) -> io::Result<File> {
        super::open_file(self, relative, FileAccess::Lock)
    }

    /// Exclusively create a private regular file.
    pub(crate) fn create_new(&self, relative: &Path) -> io::Result<File> {
        super::open_file(self, relative, FileAccess::CreateNew)
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
        super::read_directory(self, relative)
    }

    /// Remove a relative regular-file entry without following it.
    pub(crate) fn remove_file(&self, relative: &Path) -> io::Result<()> {
        super::mutation::remove_relative_file(self, relative)
    }

    /// Remove a relative directory tree without following any entry.
    pub(crate) fn remove_dir_all(&self, relative: &Path) -> io::Result<()> {
        super::mutation::remove_relative_directory(self, relative)
    }

    /// Atomically rename one regular file over another within this root.
    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        super::mutation::rename_relative_file(self, from, to)
    }

    /// Atomically rename one regular file to an unoccupied name.
    pub(crate) fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        super::mutation::rename_new_relative_file(self, from, to)
    }

    /// Atomically publish an existing regular file at an unoccupied name.
    ///
    /// Supported POSIX-style Unix targets use descriptor-relative `linkat`.
    /// Redox, ESP-IDF, and non-Unix targets fail closed as unsupported.
    pub(crate) fn publish_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        super::mutation::publish_new_relative_file(self, from, to)
    }

    /// Atomically publish an existing directory at an unoccupied name.
    ///
    /// Apple, Linux, and Android targets use descriptor-relative no-replace
    /// rename. Targets without that primitive fail closed as unsupported.
    pub(crate) fn publish_new_dir(&self, from: &Path, to: &Path) -> io::Result<()> {
        super::mutation::publish_new_relative_directory(self, from, to)
    }

    /// Synchronize a relative directory's metadata and entries.
    pub(crate) fn sync_dir(&self, relative: &Path) -> io::Result<()> {
        super::sync_relative_directory(self, relative)
    }
}
