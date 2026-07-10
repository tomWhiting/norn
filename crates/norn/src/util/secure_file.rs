//! Race-resistant reads of repository-controlled regular files on supported
//! descriptor-capable Unix targets. Other targets fail closed.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path};
use std::time::SystemTime;

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::os::fd::OwnedFd;

/// Text and metadata obtained from the same securely opened file.
pub(crate) struct WorkspaceTextFile {
    pub(crate) content: String,
    pub(crate) modified: Option<SystemTime>,
}

/// File kind reported by a securely opened workspace directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceEntryKind {
    File,
    Directory,
    Other,
}

/// One entry enumerated through a pinned workspace directory descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceDirectoryEntry {
    pub(crate) name: OsString,
    pub(crate) kind: WorkspaceEntryKind,
}

/// Reads a UTF-8 regular file beneath `root` without following symlinks.
///
/// `relative` must contain only normal path components. On supported
/// descriptor-capable Unix targets, every component, including every component
/// of `root`, is opened relative to the previously opened directory with
/// `O_NOFOLLOW`; the final descriptor is verified as a regular file before it
/// is read. Other targets reject every present workspace input with
/// [`io::ErrorKind::Unsupported`].
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an absolute, empty, or
/// parent-traversing path; [`io::ErrorKind::PermissionDenied`] for a symlink
/// or non-regular target; and the underlying I/O error otherwise.
pub(crate) fn read_workspace_text_file(
    root: &Path,
    relative: &Path,
) -> io::Result<WorkspaceTextFile> {
    let mut file = open_workspace_regular_file(root, relative)?;
    let modified = file.metadata()?.modified().ok();
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(WorkspaceTextFile { content, modified })
}

/// Returns the modification time of a securely opened workspace file.
///
/// # Errors
///
/// Returns the same errors as [`read_workspace_text_file`].
pub(crate) fn workspace_file_mtime(root: &Path, relative: &Path) -> io::Result<Option<SystemTime>> {
    open_workspace_regular_file(root, relative)?
        .metadata()
        .map(|metadata| metadata.modified().ok())
}

/// Verifies that a workspace path is a securely opened regular file without
/// reading its contents.
///
/// # Errors
///
/// Returns the same errors as [`read_workspace_text_file`].
pub(crate) fn validate_workspace_regular_file(root: &Path, relative: &Path) -> io::Result<()> {
    open_workspace_regular_file(root, relative).map(drop)
}

/// Returns a candidate's path relative to the canonical workspace root.
///
/// Besides direct lexical containment, this recognizes alternate spellings of
/// an existing ancestor (for example macOS `/var` versus `/private/var`). The
/// candidate itself is never canonicalized into the returned path, so a final
/// or intermediate symlink remains visible to the no-follow open.
pub(crate) fn workspace_relative_path(root: &Path, candidate: &Path) -> Option<std::path::PathBuf> {
    if let Ok(relative) = candidate.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    if !candidate.is_absolute() {
        return None;
    }
    let mut relative = None;
    for ancestor in candidate.ancestors() {
        if let Ok(resolved) = ancestor.canonicalize()
            && resolved == root
            && let Ok(suffix) = candidate.strip_prefix(ancestor)
        {
            // Keep the outermost spelling of the workspace root. An inner
            // symlink may also resolve to `root`; returning early would drop
            // that symlink from the suffix and bypass the no-follow open.
            relative = Some(suffix.to_path_buf());
        }
    }
    relative
}

/// Enumerates a workspace directory through the same no-follow descriptor
/// chain used for file reads.
///
/// # Errors
///
/// Returns the same path-validation and symlink errors as
/// [`read_workspace_text_file`], or the underlying directory iteration error.
pub(crate) fn read_workspace_directory(
    root: &Path,
    relative: &Path,
) -> io::Result<Vec<WorkspaceDirectoryEntry>> {
    read_workspace_directory_impl(root, relative)
}

