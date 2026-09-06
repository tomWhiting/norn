//! Opaque frontend settings snapshots, source layers and typed mutation results.

use super::settings_write::{SettingsDocumentError, SettingsPublication};
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};

/// Explicit writable scope; shared project settings are never an implicit UI target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiPreferenceScope {
    /// Personal settings in the launch-resolved Norn home.
    User,
    /// Settings local to the validated launch workspace.
    WorkspaceLocal,
}

/// Winning whole-object frontend settings layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiPreferenceLayer {
    /// Personal settings.
    User,
    /// Shared project settings, read-only to frontend saves.
    SharedProject,
    /// Local workspace settings.
    WorkspaceLocal,
}

/// Raw frontend objects captured by the existing loader, before the merge consumes them.
#[derive(Clone, Default)]
pub struct TuiPreferencesLayers {
    pub(super) user: Option<Value>,
    pub(super) project: Option<Value>,
    pub(super) local: Option<Value>,
}

impl TuiPreferencesLayers {
    /// Original value in a particular layer; no per-key merging is performed.
    #[must_use]
    pub const fn value(&self, layer: TuiPreferenceLayer) -> Option<&Value> {
        match layer {
            TuiPreferenceLayer::User => self.user.as_ref(),
            TuiPreferenceLayer::SharedProject => self.project.as_ref(),
            TuiPreferenceLayer::WorkspaceLocal => self.local.as_ref(),
        }
    }

    /// The same whole-object winner used by the existing settings merger.
    #[must_use]
    pub const fn winning_layer(&self) -> Option<TuiPreferenceLayer> {
        if self.local.is_some() {
            Some(TuiPreferenceLayer::WorkspaceLocal)
        } else if self.project.is_some() {
            Some(TuiPreferenceLayer::SharedProject)
        } else if self.user.is_some() {
            Some(TuiPreferenceLayer::User)
        } else {
            None
        }
    }
}

impl fmt::Debug for TuiPreferencesLayers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TuiPreferencesLayers")
            .field("user_present", &self.user.is_some())
            .field("project_present", &self.project.is_some())
            .field("local_present", &self.local.is_some())
            .finish()
    }
}

/// Original opaque frontend data and its fixed mutation target, never full settings.
#[derive(Clone)]
pub struct TuiPreferencesSnapshot {
    pub(super) scope: TuiPreferenceScope,
    pub(super) project_root: PathBuf,
    pub(super) path: PathBuf,
    pub(super) original: Option<Value>,
}

impl TuiPreferencesSnapshot {
    /// Explicit writable layer captured by this snapshot.
    #[must_use]
    pub const fn scope(&self) -> TuiPreferenceScope {
        self.scope
    }
    /// Actual target document, independent of subsequent environment changes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Original opaque frontend object, suitable for validation by its frontend owner.
    #[must_use]
    pub const fn original(&self) -> Option<&Value> {
        self.original.as_ref()
    }
}

impl fmt::Debug for TuiPreferencesSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TuiPreferencesSnapshot")
            .field("scope", &self.scope)
            .field("project_root", &self.project_root)
            .field("path", &self.path)
            .field("tui_present", &self.original.is_some())
            .finish()
    }
}

/// The new saved baseline and the actual publication outcome, including uncertain durability.
#[derive(Debug)]
pub struct TuiPreferencesChange {
    /// Snapshot to use for the next compare-and-set, even after uncertain durability.
    pub snapshot: TuiPreferencesSnapshot,
    /// Publication state of this mutation.
    pub publication: SettingsPublication,
}

/// A refused frontend settings mutation; no atomic publication has happened.
#[derive(Debug, thiserror::Error)]
pub enum TuiPreferencesError {
    /// The selected target cannot be derived from the launch context.
    #[error("cannot resolve frontend settings target: {reason}")]
    Target {
        /// Why the target is invalid.
        reason: &'static str,
    },
    /// A present frontend value was not an object.
    #[error("tui in {path} must be a JSON object")]
    InvalidTui {
        /// Target settings document.
        path: PathBuf,
    },
    /// A settings document was not an object.
    #[error("settings document {path} must be a JSON object")]
    InvalidDocument {
        /// Target settings document.
        path: PathBuf,
    },
    /// The caller attempted an invalid owned-key patch.
    #[error("invalid frontend settings patch for {path}: {reason}")]
    InvalidPatch {
        /// Target settings document.
        path: PathBuf,
        /// Boundary refusal, without configuration values.
        reason: &'static str,
    },
    /// Another writer changed a key owned by this frontend since its snapshot.
    #[error("frontend settings conflict in {path} at tui.{key}; nothing was saved")]
    Conflict {
        /// Target settings document.
        path: PathBuf,
        /// Conflicting owned key.
        key: String,
    },
    /// Parsing or serialization failed before publication.
    #[error("frontend settings JSON failure at {path}: {source}")]
    Json {
        /// Target settings document.
        path: PathBuf,
        /// Original JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// The existing descriptor admission policy refused the operation.
    #[error("frontend settings filesystem admission failed: {0}")]
    Resource(#[from] crate::resource::DescriptorAdmissionError),
    /// A filesystem failure occurred before publication.
    #[error(transparent)]
    Filesystem(#[from] SettingsDocumentError),
}
