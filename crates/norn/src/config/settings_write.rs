//! Publication outcomes and path-specific errors for the shared settings writers.

use std::io;
use std::path::{Path, PathBuf};

/// A filesystem failure, including whether atomic publication already happened.
#[derive(Debug, thiserror::Error)]
#[error("failed to {operation} at {path} (settings already published: {published}): {source}")]
pub struct SettingsDocumentError {
    operation: &'static str,
    path: PathBuf,
    published: bool,
    #[source]
    source: io::Error,
}

impl SettingsDocumentError {
    pub(super) fn before(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            published: false,
            source,
        }
    }

    pub(super) fn after(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            published: true,
            source,
        }
    }

    /// Exact settings document involved in the failure.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether replacement happened before this failure.
    #[must_use]
    pub const fn published(&self) -> bool {
        self.published
    }

    /// Original filesystem error, without reducing it to a message.
    #[must_use]
    pub const fn io_error(&self) -> &io::Error {
        &self.source
    }
}

/// Actual publication state; uncertainty is a committed result, never a retryable refusal.
#[derive(Debug)]
#[must_use]
pub enum SettingsPublication {
    /// The existing document already contained the requested values.
    Unchanged,
    /// Atomic replacement and the containing directory sync succeeded.
    PublishedDurable,
    /// Replacement succeeded, but directory durability could not be confirmed.
    PublishedDurabilityUncertain(SettingsDocumentError),
}

impl SettingsPublication {
    pub(super) fn after_directory_sync(path: &Path, result: io::Result<()>) -> Self {
        match result {
            Ok(()) => Self::PublishedDurable,
            Err(source) => Self::PublishedDurabilityUncertain(SettingsDocumentError::after(
                "sync settings directory",
                path,
                source,
            )),
        }
    }

    /// Preserve publication information when an existing caller requires durable success.
    pub(super) fn require_durable(self) -> Result<(), SettingsDocumentError> {
        match self {
            Self::Unchanged | Self::PublishedDurable => Ok(()),
            Self::PublishedDurabilityUncertain(error) => Err(error),
        }
    }
}