fn normal_components(relative: &Path) -> io::Result<Vec<&OsStr>> {
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => components.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace-controlled file path must be relative and cannot traverse parents",
                ));
            }
        }
    }
    if components.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace-controlled file path cannot be empty",
        ));
    }
    Ok(components)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn non_regular_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "workspace-controlled input must be a regular file without symlinks",
    )
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn read_workspace_directory_impl(
    root: &Path,
    relative: &Path,
) -> io::Result<Vec<WorkspaceDirectoryEntry>> {
    use std::os::unix::ffi::OsStringExt;

    use rustix::fs::{AtFlags, Dir, FileType, statat};

    let descriptor = open_workspace_directory(root, relative)?;
    let mut directory = Dir::new(descriptor).map_err(io::Error::from)?;
    let mut entries = Vec::new();
    while let Some(entry) = directory.read() {
        let entry = entry.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let kind = classify_workspace_entry(entry.file_type(), || {
            let directory_fd = directory.fd().map_err(io::Error::from)?;
            let metadata = statat(directory_fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            Ok(FileType::from_raw_mode(metadata.st_mode))
        })?;
        entries.push(WorkspaceDirectoryEntry {
            name: OsString::from_vec(bytes.to_vec()),
            kind,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn classify_workspace_entry(
    file_type: rustix::fs::FileType,
    resolve_unknown: impl FnOnce() -> io::Result<rustix::fs::FileType>,
) -> io::Result<WorkspaceEntryKind> {
    use rustix::fs::FileType;

    let resolved = if file_type == FileType::Unknown {
        resolve_unknown()?
    } else {
        file_type
    };
    Ok(match resolved {
        FileType::RegularFile => WorkspaceEntryKind::File,
        FileType::Directory => WorkspaceEntryKind::Directory,
        _ => WorkspaceEntryKind::Other,
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn read_workspace_directory_impl(
    root: &Path,
    relative: &Path,
) -> io::Result<Vec<WorkspaceDirectoryEntry>> {
    fail_closed_without_nofollow(root, relative, true)?;
    Err(no_nofollow_error())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_workspace_regular_file(root: &Path, relative: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};

    const FILE_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::CLOEXEC)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK);

    let components = normal_components(relative)?;
    let (file_name, parent_components) = components
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty workspace path"))?;
    let parent = parent_components.iter().collect::<std::path::PathBuf>();
    let directory = open_workspace_directory(root, &parent)?;

    let descriptor =
        openat(&directory, *file_name, FILE_FLAGS, Mode::empty()).map_err(io::Error::from)?;
    let file = File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return Err(non_regular_error());
    }
    Ok(file)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_workspace_regular_file(root: &Path, relative: &Path) -> io::Result<File> {
    fail_closed_without_nofollow(root, relative, false)?;
    Err(no_nofollow_error())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn open_workspace_directory(root: &Path, relative: &Path) -> io::Result<OwnedFd> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW;
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace root must be absolute",
        ));
    }
    let mut directory = open(Path::new("/"), flags, Mode::empty()).map_err(io::Error::from)?;
    for component in root.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory =
                    openat(&directory, name, flags, Mode::empty()).map_err(io::Error::from)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace root must be an absolute normalized path",
                ));
            }
        }
    }
    for component in normal_components_allow_empty(relative)? {
        directory = openat(&directory, component, flags, Mode::empty()).map_err(io::Error::from)?;
    }
    Ok(directory)
}

