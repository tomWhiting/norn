//! Closed writer-finding issue types.

use serde::Serialize;

use crate::digest::Digest;
use crate::writers::{ClassificationIssue, UnknownSinkReason};

/// Closed writer-inventory issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownWriterIssue {
    /// A name could refer to more than one imported item.
    AmbiguousAlias,
    /// A matching terminal name came from an unresolved path.
    UnresolvedAlias,
    /// A wildcard import prevents exact resolution.
    WildcardImport,
    /// A writer-like method was called on an untracked value.
    DynamicReceiver,
    /// A writer-like method was called through generic authority.
    GenericReceiver,
    /// A raw macro token names a registered sink.
    MacroTokenCandidate,
    /// A macro definition contains a registered sink token.
    MacroDefinitionCandidate,
    /// A governed writer namespace contains an unreviewed call.
    KnownNamespaceCandidate,
    /// A registered callable escaped through an untracked argument.
    CallableEscape,
    /// Tracked writer authority was passed to an unreviewed callee.
    AuthorityArgument,
    /// An unreviewed method operated on tracked writer authority.
    AuthorityMethod,
    /// Tracked writer authority entered unsupported local storage.
    AuthorityStorage,
    /// Tracked writer authority escaped from a function return.
    AuthorityReturn,
    /// A function appears to return unregistered writer authority.
    NewWrapperCandidate,
    /// A required project wrapper definition is incompatible or duplicated.
    DefinitionMismatch,
    /// A definition-backed sink required by reviewed authority was not observed.
    UnobservedRequiredSink,
}

impl From<UnknownSinkReason> for UnknownWriterIssue {
    fn from(reason: UnknownSinkReason) -> Self {
        match reason {
            UnknownSinkReason::AmbiguousAlias => Self::AmbiguousAlias,
            UnknownSinkReason::UnresolvedAlias => Self::UnresolvedAlias,
            UnknownSinkReason::WildcardImport => Self::WildcardImport,
            UnknownSinkReason::DynamicReceiver => Self::DynamicReceiver,
            UnknownSinkReason::GenericReceiver => Self::GenericReceiver,
            UnknownSinkReason::MacroTokenCandidate => Self::MacroTokenCandidate,
            UnknownSinkReason::MacroDefinitionCandidate => Self::MacroDefinitionCandidate,
            UnknownSinkReason::KnownNamespaceCandidate => Self::KnownNamespaceCandidate,
            UnknownSinkReason::CallableEscape => Self::CallableEscape,
            UnknownSinkReason::AuthorityArgument => Self::AuthorityArgument,
            UnknownSinkReason::AuthorityMethod => Self::AuthorityMethod,
            UnknownSinkReason::AuthorityStorage => Self::AuthorityStorage,
            UnknownSinkReason::AuthorityReturn => Self::AuthorityReturn,
            UnknownSinkReason::NewWrapperCandidate => Self::NewWrapperCandidate,
            UnknownSinkReason::DefinitionMismatch => Self::DefinitionMismatch,
        }
    }
}

/// An exact writer-classification integrity issue.
///
/// Every variant carries only the operation identity available from the writer
/// classifier. Artifact family and operation kind are deliberately absent
/// because classification failures do not reliably provide them.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum WriterClassificationIssue {
    /// An inventory operation has no classification row.
    Missing {
        /// Complete stable writer-operation identity.
        operation: Digest,
    },
    /// An operation has more than one classification row.
    Duplicate {
        /// Complete stable writer-operation identity.
        operation: Digest,
    },
    /// A classification row names no current operation.
    Stale {
        /// Complete stable writer-operation identity.
        operation: Digest,
    },
    /// A shared primitive lacks two unique inbound family edges.
    SharedEdges {
        /// Complete stable writer-operation identity.
        operation: Digest,
    },
}

impl From<ClassificationIssue> for WriterClassificationIssue {
    fn from(issue: ClassificationIssue) -> Self {
        match issue {
            ClassificationIssue::Missing { operation } => Self::Missing {
                operation: operation.digest(),
            },
            ClassificationIssue::Duplicate { operation } => Self::Duplicate {
                operation: operation.digest(),
            },
            ClassificationIssue::Stale { operation } => Self::Stale {
                operation: operation.digest(),
            },
            ClassificationIssue::SharedEdges { operation } => Self::SharedEdges {
                operation: operation.digest(),
            },
        }
    }
}
