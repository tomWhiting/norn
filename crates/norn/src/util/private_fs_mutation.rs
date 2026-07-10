//! Descriptor-relative mutation helpers for private storage.

use std::io;
use std::path::Path;

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use super::{
    FileAccess, PrivateEntryKind, PrivateRoot, non_regular, open_file_at, open_relative_directory,
    read_directory, split_file_path,
};
#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
use super::{PrivateRoot, unsupported};

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn remove_relative_file(root: &PrivateRoot, relative: &Path) -> io::Result<()> {
    remove_relative_file_after(root, relative, || {})
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn remove_relative_file_after(
    root: &PrivateRoot,
    relative: &Path,
    before_validation: impl FnOnce(),
) -> io::Result<()> {
    remove_relative_file_with_hooks(root, relative, before_validation, || {})
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn remove_relative_file_with_hooks(
    root: &PrivateRoot,
    relative: &Path,
    before_validation: impl FnOnce(),
    before_mutation: impl FnOnce(),
) -> io::Result<()> {
    use rustix::fs::{AtFlags, unlinkat};

    let (parent_path, name) = split_file_path(relative)?;
    let parent = open_relative_directory(root, &parent_path)?;
    before_validation();
    drop(open_file_at(&parent, name, FileAccess::Read)?);
    before_mutation();
    unlinkat(&parent, name, AtFlags::empty()).map_err(io::Error::from)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
pub(super) fn remove_relative_file(_root: &PrivateRoot, _relative: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn remove_relative_directory(root: &PrivateRoot, relative: &Path) -> io::Result<()> {
    use rustix::fs::{AtFlags, unlinkat};

    let entries = read_directory(root, relative)?;
    for entry in entries {
        let child = relative.join(&entry.name);
        match entry.kind {
            PrivateEntryKind::File => remove_relative_file(root, &child)?,
            PrivateEntryKind::Directory => remove_relative_directory(root, &child)?,
            PrivateEntryKind::Other => return Err(non_regular()),
        }
    }
    let (parent_path, name) = split_file_path(relative)?;
    let parent = open_relative_directory(root, &parent_path)?;
    unlinkat(&parent, name, AtFlags::REMOVEDIR).map_err(io::Error::from)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
pub(super) fn remove_relative_directory(_root: &PrivateRoot, _relative: &Path) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn rename_relative_file(root: &PrivateRoot, from: &Path, to: &Path) -> io::Result<()> {
    rename_relative_file_after(root, from, to, || {})
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn rename_relative_file_after(
    root: &PrivateRoot,
    from: &Path,
    to: &Path,
    before_validation: impl FnOnce(),
) -> io::Result<()> {
    rename_relative_file_with_hooks(root, from, to, before_validation, || {})
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn rename_relative_file_with_hooks(
    root: &PrivateRoot,
    from: &Path,
    to: &Path,
    before_validation: impl FnOnce(),
    before_mutation: impl FnOnce(),
) -> io::Result<()> {
    use rustix::fs::renameat;

    let (from_parent_path, from_name) = split_file_path(from)?;
    let (to_parent_path, to_name) = split_file_path(to)?;
    let from_parent = open_relative_directory(root, &from_parent_path)?;
    let to_parent = open_relative_directory(root, &to_parent_path)?;
    before_validation();
    drop(open_file_at(&from_parent, from_name, FileAccess::Read)?);
    match open_file_at(&to_parent, to_name, FileAccess::Read) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    before_mutation();
    renameat(&from_parent, from_name, &to_parent, to_name).map_err(io::Error::from)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
pub(super) fn rename_relative_file(
    _root: &PrivateRoot,
    _from: &Path,
    _to: &Path,
) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn publish_new_relative_file(
    root: &PrivateRoot,
    from: &Path,
    to: &Path,
) -> io::Result<()> {
    publish_new_relative_file_after(root, from, to, || {})
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn publish_new_relative_file_after(
    root: &PrivateRoot,
    from: &Path,
    to: &Path,
    before_validation: impl FnOnce(),
) -> io::Result<()> {
    publish_new_relative_file_with_hooks(root, from, to, before_validation, || {})
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
pub(super) fn publish_new_relative_file_with_hooks(
    root: &PrivateRoot,
    from: &Path,
    to: &Path,
    before_validation: impl FnOnce(),
    before_mutation: impl FnOnce(),
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    use rustix::fs::{AtFlags, unlinkat};

    let (from_parent_path, from_name) = split_file_path(from)?;
    let (to_parent_path, to_name) = split_file_path(to)?;
    let from_parent = open_relative_directory(root, &from_parent_path)?;
    let to_parent = open_relative_directory(root, &to_parent_path)?;
    before_validation();
    let source = open_file_at(&from_parent, from_name, FileAccess::Read)?;
    before_mutation();
    rustix::fs::linkat(
        &from_parent,
        from_name,
        &to_parent,
        to_name,
        AtFlags::empty(),
    )
    .map_err(io::Error::from)?;
    let published = open_file_at(&to_parent, to_name, FileAccess::Read);
    let valid = published.as_ref().is_ok_and(|file| {
        let source_metadata = source.metadata();
        let published_metadata = file.metadata();
        matches!((source_metadata, published_metadata), (Ok(left), Ok(right)) if left.dev() == right.dev() && left.ino() == right.ino())
    });
    if valid {
        unlinkat(&from_parent, from_name, AtFlags::empty()).map_err(io::Error::from)?;
        return Ok(());
    }
    if let Err(error) = unlinkat(&to_parent, to_name, AtFlags::empty()) {
        tracing::warn!(
            path = %root.display_path(to).display(),
            %error,
            "failed to remove invalid private file publication",
        );
    }
    Err(non_regular())
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
pub(super) fn publish_new_relative_file(
    _root: &PrivateRoot,
    _from: &Path,
    _to: &Path,
) -> io::Result<()> {
    Err(unsupported())
}