fn normal_components_allow_empty(relative: &Path) -> io::Result<Vec<&OsStr>> {
    if relative.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    normal_components(relative)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn fail_closed_without_nofollow(root: &Path, relative: &Path, allow_empty: bool) -> io::Result<()> {
    let components = if allow_empty {
        normal_components_allow_empty(relative)?
    } else {
        normal_components(relative)?
    };
    let candidate = components
        .iter()
        .fold(root.to_path_buf(), |mut path, part| {
            path.push(part);
            path
        });
    match std::fs::symlink_metadata(candidate) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(error),
        Ok(_) | Err(_) => Err(no_nofollow_error()),
    }
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn no_nofollow_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "secure workspace reads require a supported descriptor-capable Unix target",
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    fn reads_regular_file_and_metadata() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("nested"))?;
        std::fs::write(
            root.path().join("nested/input.md"),
            "trusted workspace text",
        )?;

        let launch_root = root.path().canonicalize()?;
        let loaded = read_workspace_text_file(&launch_root, Path::new("nested/input.md"))?;

        assert_eq!(loaded.content, "trusted workspace text");
        assert!(loaded.modified.is_some());
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    #[test]
    fn enumerates_a_pinned_directory_without_following_entries()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("items"))?;
        std::fs::write(root.path().join("items/file.md"), "content")?;
        std::fs::create_dir(root.path().join("items/nested"))?;

        let launch_root = root.path().canonicalize()?;
        let mut entries = read_workspace_directory(&launch_root, Path::new("items"))?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, OsStr::new("file.md"));
        assert_eq!(entries[0].kind, WorkspaceEntryKind::File);
        assert_eq!(entries[1].name, OsStr::new("nested"));
        assert_eq!(entries[1].kind, WorkspaceEntryKind::Directory);
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    #[test]
    fn resolves_unknown_directory_entry_types() -> Result<(), Box<dyn std::error::Error>> {
        use rustix::fs::FileType;

        let kind = classify_workspace_entry(FileType::Unknown, || Ok(FileType::RegularFile))?;

        assert_eq!(kind, WorkspaceEntryKind::File);
        Ok(())
    }

    #[test]
    fn rejects_parent_absolute_and_non_regular_paths() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("directory"))?;
        let launch_root = root.path().canonicalize()?;

        let Err(parent_error) = read_workspace_text_file(&launch_root, Path::new("../outside"))
        else {
            return Err("parent traversal was accepted".into());
        };
        assert_eq!(parent_error.kind(), io::ErrorKind::InvalidInput);
        let Err(absolute_error) =
            read_workspace_text_file(&launch_root, Path::new("/absolute/path"))
        else {
            return Err("absolute input was accepted".into());
        };
        assert_eq!(absolute_error.kind(), io::ErrorKind::InvalidInput);
        let Err(directory_error) = read_workspace_text_file(&launch_root, Path::new("directory"))
        else {
            return Err("directory input was accepted".into());
        };
        #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
        assert_eq!(directory_error.kind(), io::ErrorKind::PermissionDenied);
        #[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
        assert_eq!(directory_error.kind(), io::ErrorKind::Unsupported);
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    #[test]
    fn rejects_final_and_intermediate_symlinks() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::write(outside.path().join("secret.md"), "sentinel-private-content")?;
        symlink(
            outside.path().join("secret.md"),
            root.path().join("file-link.md"),
        )?;
        symlink(outside.path(), root.path().join("directory-link"))?;
        let launch_root = root.path().canonicalize()?;

        assert!(read_workspace_text_file(&launch_root, Path::new("file-link.md")).is_err());
        assert!(
            read_workspace_text_file(&launch_root, Path::new("directory-link/secret.md")).is_err()
        );
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    #[test]
    fn alternate_root_spelling_preserves_workspace_symlinks_in_the_suffix()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let container = tempfile::tempdir()?;
        let workspace = container.path().join("workspace");
        std::fs::create_dir(&workspace)?;
        std::fs::write(workspace.join("secret.md"), "sentinel-private-content")?;
        symlink("secret.md", workspace.join("file-link.md"))?;
        symlink(".", workspace.join("directory-link"))?;
        let launch_root = workspace.canonicalize()?;
        let alternate_root = container.path().join("alternate-workspace");
        symlink(&launch_root, &alternate_root)?;

        for candidate in [
            alternate_root.join("file-link.md"),
            alternate_root.join("directory-link/secret.md"),
        ] {
            let relative = workspace_relative_path(&launch_root, &candidate)
                .ok_or_else(|| io::Error::other("alternate root spelling was not recognized"))?;
            assert!(
                read_workspace_text_file(&launch_root, &relative).is_err(),
                "workspace symlink must remain visible in {relative:?}",
            );
        }
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    #[test]
    fn refuses_launch_root_path_replaced_by_symlink() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir()?;
        let root = parent.path().join("workspace");
        let moved = parent.path().join("moved-workspace");
        let outside = tempfile::tempdir()?;
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("input.md"), "safe")?;
        std::fs::write(outside.path().join("input.md"), "sentinel-private-root")?;
        let launch_root = root.canonicalize()?;
        let loaded = read_workspace_text_file(&launch_root, Path::new("input.md"))?;
        assert_eq!(loaded.content, "safe");

        std::fs::rename(&root, &moved)?;
        symlink(outside.path(), &root)?;

        assert!(read_workspace_text_file(&launch_root, Path::new("input.md")).is_err());
        Ok(())
    }

    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    #[test]
    fn refuses_launch_root_with_replaced_symlink_ancestor() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::symlink;

        let container = tempfile::tempdir()?;
        let ancestor = container.path().join("ancestor");
        let workspace = ancestor.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("input.md"), "original")?;
        let launch_root = workspace.canonicalize()?;

        std::fs::rename(&ancestor, container.path().join("parked"))?;
        let outside = tempfile::tempdir()?;
        std::fs::create_dir(outside.path().join("workspace"))?;
        std::fs::write(outside.path().join("workspace/input.md"), "outside")?;
        symlink(outside.path(), &ancestor)?;

        assert!(read_workspace_text_file(&launch_root, Path::new("input.md")).is_err());
        Ok(())
    }

    #[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
    #[test]
    fn present_workspace_inputs_fail_closed_without_nofollow_support()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("input.md"), "content")?;

        let Err(error) = read_workspace_text_file(root.path(), Path::new("input.md")) else {
            return Err("workspace input unexpectedly opened".into());
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        let Err(directory_error) = read_workspace_directory(root.path(), Path::new("")) else {
            return Err("workspace directory unexpectedly opened".into());
        };
        assert_eq!(directory_error.kind(), io::ErrorKind::Unsupported);
        Ok(())
    }
}
