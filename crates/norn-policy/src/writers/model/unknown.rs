//! Typed unresolved writer candidates.

use serde::{Deserialize, Serialize};

/// Why a possible writer sink could not be resolved.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownSinkReason {
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
    /// A `macro_rules!` expansion body contains a registered sink token.
    MacroDefinitionCandidate,
    /// A call in a governed writer namespace has no reviewed registry row.
    KnownNamespaceCandidate,
    /// A registered writer callable escaped through an untracked argument.
    CallableEscape,
    /// Tracked writer authority was passed to an unreviewed callee.
    AuthorityArgument,
    /// An unreviewed method operated on tracked writer authority.
    AuthorityMethod,
    /// Tracked writer authority entered unsupported local storage.
    AuthorityStorage,
    /// Tracked writer authority escaped from a function return.
    AuthorityReturn,
    /// A function appears to return writer authority without registered semantics.
    NewWrapperCandidate,
    /// A required project wrapper definition is duplicated or signature-incompatible.
    DefinitionMismatch,
}

impl UnknownSinkReason {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::AmbiguousAlias => "ambiguous_alias",
            Self::UnresolvedAlias => "unresolved_alias",
            Self::WildcardImport => "wildcard_import",
            Self::DynamicReceiver => "dynamic_receiver",
            Self::GenericReceiver => "generic_receiver",
            Self::MacroTokenCandidate => "macro_token_candidate",
            Self::MacroDefinitionCandidate => "macro_definition_candidate",
            Self::KnownNamespaceCandidate => "known_namespace_candidate",
            Self::CallableEscape => "callable_escape",
            Self::AuthorityArgument => "authority_argument",
            Self::AuthorityMethod => "authority_method",
            Self::AuthorityStorage => "authority_storage",
            Self::AuthorityReturn => "authority_return",
            Self::NewWrapperCandidate => "new_wrapper_candidate",
            Self::DefinitionMismatch => "definition_mismatch",
        }
    }
}
