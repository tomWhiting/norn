//! No-follow atomic mutation of the two workspace MCP settings documents.

use std::io;
use std::path::Path;

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::fs::File;
#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::io::{Read, Write};
#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::os::fd::OwnedFd;
#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
use std::path::Component;

/// Workspace settings document accepted by the narrow mutation boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceSettingsFile {
    Shared,
    Local,
}

impl WorkspaceSettingsFile {
    pub(super) const fn file_name(self) -> &'static str {
        match self {
            Self::Shared => "settings.json",
            Self::Local => "settings.local.json",
        }
    }
}

/// Locked descriptor-pinned workspace settings document.
pub(super) struct WorkspaceSettingsDocument {
    project_root: std::path::PathBuf,
    kind: WorkspaceSettingsFile,
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    directory: OwnedFd,
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    lock: File,
}

impl WorkspaceSettingsDocument {
    pub(super) fn open(project_root: &Path, kind: WorkspaceSettingsFile) -> io::Result<Self> {
        open_document(project_root, kind)
    }

    pub(super) fn display_path(&self) -> std::path::PathBuf {
        self.project_root.join(".norn").join(self.kind.file_name())
    }

    pub(super) fn read(&self) -> io::Result<Option<String>> {
        read_document(self)
    }

    pub(super) fn replace(&self, bytes: &[u8]) -> io::Result<()> {
        replace_document(self, bytes)
    }
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
impl Drop for WorkspaceSettingsDocument {
    fn drop(&mut self) {
        if let Err(error) = self.lock.unlock() {
            tracing::warn!(
                path = %self.display_path().display(),
                %error,
                "failed to explicitly unlock workspace MCP settings directory",
            );
        }
    }
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
fn open_document(
    project_root: &Path,
    kind: WorkspaceSettingsFile,
) -> io::Result<WorkspaceSettingsDocument> {
    use rustix::fs::{Mode, mkdirat, open, openat};
    use rustix::io::Errno;

    if !project_root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace MCP settings root must be absolute",
        ));
    }
    let mut directory =
        open(Path::new("/"), directory_flags(), Mode::empty()).map_err(io::Error::from)?;
    for component in project_root.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = openat(&directory, name, directory_flags(), Mode::empty())
                    .map_err(io::Error::from)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace MCP settings root must be normalized",
                ));
            }
        }
    }
    directory = match openat(&directory, ".norn", directory_flags(), Mode::empty()) {
        Ok(opened) => opened,
        Err(Errno::NOENT) => {
            match mkdirat(&directory, ".norn", Mode::from_raw_mode(0o777)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            openat(&directory, ".norn", directory_flags(), Mode::empty())
                .map_err(io::Error::from)?
        }
        Err(error) => return Err(io::Error::from(error)),
    };
    let lock = File::from(rustix::io::fcntl_dupfd_cloexec(&directory, 0).map_err(io::Error::from)?);
    lock.lock()?;
    Ok(WorkspaceSettingsDocument {
        project_root: project_root.to_path_buf(),
        kind,
        directory,
        lock,
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn open_document(
    project_root: &Path,
    kind: WorkspaceSettingsFile,
) -> io::Result<WorkspaceSettingsDocument> {
    let _unused = (project_root, kind);
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn read_document(document: &WorkspaceSettingsDocument) -> io::Result<Option<String>> {
    use rustix::fs::{Mode, OFlags, openat};
    use rustix::io::Errno;

    let descriptor = match openat(
        &document.directory,
        document.kind.file_name(),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => return Err(io::Error::from(error)),
    };
    let mut file = File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return Err(non_regular());
    }
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(Some(content))
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn read_document(_document: &WorkspaceSettingsDocument) -> io::Result<Option<String>> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn replace_document(document: &WorkspaceSettingsDocument, bytes: &[u8]) -> io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, fchmod, fstat, openat, renameat, unlinkat};
    use rustix::io::Errno;

    let target_name = document.kind.file_name();
    let existing_mode = match openat(
        &document.directory,
        target_name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => {
            let file = File::from(descriptor);
            if !file.metadata()?.file_type().is_file() {
                return Err(non_regular());
            }
            Some(Mode::from_raw_mode(
                fstat(&file).map_err(io::Error::from)?.st_mode & 0o777,
            ))
        }
        Err(Errno::NOENT) => None,
        Err(error) => return Err(io::Error::from(error)),
    };
    let temporary = format!(".{target_name}.mcp.tmp.{}", uuid::Uuid::new_v4());
    let descriptor = openat(
        &document.directory,
        temporary.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o666),
    )
    .map_err(io::Error::from)?;
    let mut file = File::from(descriptor);
    let result = (|| {
        if let Some(mode) = existing_mode {
            fchmod(&file, mode).map_err(io::Error::from)?;
        }
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        renameat(
            &document.directory,
            temporary.as_str(),
            &document.directory,
            target_name,
        )
        .map_err(io::Error::from)?;
        File::from(
            rustix::io::fcntl_dupfd_cloexec(&document.directory, 0).map_err(io::Error::from)?,
        )
        .sync_all()
    })();
    if result.is_err()
        && let Err(error) = unlinkat(&document.directory, temporary.as_str(), AtFlags::empty())
        && error != Errno::NOENT
    {
        tracing::warn!(
            path = %document.display_path().display(),
            temporary,
            %error,
            "failed to remove temporary workspace MCP settings file",
        );
    }
    result
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn replace_document(_document: &WorkspaceSettingsDocument, _bytes: &[u8]) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn non_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "workspace MCP settings target must be a regular file without links",
    )
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "workspace MCP settings mutation requires descriptor-capable Unix",
    )
}

#[cfg(test)]
#[path = "mcp_workspace_write_tests.rs"]
mod tests;
