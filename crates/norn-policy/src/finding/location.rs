//! Disclosed and non-disclosing finding locations.

use serde::Serialize;

use crate::digest::Digest;
use crate::path::RepositoryPath;

/// A non-disclosing identity for one retained artifact observation.
///
/// The path itself is never retained. Preregistered paths may carry their
/// reviewed, domain-separated technical digest and their closed-registry
/// ordinal. Unregistered observations use only an observation ordinal so
/// output contains nothing derived from a potentially sensitive filename.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ArtifactIdentity {
    ordinal: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_digest: Option<Digest>,
}

impl ArtifactIdentity {
    /// Construct an identity for a preregistered, reviewed artifact path.
    #[must_use]
    pub const fn registered(ordinal: u64, path_digest: Digest) -> Self {
        Self {
            ordinal,
            path_digest: Some(path_digest),
        }
    }

    /// Construct an identity for an unregistered artifact observation.
    #[must_use]
    pub const fn observed(ordinal: u64) -> Self {
        Self {
            ordinal,
            path_digest: None,
        }
    }

    /// Return the registry ordinal or unknown-observation ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u64 {
        self.ordinal
    }

    /// Return the reviewed path digest for a registered artifact.
    #[must_use]
    pub const fn path_digest(self) -> Option<Digest> {
        self.path_digest
    }
}

/// The location authority carried by one finding.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingLocation {
    /// A repository-relative path that is intentionally safe to disclose.
    Repository {
        /// Validated repository-relative path.
        path: RepositoryPath,
    },
    /// An artifact path represented only by a non-disclosing identity.
    Artifact {
        /// Stable registered identity or ordinal-only unknown observation.
        artifact: ArtifactIdentity,
    },
}

impl FindingLocation {
    /// Return a disclosed repository path, if this location contains one.
    #[must_use]
    pub const fn path(&self) -> Option<&RepositoryPath> {
        match self {
            Self::Repository { path } => Some(path),
            Self::Artifact { .. } => None,
        }
    }

    /// Return a non-disclosing artifact identity, if present.
    #[must_use]
    pub const fn artifact(&self) -> Option<ArtifactIdentity> {
        match self {
            Self::Repository { .. } => None,
            Self::Artifact { artifact } => Some(*artifact),
        }
    }
}
