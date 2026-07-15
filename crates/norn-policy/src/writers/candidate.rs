//! Stable semantic identities for unresolved writer candidates.

use serde::{Deserialize, Serialize};

use crate::digest::{Digest, digest_bytes};
use crate::finding::ByteSpan;
use crate::path::RepositoryPath;

use super::{UnknownSinkReason, WRITER_ANALYZER_VERSION, WriterToken};

const CANDIDATE_IDENTITY_DOMAIN: &[u8] = b"norn-writer-candidate-1";

/// Closed syntax or authority form that produced an unresolved candidate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterCandidateForm {
    /// A qualified, imported, or otherwise callable function expression.
    FunctionCall,
    /// A method call whose receiver could not be resolved conclusively.
    MethodCall,
    /// A macro invocation path.
    MacroInvocation,
    /// A writer-like token inside a macro invocation.
    MacroToken,
    /// A writer-like token inside a macro definition.
    MacroDefinition,
    /// A writer callable escaped the analyzer's tracked local flow.
    CallableEscape,
    /// Writer authority escaped through an unsupported operation or boundary.
    AuthorityEscape,
    /// A project wrapper definition was new, duplicated, or incompatible.
    WrapperDefinition,
}

impl WriterCandidateForm {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::FunctionCall => "function_call",
            Self::MethodCall => "method_call",
            Self::MacroInvocation => "macro_invocation",
            Self::MacroToken => "macro_token",
            Self::MacroDefinition => "macro_definition",
            Self::CallableEscape => "callable_escape",
            Self::AuthorityEscape => "authority_escape",
            Self::WrapperDefinition => "wrapper_definition",
        }
    }
}

/// Strongly typed stable identity for one unresolved writer candidate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct WriterCandidateId(Digest);

impl WriterCandidateId {
    /// Return the complete candidate identity digest.
    #[must_use]
    pub const fn digest(self) -> Digest {
        self.0
    }
}

/// Semantic fields supplied by the syntax-aware writer analyzer.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WriterCandidateSemantics {
    enclosing_item: Digest,
    normalized_call: Digest,
    candidate: WriterToken,
    reason: UnknownSinkReason,
    form: WriterCandidateForm,
}

impl WriterCandidateSemantics {
    /// Construct one complete candidate semantic description.
    #[must_use]
    pub const fn new(
        enclosing_item: Digest,
        normalized_call: Digest,
        candidate: WriterToken,
        reason: UnknownSinkReason,
        form: WriterCandidateForm,
    ) -> Self {
        Self {
            enclosing_item,
            normalized_call,
            candidate,
            reason,
            form,
        }
    }
}

/// One unresolved writer candidate with stable semantics and diagnostic span.
///
/// The byte span is deliberately excluded from identity. Formatting may move
/// an otherwise identical candidate without invalidating its reviewed semantic
/// disposition.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WriterCandidate {
    id: WriterCandidateId,
    path: RepositoryPath,
    span: ByteSpan,
    enclosing_item: Digest,
    normalized_call: Digest,
    candidate: WriterToken,
    reason: UnknownSinkReason,
    form: WriterCandidateForm,
    ordinal: u32,
}

impl WriterCandidate {
    /// Construct a candidate and derive its complete stable identity.
    #[must_use]
    pub fn new(
        path: RepositoryPath,
        span: ByteSpan,
        semantics: WriterCandidateSemantics,
        ordinal: u32,
    ) -> Self {
        let WriterCandidateSemantics {
            enclosing_item,
            normalized_call,
            candidate,
            reason,
            form,
        } = semantics;
        let id = candidate_id(&CandidateIdentityInput {
            path: &path,
            enclosing_item,
            normalized_call,
            candidate: &candidate,
            reason,
            form,
            ordinal,
        });
        Self {
            id,
            path,
            span,
            enclosing_item,
            normalized_call,
            candidate,
            reason,
            form,
            ordinal,
        }
    }

    /// Return the stable semantic candidate identity.
    #[must_use]
    pub const fn id(&self) -> WriterCandidateId {
        self.id
    }

    /// Return the repository-relative source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the current diagnostic byte span.
    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    /// Return the enclosing-item identity.
    #[must_use]
    pub const fn enclosing_item(&self) -> Digest {
        self.enclosing_item
    }

    /// Return the normalized candidate syntax identity.
    #[must_use]
    pub const fn normalized_call(&self) -> Digest {
        self.normalized_call
    }

    /// Return the normalized candidate token.
    #[must_use]
    pub const fn candidate(&self) -> &WriterToken {
        &self.candidate
    }

    /// Return the closed uncertainty reason.
    #[must_use]
    pub const fn reason(&self) -> UnknownSinkReason {
        self.reason
    }

    /// Return the closed candidate form.
    #[must_use]
    pub const fn form(&self) -> WriterCandidateForm {
        self.form
    }

    /// Return the multiset ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Forge an identity solely to exercise cryptographic-collision defenses.
    #[cfg(test)]
    pub(crate) fn with_forged_id_for_collision_test(&self, id: WriterCandidateId) -> Self {
        let mut forged = self.clone();
        forged.id = id;
        forged
    }

    pub(crate) fn same_semantics(&self, other: &Self) -> bool {
        self.path == other.path
            && self.enclosing_item == other.enclosing_item
            && self.normalized_call == other.normalized_call
            && self.candidate == other.candidate
            && self.reason == other.reason
            && self.form == other.form
            && self.ordinal == other.ordinal
    }
}

struct CandidateIdentityInput<'a> {
    path: &'a RepositoryPath,
    enclosing_item: Digest,
    normalized_call: Digest,
    candidate: &'a WriterToken,
    reason: UnknownSinkReason,
    form: WriterCandidateForm,
    ordinal: u32,
}

fn candidate_id(input: &CandidateIdentityInput<'_>) -> WriterCandidateId {
    let mut framed = Vec::new();
    field(&mut framed, CANDIDATE_IDENTITY_DOMAIN);
    field(&mut framed, WRITER_ANALYZER_VERSION.as_bytes());
    field(&mut framed, input.path.as_str().as_bytes());
    field(&mut framed, input.enclosing_item.as_bytes());
    field(&mut framed, input.normalized_call.as_bytes());
    field(&mut framed, input.candidate.as_str().as_bytes());
    field(&mut framed, input.reason.token().as_bytes());
    field(&mut framed, input.form.token().as_bytes());
    field(&mut framed, &input.ordinal.to_be_bytes());
    WriterCandidateId(digest_bytes(&framed))
}

fn field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
