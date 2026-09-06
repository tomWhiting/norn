//! One locked document boundary shared by persistent MCP and frontend settings.

use std::path::{Path, PathBuf};

use super::private_settings_document::PrivateSettingsDocument;
use super::settings_write::{SettingsDocumentError, SettingsPublication};
use super::workspace_settings_document::{WorkspaceSettingsDocument, WorkspaceSettingsFile};

pub(super) enum SettingsDocument {
    Private(PrivateSettingsDocument),
    Workspace(WorkspaceSettingsDocument),
}

impl SettingsDocument {
    pub(super) fn private(path: &Path) -> Result<Self, SettingsDocumentError> {
        PrivateSettingsDocument::open(path).map(Self::Private)
    }

    pub(super) fn workspace(
        root: &Path,
        kind: WorkspaceSettingsFile,
    ) -> Result<Self, SettingsDocumentError> {
        WorkspaceSettingsDocument::open(root, kind)
            .map(Self::Workspace)
            .map_err(|error| {
                SettingsDocumentError::before(
                    "open workspace settings",
                    &workspace_path(root, kind),
                    error,
                )
            })
    }

    pub(super) fn read(&self) -> Result<Option<String>, SettingsDocumentError> {
        let content = match self {
            Self::Private(document) => document.read(),
            Self::Workspace(document) => document.read().map_err(|error| {
                SettingsDocumentError::before(
                    "read workspace settings",
                    &document.display_path(),
                    error,
                )
            }),
        }?;
        #[cfg(all(test, unix))]
        super::settings_write_process_tests::after_read(&self.observer_path(), content.as_deref())
            .map_err(|error| {
                SettingsDocumentError::before(
                    "observe locked settings read",
                    &self.observer_path(),
                    error,
                )
            })?;
        Ok(content)
    }

    pub(super) fn replace(
        &self,
        bytes: &[u8],
    ) -> Result<SettingsPublication, SettingsDocumentError> {
        let publication = match self {
            Self::Private(document) => document.replace(bytes),
            Self::Workspace(document) => document.replace(bytes).map_err(|error| {
                SettingsDocumentError::before(
                    "replace workspace settings",
                    &document.display_path(),
                    error,
                )
            }),
        }?;
        #[cfg(all(test, unix))]
        let publication = match super::settings_write_process_tests::after_publish(
            &self.observer_path(),
            &publication,
        ) {
            Ok(()) => publication,
            Err(error) => {
                SettingsPublication::PublishedDurabilityUncertain(SettingsDocumentError::after(
                    "observe published settings",
                    &self.observer_path(),
                    error,
                ))
            }
        };
        Ok(publication)
    }

    #[cfg(all(test, unix))]
    fn observer_path(&self) -> PathBuf {
        match self {
            Self::Private(document) => document.observer_path().to_path_buf(),
            Self::Workspace(document) => document.display_path(),
        }
    }
}

pub(super) fn workspace_path(root: &Path, kind: WorkspaceSettingsFile) -> PathBuf {
    root.join(".norn").join(kind.file_name())
}
