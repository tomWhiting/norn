//! Locked private settings documents shared by MCP and frontend preference mutations.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::settings_write::{SettingsDocumentError, SettingsPublication};
use crate::util::PrivateRoot;

pub(super) struct PrivateSettingsDocument {
    root: PrivateRoot,
    file_name: PathBuf,
    display_path: PathBuf,
    lock: File,
}

impl PrivateSettingsDocument {
    pub(super) fn open(path: &Path) -> Result<Self, SettingsDocumentError> {
        let invalid = || {
            SettingsDocumentError::before(
                "resolve private settings path",
                path,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "settings path requires a parent and file name",
                ),
            )
        };
        let parent = path.parent().ok_or_else(invalid)?;
        let file_name = path.file_name().map(PathBuf::from).ok_or_else(invalid)?;
        let root = PrivateRoot::create(parent).map_err(|error| {
            SettingsDocumentError::before("open private settings root", path, error)
        })?;
        // This physical lock is shared with existing MCP processes. Do not rename it.
        let lock = root
            .open_lock(Path::new(".mcp-settings.lock"))
            .map_err(|error| {
                SettingsDocumentError::before("open private settings lock", path, error)
            })?;
        #[cfg(all(test, unix))]
        super::settings_write_process_tests::before_lock(&lock, path).map_err(|error| {
            SettingsDocumentError::before("observe private settings lock", path, error)
        })?;
        lock.lock()
            .map_err(|error| SettingsDocumentError::before("lock private settings", path, error))?;
        Ok(Self {
            root,
            file_name,
            display_path: path.to_path_buf(),
            lock,
        })
    }

    #[cfg(all(test, unix))]
    pub(super) fn observer_path(&self) -> &Path {
        &self.display_path
    }

    pub(super) fn read(&self) -> Result<Option<String>, SettingsDocumentError> {
        let mut file = match self.root.open_read(&self.file_name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.before("open private settings", error)),
        };
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|error| self.before("read private settings", error))?;
        Ok(Some(content))
    }

    pub(super) fn replace(
        &self,
        bytes: &[u8],
    ) -> Result<SettingsPublication, SettingsDocumentError> {
        let temporary = PathBuf::from(format!(
            ".{}.mcp.tmp.{}",
            self.file_name.display(),
            uuid::Uuid::new_v4()
        ));
        let result = self.write_and_publish(&temporary, bytes);
        if result.is_err()
            && let Err(error) = self.root.remove_file(&temporary)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.display_path.display(), temporary = %temporary.display(),
                %error, "failed to remove temporary private settings file");
        }
        result
    }

    fn write_and_publish(
        &self,
        temporary: &Path,
        bytes: &[u8],
    ) -> Result<SettingsPublication, SettingsDocumentError> {
        let mut file = self
            .root
            .create_new(temporary)
            .map_err(|error| self.before("create private settings temp", error))?;
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|error| self.before("write private settings", error))?;
        drop(file);
        self.root
            .rename(temporary, &self.file_name)
            .map_err(|error| self.before("replace private settings", error))?;
        Ok(SettingsPublication::after_directory_sync(
            &self.display_path,
            self.root.sync_dir(Path::new("")),
        ))
    }

    fn before(&self, operation: &'static str, error: io::Error) -> SettingsDocumentError {
        SettingsDocumentError::before(operation, &self.display_path, error)
    }
}

impl Drop for PrivateSettingsDocument {
    fn drop(&mut self) {
        if let Err(error) = self.lock.unlock() {
            tracing::warn!(path = %self.display_path.display(), %error, "failed to explicitly unlock private settings");
        }
    }
}
