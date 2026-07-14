//! Source-preserving failures for the live MCP control plane.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::error::{ConfigError, IntegrationError, NornError};
use crate::tool::{ToolGenerationBuildError, ToolGenerationPublishError};

type SharedError = Arc<dyn Error + Send + Sync>;

/// Stable category for programmatic handling of a live MCP control failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpControlErrorKind {
    /// The actor or its runtime is unavailable.
    Unavailable,
    /// Configuration validation, loading, or persistence failed.
    Configuration,
    /// Approval storage could not be read or changed.
    Approval,
    /// The named server has no shared-project definition requiring approval.
    NotSharedProject,
    /// Runtime or tool-generation candidate construction failed.
    Candidate,
    /// Candidate publication violated a generation invariant.
    Publication,
    /// A compensating operation failed after the primary operation failed.
    Rollback,
    /// The actor and handle disagreed about their response protocol.
    Protocol,
}

/// Failure while building an immutable MCP runtime/tool candidate.
#[derive(Debug, thiserror::Error)]
pub enum McpCandidateError {
    /// The activation request contained the same logical server twice.
    #[error("activation request repeated MCP server '{name}'")]
    DuplicateServer {
        /// Repeated logical server name.
        name: String,
    },
    /// Runtime tool composition failed.
    #[error("MCP tool generation could not be assembled: {0}")]
    Generation(#[from] ToolGenerationBuildError),
    /// An injected builder could not publish its competing fixture generation.
    #[error("MCP candidate fixture publication failed: {0}")]
    Publication(ToolGenerationPublishError),
    /// Selecting tools from the connected runtime failed.
    #[error("MCP runtime selection failed: {0}")]
    Integration(#[from] IntegrationError),
    /// A notification-driven tool refresh failed.
    #[error("MCP tool refresh failed: {0}")]
    Refresh(IntegrationError),
    /// A builder returned a generation other than the requested revision.
    #[error("MCP candidate revision {actual} did not match requested revision {expected}")]
    RevisionMismatch {
        /// Revision requested by the actor.
        expected: u64,
        /// Revision returned by the builder.
        actual: u64,
    },
    /// Test or embedding candidate builder rejected the request.
    #[error("MCP candidate builder rejected the activation request: {reason}")]
    Rejected {
        /// Static diagnostic supplied by the injected builder.
        reason: &'static str,
    },
}

impl McpCandidateError {
    #[cfg(test)]
    pub(crate) const fn rejected(reason: &'static str) -> Self {
        Self::Rejected { reason }
    }
}

/// Source-preserving live MCP controller failure.
#[derive(Clone)]
pub struct McpControlError {
    kind: McpControlErrorKind,
    context: &'static str,
    source: SharedError,
}

impl McpControlError {
    pub(crate) fn unavailable(error: impl Error + Send + Sync + 'static) -> Self {
        Self::new(
            McpControlErrorKind::Unavailable,
            "the MCP control plane is unavailable",
            error,
        )
    }

    pub(crate) fn configuration(error: impl Into<NornError>) -> Self {
        Self::new(
            McpControlErrorKind::Configuration,
            "the MCP configuration operation failed",
            error.into(),
        )
    }

    pub(crate) fn approval(error: impl Error + Send + Sync + 'static) -> Self {
        Self::new(
            McpControlErrorKind::Approval,
            "the MCP approval operation failed",
            error,
        )
    }

    pub(crate) fn not_shared_project(name: &str) -> Self {
        Self::new(
            McpControlErrorKind::NotSharedProject,
            "the MCP server is not eligible for remembered shared-project approval",
            McpControlDiagnostic(format!("server '{name}' has no shared-project definition")),
        )
    }

    pub(crate) fn candidate(error: McpCandidateError) -> Self {
        Self::new(
            McpControlErrorKind::Candidate,
            "the MCP runtime candidate could not be built",
            error,
        )
    }

    pub(crate) fn publication(error: ToolGenerationPublishError) -> Self {
        Self::new(
            McpControlErrorKind::Publication,
            "the MCP runtime candidate could not be published",
            error,
        )
    }

    pub(crate) fn rollback(primary: Self, rollback: impl Error + Send + Sync + 'static) -> Self {
        Self::new(
            McpControlErrorKind::Rollback,
            "the MCP compensating operation failed",
            McpRollbackDiagnostic {
                primary,
                rollback: Arc::new(rollback),
            },
        )
    }

    pub(crate) fn refresh_recovery(refresh: Self, recovery: Self) -> Self {
        Self::new(
            McpControlErrorKind::Candidate,
            "the MCP tool refresh and client recovery failed",
            McpRefreshRecoveryDiagnostic { refresh, recovery },
        )
    }

    pub(crate) fn protocol(detail: &'static str) -> Self {
        Self::new(
            McpControlErrorKind::Protocol,
            "the MCP control plane returned an invalid response",
            McpControlDiagnostic(detail.to_owned()),
        )
    }

    pub(crate) fn approval_unavailable() -> Self {
        Self::approval(McpControlDiagnostic(
            "the shared-project approval store was unavailable when live control started"
                .to_owned(),
        ))
    }

    pub(crate) fn revision_overflow() -> Self {
        Self::publication_diagnostic("the MCP tool generation revision overflowed")
    }

    pub(crate) fn publication_diagnostic(detail: &'static str) -> Self {
        Self::new(
            McpControlErrorKind::Publication,
            "the MCP runtime candidate could not be published",
            McpControlDiagnostic(detail.to_owned()),
        )
    }

    fn new(
        kind: McpControlErrorKind,
        context: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            context,
            source: Arc::new(source),
        }
    }

    /// Stable programmatic error category.
    #[must_use]
    pub const fn kind(&self) -> McpControlErrorKind {
        self.kind
    }
}

impl fmt::Display for McpControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.context, self.source)
    }
}

impl fmt::Debug for McpControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpControlError")
            .field("kind", &self.kind)
            .field("context", &self.context)
            .field("source", &self.source)
            .finish()
    }
}

impl Error for McpControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl PartialEq for McpControlError {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for McpControlError {}

impl From<ConfigError> for McpControlError {
    fn from(error: ConfigError) -> Self {
        Self::configuration(error)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct McpControlDiagnostic(String);

#[derive(Debug)]
struct McpRollbackDiagnostic {
    primary: McpControlError,
    rollback: SharedError,
}

#[derive(Debug)]
struct McpRefreshRecoveryDiagnostic {
    refresh: McpControlError,
    recovery: McpControlError,
}

impl fmt::Display for McpRefreshRecoveryDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "recovery failed with '{}', after '{}'",
            self.recovery, self.refresh
        )
    }
}

impl Error for McpRefreshRecoveryDiagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.recovery)
    }
}

impl fmt::Display for McpRollbackDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "rollback failed with '{}', after '{}'",
            self.rollback, self.primary
        )
    }
}

impl Error for McpRollbackDiagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.rollback.as_ref())
    }
}
